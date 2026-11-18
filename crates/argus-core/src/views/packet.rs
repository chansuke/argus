//! The review packet: what the human sees when opening a job's review, ordered to spend
//! attention where it matters. Composed at read time from the views plus the head diff
//! (which the host computes outside the sequencer); never stored. Everything here is pure.
//!
//! First screen, top to bottom:
//! 1. [`Headline`]: readable in five seconds.
//! 2. Agenda: one [`AgendaItem`] per head hunk, sorted by [`Tier`], then churn. A hunk seen
//!    in a decided snapshot and unchanged since is `Collapsed`, so the second round is an
//!    interdiff for free.
//! 3. Findings and threads that no hunk holds (unlocated, or outside the diff).

use std::collections::BTreeSet;

use crate::diff::{DiffHunk, HeadDiff, HunkAnchor};
use crate::event::{Anchor, ApprovedHead, Decision};
use crate::ids::*;
use crate::models::ModelChoice;
use crate::views::checks::CheckVerdict;
use crate::views::review::{
    Finding, ReviewBoard, ReviewOutput, Severity, SnapshotEntry, Thread, ThreadOrigin,
};
use crate::views::transcript::{EditHow, EditTrace};
use crate::views::{GateState, JobViews};

/// Path globs whose hunks are `Suspicious` by themselves (specs, CI, lockfiles, Nix).
/// Config may add more.
pub const SENSITIVE_DEFAULT: &[&str] = &[
    "*.tla",
    "*.cfg",
    ".github/**",
    "*.lock",
    "flake.nix",
    "*.lean",
];

pub struct ReviewPacket<'a> {
    pub head: &'a SnapshotEntry,
    /// 1 + the number of `RequestChanges` decisions so far.
    pub round: u32,
    pub headline: Headline,
    pub agenda: Vec<AgendaItem<'a>>,
    pub unattached: Vec<FindingRef<'a>>,
    pub loose_threads: Vec<ThreadRef<'a>>,
    /// The edits that produced this snapshot: the raw-log entry point for "what did the
    /// agent do this round".
    pub window: &'a [EditTrace],
    /// Present iff `merge` would be admitted now.
    pub approved: Option<ApprovedHead>,
}

#[derive(Clone, Debug)]
pub struct Headline {
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    pub checks: GateState,
    pub ai: Vec<AiTally>,
    /// Reviews of head still running (spinners, not "no findings").
    pub pending_reviews: u32,
    pub unaddressed: u32,
    pub unexplained: u32,
    pub withheld: u32,
}

#[derive(Clone, Debug)]
pub struct AiTally {
    pub model: ModelChoice,
    /// High, Medium, Low, Nit.
    pub by_severity: [u32; 4],
    /// The model ignored the output contract; its answer is shown whole.
    pub unstructured: bool,
}

/// Declaration order is priority order (derived `Ord`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    Blocking,
    Suspicious,
    Normal,
    Collapsed,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    /// A check message located in this hunk (a Lean error, a failing device step).
    CheckFailed {
        run: RunId,
    },
    Finding {
        severity: Severity,
        agreeing_models: u16,
    },
    /// A sent thread whose lines are unchanged at head: the agent did not address it.
    Unaddressed(ThreadId),
    /// No edit in the transcript explains this hunk (generated, formatted, or done
    /// outside the agent).
    Unexplained,
    SensitivePath,
}

/// The one tiering rule. `seen` = the hunk's anchor appeared in a decided snapshot.
/// Collapsed means "seen, unchanged, and nothing new about it": only properties of the
/// hunk itself, which the human already saw, may remain.
pub fn tier(signals: &[Signal], seen: bool) -> Tier {
    if seen
        && signals
            .iter()
            .all(|s| matches!(s, Signal::Unexplained | Signal::SensitivePath))
    {
        return Tier::Collapsed;
    }
    let of = |s: &Signal| match s {
        Signal::CheckFailed { .. } | Signal::Unaddressed(_) => Tier::Blocking,
        Signal::Finding {
            severity: Severity::High,
            agreeing_models,
        } if *agreeing_models >= 2 => Tier::Blocking,
        Signal::Finding {
            severity: Severity::High,
            ..
        }
        | Signal::Unexplained
        | Signal::SensitivePath => Tier::Suspicious,
        Signal::Finding { .. } => Tier::Normal,
    };
    signals.iter().map(of).min().unwrap_or(Tier::Normal)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Applicability {
    Current,
    /// From an older snapshot; every file it can depend on is byte-identical at head
    /// (`Snapshot::files` blob ids), so it still holds. Otherwise it is not shown: rerun.
    CarriedForward {
        from: TreeOid,
    },
}

/// Why a hunk exists, in the agent's own raw words.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Provenance {
    /// Structured edits explain `coverage_permille` of the added lines.
    Direct {
        facts: Vec<FactId>,
        coverage_permille: u16,
    },
    /// Only shell commands named the path.
    ViaShell {
        facts: Vec<FactId>,
    },
    Unexplained,
}

pub const MIN_COVERAGE_PERMILLE: u16 = 600;

/// Pure. `edits` are every edit up to head's cut, in stamp order.
///
/// Candidates are the edits of the hunk's file. Structured candidates that inserted at
/// least one of the hunk's added lines explain it; coverage is the share of added lines
/// some candidate inserted.
/// - coverage >= [`MIN_COVERAGE_PERMILLE`] -> `Direct`.
/// - else a shell command named the file -> `ViaShell`.
/// - else partial coverage -> `Direct` with that coverage (shown as weak).
/// - else `Unexplained`.
///
/// A pure deletion has no added lines: any structured edit of the file explains it, then
/// any shell command.
pub fn provenance(hunk: &DiffHunk, edits: &[EditTrace]) -> Provenance {
    let on_path = || edits.iter().filter(|e| e.path == hunk.anchor.path);
    let shell: Vec<FactId> = on_path()
        .filter(|e| matches!(e.how, EditHow::Shell))
        .map(|e| e.fact.clone())
        .collect();
    let structured = on_path().filter_map(|e| match &e.how {
        EditHow::Structured { inserted } => Some((e, inserted)),
        EditHow::Shell => None,
    });
    let (facts, coverage_permille) = if hunk.added.is_empty() {
        (
            structured.map(|(e, _)| e.fact.clone()).collect::<Vec<_>>(),
            1000,
        )
    } else {
        let wanted: BTreeSet<u64> = hunk.added.iter().copied().collect();
        let mut covered = BTreeSet::new();
        let mut facts = Vec::new();
        for (e, inserted) in structured {
            let hits: Vec<u64> = inserted
                .iter()
                .copied()
                .filter(|h| wanted.contains(h))
                .collect();
            if !hits.is_empty() {
                covered.extend(hits);
                facts.push(e.fact.clone());
            }
        }
        let lines = hunk.added.iter().filter(|h| covered.contains(h)).count();
        (facts, (lines * 1000 / hunk.added.len()) as u16)
    };
    if !facts.is_empty() && coverage_permille >= MIN_COVERAGE_PERMILLE {
        Provenance::Direct {
            facts,
            coverage_permille,
        }
    } else if !shell.is_empty() {
        Provenance::ViaShell { facts: shell }
    } else if !facts.is_empty() {
        Provenance::Direct {
            facts,
            coverage_permille,
        }
    } else {
        Provenance::Unexplained
    }
}

pub struct FindingRef<'a> {
    pub finding: &'a Finding,
    pub run: &'a RunId,
    pub model: &'a ModelChoice,
    pub applicability: Applicability,
    /// Distinct models whose findings overlap this one's location (itself included).
    pub agreeing_models: u16,
    pub dismissed: bool,
    pub promoted: bool,
}

pub struct ThreadRef<'a> {
    pub id: &'a ThreadId,
    pub thread: &'a Thread,
    /// Re-anchored to head; `None` when outdated (its lines were rewritten).
    pub at: Option<LineRange>,
    pub unaddressed: bool,
}

pub struct AgendaItem<'a> {
    pub hunk: &'a DiffHunk,
    pub tier: Tier,
    pub signals: Vec<Signal>,
    pub provenance: Provenance,
    pub findings: Vec<FindingRef<'a>>,
    pub threads: Vec<ThreadRef<'a>>,
}

/// The diff no longer matches the head: a newer snapshot arrived after the host read
/// `diff_request`. The host diffs again (memoized) instead of showing a stale packet.
#[derive(Debug)]
pub struct HeadMoved;

/// Anchors of every hunk of every snapshot that received a decision (any verdict).
pub fn seen_anchors(board: &ReviewBoard) -> BTreeSet<&HunkAnchor> {
    let decided: BTreeSet<&TreeOid> = board
        .decisions()
        .iter()
        .filter_map(|d| d.tree.as_ref())
        .collect();
    board
        .snapshots()
        .iter()
        .filter(|s| decided.contains(&s.snapshot.tree))
        .flat_map(|s| &s.snapshot.files)
        .flat_map(|f| &f.hunks)
        .collect()
}

/// One comment as it goes to the agent.
#[derive(Clone, Debug)]
pub struct FeedbackItem<'a> {
    pub at: Option<(&'a RepoPath, LineRange)>,
    pub body: &'a str,
    pub from_finding: bool,
}

/// The exact text an agent receives for a `RequestChanges` decision: the note, then each
/// comment with `path:start-end` (the agent has the files, so nothing is quoted). Pure, so
/// `preview_feedback` shows the human byte-for-byte what delivery will send.
///
/// ```text
/// <note>
///
/// - src/a.rs:10-12: first line of the comment
///   further lines, indented
/// - [AI review] src/b.rs:3: a promoted finding
/// - a comment with no code location
/// ```
pub fn render_feedback(note: &str, items: &[FeedbackItem<'_>]) -> String {
    let mut out = String::new();
    let note = note.trim();
    if !note.is_empty() {
        out.push_str(note);
        out.push('\n');
        if !items.is_empty() {
            out.push('\n');
        }
    }
    for item in items {
        out.push_str("- ");
        if item.from_finding {
            out.push_str("[AI review] ");
        }
        if let Some((path, lines)) = item.at {
            out.push_str(path.as_str());
            match lines.start() == lines.end() {
                true => out.push_str(&format!(":{}: ", lines.start())),
                false => out.push_str(&format!(":{}-{}: ", lines.start(), lines.end())),
            }
        }
        let mut body = item.body.trim().lines();
        out.push_str(body.next().unwrap_or_default());
        out.push('\n');
        for line in body {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Whether `path` matches `pattern`. A pattern without `/` matches the file name (`*.tla`
/// anywhere); one with `/` matches the whole path. `*` stops at `/`, `**` does not.
fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| go(rest, &s[i..])),
            [b'*', rest @ ..] => (0..=s.len())
                .take_while(|i| *i == 0 || s[i - 1] != b'/')
                .any(|i| go(rest, &s[i..])),
            [c, rest @ ..] => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    let subject = match pattern.contains('/') {
        true => path,
        false => path.rsplit('/').next().unwrap_or(path),
    };
    go(pattern.as_bytes(), subject.as_bytes())
}

/// Read-time configuration of the packet.
#[derive(Clone, Debug)]
pub struct PacketConfig {
    /// `SENSITIVE_DEFAULT` plus the user's additions.
    pub sensitive: Vec<String>,
}

impl JobViews {
    /// Builds the packet for `diff`, which the host computed for
    /// `review().diff_request()` without holding the sequencer.
    ///
    /// - A diff of another tree than the head is refused (`HeadMoved`).
    /// - Findings: every finding of a review of head (`Current`), and every located
    ///   finding of an older review whose file is byte-identical at head, on the same base
    ///   (`CarriedForward`). Others are not shown: rerun the review. Findings on overlapping
    ///   lines of one file form a cluster; `agreeing_models` counts its distinct models.
    /// - Threads are re-anchored to head. A sent, unresolved thread from an older tree
    ///   whose lines survived unchanged is `Unaddressed`.
    /// - A finding, thread, or failing check message attaches to the first hunk of its file
    ///   whose new lines overlap it; the rest are `unattached` / `loose_threads`.
    /// - The agenda is sorted by tier, then by churn (added + removed), largest first.
    pub fn review_packet<'a>(
        &'a self,
        diff: &'a HeadDiff,
        cfg: &PacketConfig,
    ) -> Result<ReviewPacket<'a>, HeadMoved> {
        let review = self.review();
        let head = review.head().ok_or(HeadMoved)?;
        let head_tree = &head.snapshot.tree;
        if &diff.request.tree != head_tree {
            return Err(HeadMoved);
        }
        let transcript = self.transcript();
        let edits = transcript.edits_between(None, head.cut);
        let previous_cut = review.snapshots().iter().rev().nth(1).map(|s| s.cut);
        let window = transcript.edits_between(previous_cut, head.cut);
        let seen = seen_anchors(review);
        let round = 1 + review
            .decisions()
            .iter()
            .filter(|d| d.verdict == Decision::RequestChanges)
            .count() as u32;

        let mut findings = self.packet_findings(head);
        let mut threads = Vec::new();
        for (id, thread) in review.threads() {
            let (at, path) = match &thread.anchor {
                Anchor::Code { tree, path, lines } => (
                    review.reanchor(tree, head_tree, path, *lines),
                    Some((path, tree)),
                ),
                _ => (None, None),
            };
            let sent = thread.comments.iter().any(|c| c.sent_by.is_some());
            let older = path.is_some_and(|(_, tree)| tree != head_tree);
            let unaddressed = sent && !thread.resolved && older && at.is_some();
            let location = path.zip(at).map(|((p, _), l)| (p, l));
            threads.push((
                ThreadRef {
                    id,
                    thread,
                    at,
                    unaddressed,
                },
                location,
            ));
        }
        let failing_checks: Vec<(&RunId, Vec<(RepoPath, LineRange)>)> = self
            .checks()
            .latest_runs()
            .filter(|(_, c)| &c.request.tree == head_tree)
            .filter(|(_, c)| {
                !matches!(
                    c.verdict,
                    None | Some((_, CheckVerdict::Proved | CheckVerdict::Passed))
                )
            })
            .map(|(run, _)| (run, self.checks().located(run)))
            .collect();

        let overlaps = |hunk: &DiffHunk, path: &RepoPath, lines: LineRange| {
            &hunk.anchor.path == path
                && hunk
                    .new_lines
                    .is_some_and(|h| h.start() <= lines.end() && lines.start() <= h.end())
        };
        let mut agenda = Vec::new();
        for hunk in &diff.hunks {
            let mut signals = Vec::new();
            for (run, located) in &failing_checks {
                if located.iter().any(|(p, l)| overlaps(hunk, p, *l)) {
                    signals.push(Signal::CheckFailed { run: **run });
                }
            }
            let mut mine = Vec::new();
            let mut i = 0;
            while i < findings.len() {
                match findings[i].1 {
                    Some((p, l)) if overlaps(hunk, p, l) => mine.push(findings.remove(i).0),
                    _ => i += 1,
                }
            }
            for f in mine.iter().filter(|f| !f.dismissed && !f.promoted) {
                signals.push(Signal::Finding {
                    severity: f.finding.severity,
                    agreeing_models: f.agreeing_models,
                });
            }
            let mut held = Vec::new();
            let mut i = 0;
            while i < threads.len() {
                match threads[i].1 {
                    Some((p, l)) if overlaps(hunk, p, l) => held.push(threads.remove(i).0),
                    _ => i += 1,
                }
            }
            signals.extend(
                held.iter()
                    .filter(|t| t.unaddressed)
                    .map(|t| Signal::Unaddressed(*t.id)),
            );
            let provenance = provenance(hunk, edits);
            if provenance == Provenance::Unexplained {
                signals.push(Signal::Unexplained);
            }
            if cfg
                .sensitive
                .iter()
                .any(|g| glob(g, hunk.anchor.path.as_str()))
            {
                signals.push(Signal::SensitivePath);
            }
            agenda.push(AgendaItem {
                hunk,
                tier: tier(&signals, seen.contains(&hunk.anchor)),
                signals,
                provenance,
                findings: mine,
                threads: held,
            });
        }
        agenda.sort_by_key(|a| {
            (
                a.tier,
                std::cmp::Reverse(a.hunk.added.len() as u32 + a.hunk.removed_count),
            )
        });

        let mut ai = Vec::new();
        let mut pending_reviews = 0;
        for (_, r) in review.reviews().filter(|(_, r)| &r.spec.tree == head_tree) {
            let mut tally = AiTally {
                model: r.spec.llm.model.clone(),
                by_severity: [0; 4],
                unstructured: false,
            };
            match &r.output {
                ReviewOutput::Running => {
                    pending_reviews += 1;
                    continue;
                }
                ReviewOutput::Failed { .. } => continue,
                ReviewOutput::Unstructured { .. } => tally.unstructured = true,
                ReviewOutput::Findings(fs) => {
                    for f in fs {
                        tally.by_severity[f.severity as usize] += 1;
                    }
                }
            }
            ai.push(tally);
        }
        let mut paths: Vec<&RepoPath> = diff.hunks.iter().map(|h| &h.anchor.path).collect();
        paths.dedup();
        let loose_threads: Vec<ThreadRef<'a>> = threads.into_iter().map(|(t, _)| t).collect();
        let unaddressed = agenda
            .iter()
            .flat_map(|a| &a.threads)
            .chain(&loose_threads)
            .filter(|t| t.unaddressed)
            .count() as u32;
        let headline = Headline {
            files: paths.len() as u32,
            added: diff.hunks.iter().map(|h| h.added.len() as u32).sum(),
            removed: diff.hunks.iter().map(|h| h.removed_count).sum(),
            checks: self.checks().gate(Some(head_tree)),
            ai,
            pending_reviews,
            unaddressed,
            unexplained: agenda
                .iter()
                .filter(|a| a.provenance == Provenance::Unexplained)
                .count() as u32,
            withheld: head.snapshot.withheld.len() as u32,
        };
        Ok(ReviewPacket {
            head,
            round,
            headline,
            agenda,
            unattached: findings.into_iter().map(|(f, _)| f).collect(),
            loose_threads,
            window,
            approved: review.approved_head(),
        })
    }

    /// Every finding the packet shows, with its location at head, and how many models
    /// agree on its spot.
    fn packet_findings<'a>(
        &'a self,
        head: &'a SnapshotEntry,
    ) -> Vec<(FindingRef<'a>, Option<(&'a RepoPath, LineRange)>)> {
        let review = self.review();
        let snapshot_of =
            |tree: &TreeOid| review.snapshots().iter().find(|s| &s.snapshot.tree == tree);
        // `None`: the file is as in the base. `Some(None)`: deleted.
        let blob_at = |s: &'a SnapshotEntry, path: &RepoPath| {
            s.snapshot
                .files
                .iter()
                .find(|f| &f.path == path)
                .map(|f| f.blob.as_ref())
        };
        let promoted: BTreeSet<&FindingId> = review
            .threads()
            .filter_map(|(_, t)| match &t.origin {
                ThreadOrigin::Promoted(f) => Some(f),
                ThreadOrigin::Human => None,
            })
            .collect();
        let mut out = Vec::new();
        for (run, r) in review.reviews() {
            let ReviewOutput::Findings(found) = &r.output else {
                continue;
            };
            for finding in found {
                let location = match &finding.anchor {
                    Some(Anchor::Code { path, lines, .. }) => Some((path, *lines)),
                    _ => None,
                };
                let applicability = if r.spec.tree == head.snapshot.tree {
                    Applicability::Current
                } else {
                    let Some((path, _)) = location else {
                        continue;
                    };
                    let Some(old) = snapshot_of(&r.spec.tree) else {
                        continue;
                    };
                    if old.snapshot.base != head.snapshot.base
                        || blob_at(old, path) != blob_at(head, path)
                    {
                        continue;
                    }
                    Applicability::CarriedForward {
                        from: r.spec.tree.clone(),
                    }
                };
                let finding_ref = FindingRef {
                    finding,
                    run,
                    model: &r.spec.llm.model,
                    applicability,
                    agreeing_models: 1,
                    dismissed: review.is_dismissed(&finding.id),
                    promoted: promoted.contains(&finding.id),
                };
                out.push((finding_ref, location));
            }
        }
        // Clusters: findings on overlapping lines of one file, transitively.
        let n = out.len();
        let mut cluster: Vec<usize> = (0..n).collect();
        fn root(c: &mut [usize], mut i: usize) -> usize {
            while c[i] != i {
                c[i] = c[c[i]];
                i = c[i];
            }
            i
        }
        for i in 0..n {
            for j in i + 1..n {
                if let (Some((pa, la)), Some((pb, lb))) = (out[i].1, out[j].1)
                    && pa == pb
                    && la.start() <= lb.end()
                    && lb.start() <= la.end()
                {
                    let (a, b) = (root(&mut cluster, i), root(&mut cluster, j));
                    cluster[a] = b;
                }
            }
        }
        let roots: Vec<usize> = (0..n).map(|i| root(&mut cluster, i)).collect();
        for i in 0..n {
            if out[i].1.is_none() {
                continue;
            }
            let models: BTreeSet<&ModelChoice> = (0..n)
                .filter(|j| roots[*j] == roots[i])
                .map(|j| out[j].0.model)
                .collect();
            out[i].0.agreeing_models = models.len() as u16;
        }
        out
    }

    /// The `preview_feedback` query: exactly what a `RequestChanges` now would deliver.
    pub fn preview_feedback(&self, note: &str) -> String {
        render_feedback(note, &self.review().feedback(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_globs_match_file_names_or_whole_paths() {
        assert!(glob("*.tla", "specs/tla/Lease.tla"));
        assert!(glob("flake.nix", "flake.nix"));
        assert!(glob("flake.nix", "sub/flake.nix"));
        assert!(!glob("flake.nix", "flake.nix.bak"));
        assert!(glob(".github/**", ".github/workflows/ci.yaml"));
        assert!(!glob(".github/**", "docs/.github/x"));
        assert!(glob("crates/*/Cargo.toml", "crates/argus-core/Cargo.toml"));
        assert!(!glob("crates/*/Cargo.toml", "crates/a/b/Cargo.toml"));
        assert!(glob("*.lock", "Cargo.lock"));
        assert!(!glob("*.lean", "proofs/lakefile.toml"));
    }
}
