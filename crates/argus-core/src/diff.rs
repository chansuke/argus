//! Diff hunks and the two hashes that tie them to the rest of argus. This module owns the
//! unified-diff parser and both hashing rules, so the host (git output), the adapters
//! (agent edit inputs), and the review view always agree:
//!
//! - [`line_hash`]: one whitespace-trimmed line. A hunk's added lines are matched against
//!   the lines an agent's edit inserted (hunk provenance, `views::packet`).
//! - [`HunkAnchor`]: path plus a fingerprint of the hunk's removed and added lines, not of
//!   their numbers. A hunk that survives untouched into a later snapshot keeps its anchor
//!   when lines above it shift, which is how "already seen in a decided snapshot" works.

use crate::ids::{CommitOid, LineRange, RepoPath, TreeOid};

/// One hunk header of a unified diff: where the change sits in the old and new file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

/// FNV-1a/64 of `line.trim()`.
pub fn line_hash(line: &str) -> u64 {
    line.trim().bytes().fold(FNV_OFFSET, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HunkAnchor {
    pub path: RepoPath,
    pub fingerprint: u64,
}

/// One hunk of `git diff <base> <tree>`, with its text.
#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub anchor: HunkAnchor,
    pub header: Hunk,
    /// Lines of the hunk in the new tree; `None` for a pure deletion.
    pub new_lines: Option<LineRange>,
    pub text: String,
    /// `line_hash` of every `+` line.
    pub added: Vec<u64>,
    pub removed_count: u32,
}

/// `git diff --no-color --no-renames --unified=3 <base> <tree>` output, parsed. Pure.
///
/// The path comes from `+++ b/` (`--- a/` for a deletion). Files without hunks (binary,
/// mode-only) and paths git had to quote are skipped. The fingerprint is FNV-1a/64 over
/// each changed line's `line_hash`, tagged with its side, so it moves with the content
/// and not with the line numbers.
pub fn parse_unified(text: &str) -> Vec<DiffHunk> {
    let mut out = Vec::new();
    let mut path: Option<RepoPath> = None;
    let mut old_path: Option<RepoPath> = None;
    let mut open: Option<Open> = None;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            out.extend(open.take().map(Open::finish));
            (path, old_path) = (None, None);
        } else if let Some(p) = line.strip_prefix("--- ").filter(|_| open.is_none()) {
            old_path = p.strip_prefix("a/").and_then(RepoPath::parse);
        } else if let Some(p) = line.strip_prefix("+++ ").filter(|_| open.is_none()) {
            path = p
                .strip_prefix("b/")
                .and_then(RepoPath::parse)
                .or(old_path.clone());
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            out.extend(open.take().map(Open::finish));
            open = path
                .clone()
                .zip(parse_header(rest))
                .map(|(path, header)| Open {
                    path,
                    header,
                    text: format!("{line}\n"),
                    added: Vec::new(),
                    fingerprint: FNV_OFFSET,
                    removed_count: 0,
                });
        } else if let Some(hunk) = &mut open {
            hunk.text.push_str(line);
            hunk.text.push('\n');
            match line.as_bytes().first() {
                Some(b'+') => {
                    let h = line_hash(&line[1..]);
                    hunk.added.push(h);
                    hunk.feed(b'+', h);
                }
                Some(b'-') => {
                    hunk.removed_count += 1;
                    hunk.feed(b'-', line_hash(&line[1..]));
                }
                _ => {}
            }
        }
    }
    out.extend(open.map(Open::finish));
    out
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

struct Open {
    path: RepoPath,
    header: Hunk,
    text: String,
    added: Vec<u64>,
    fingerprint: u64,
    removed_count: u32,
}

impl Open {
    fn feed(&mut self, side: u8, hash: u64) {
        for b in std::iter::once(side).chain(hash.to_le_bytes()) {
            self.fingerprint = (self.fingerprint ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn finish(self) -> DiffHunk {
        let Hunk {
            new_start, new_len, ..
        } = self.header;
        DiffHunk {
            anchor: HunkAnchor {
                path: self.path,
                fingerprint: self.fingerprint,
            },
            header: self.header,
            new_lines: LineRange::new(new_start, new_start + new_len.saturating_sub(1))
                .filter(|_| new_len > 0),
            text: self.text,
            added: self.added,
            removed_count: self.removed_count,
        }
    }
}

/// `-a[,b] +c[,d] @@ ...`; an omitted count is 1.
fn parse_header(rest: &str) -> Option<Hunk> {
    let mut parts = rest.split(' ');
    let side = |s: &str, sign: char| -> Option<(u32, u32)> {
        let s = s.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = side(parts.next()?, '-')?;
    let (new_start, new_len) = side(parts.next()?, '+')?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

/// What the host must diff for a review packet: always the head snapshot against the
/// job's base. Trees are immutable, so the host may memoize the result by `(base, tree)`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiffRequest {
    pub base: CommitOid,
    pub tree: TreeOid,
}

/// The answer to a [`DiffRequest`], handed to `JobViews::review_packet`.
#[derive(Clone, Debug)]
pub struct HeadDiff {
    pub request: DiffRequest,
    pub hunks: Vec<DiffHunk>,
}
