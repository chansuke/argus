//! Diff hunks and the two hashes that tie them to the rest of argus. This module owns the
//! unified-diff parser and both hashing rules, so the host (git output), the adapters
//! (agent edit inputs), and the review view always agree:
//!
//! - [`line_hash`]: one whitespace-trimmed line. A hunk's added lines are matched against
//!   the lines an agent's edit inserted (hunk provenance, `views::packet`).
//! - [`HunkAnchor`]: path plus a fingerprint of the hunk's removed and added lines, not of
//!   their numbers. A hunk that survives untouched into a later snapshot keeps its anchor
//!   when lines above it shift, which is how "already seen in a decided snapshot" works.

use crate::event::Hunk;
use crate::ids::{CommitOid, LineRange, RepoPath, TreeOid};

/// FNV-1a/64 of `line.trim()`.
pub fn line_hash(line: &str) -> u64 {
    line.trim().bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
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
/// TODO: split on `diff --git`; path from `+++ b/` (`--- a/` for deletions);
/// per `@@ -a,b +c,d @@`: header, text, added hashes, fingerprint =
/// FNV over (`-` lines, a separator, `+` lines), each through `line_hash`.
pub fn parse_unified(text: &str) -> Vec<DiffHunk> {
    todo!("not implemented")
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
