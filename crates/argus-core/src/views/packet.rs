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
use crate::event::ApprovedHead;
use crate::ids::*;
use crate::models::ModelChoice;
use crate::views::review::{Finding, ReviewBoard, Severity, SnapshotEntry, Thread};
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
    /// TODO:
    /// - head = review.head(); diff.request.tree != head tree -> Err(HeadMoved).
    /// - seen = seen_anchors; edits = transcript.edits_between(None, head.cut);
    ///   window = edits_between(previous cut, head.cut).
    /// - findings of every review whose tree is head (Current) or whose files are
    ///   identical at head (CarriedForward); cluster by path + overlapping lines ->
    ///   agreeing_models = distinct models per cluster.
    /// - threads re-anchored to head; a sent thread whose lines survive unchanged ->
    ///   Unaddressed. Check messages located in a hunk -> CheckFailed.
    /// - per hunk: signals, provenance, tier(signals, seen.contains(anchor)); sort by
    ///   (tier, -(added + removed)); the rest -> unattached / loose_threads.
    /// - approved = review.approved_head().
    pub fn review_packet<'a>(
        &'a self,
        diff: &'a HeadDiff,
        cfg: &PacketConfig,
    ) -> Result<ReviewPacket<'a>, HeadMoved> {
        todo!("not implemented")
    }

    /// The `preview_feedback` query: exactly what a `RequestChanges` now would deliver.
    pub fn preview_feedback(&self, note: &str) -> String {
        render_feedback(note, &self.review().feedback(None))
    }
}
