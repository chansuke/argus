//! Review state of one job, as fold state: the snapshot chain, AI review runs and their
//! findings, human threads (drafts until sent), finding triage, and decisions.
//!
//! The rules that make approval mean "this exact code" live here and only here:
//! - `head()`: the latest snapshot whose tree differs from the one before it. A turn that
//!   changed nothing makes no new head, so it does not revoke an approval.
//! - `approved_head()`: the human's latest decision on the head tree is `Approve`. A new
//!   head therefore revokes approval by construction; nothing has to "demote" it.
//! - `admit`: `Decided` only on the current head; `Merge` only with a current approval.
//!
//! The packet (`views::packet`), the policy, and the glance read these; none re-derives them.

use std::collections::{BTreeMap, BTreeSet};

use crate::diff::DiffRequest;
use crate::event::{
    Anchor, ApprovedHead, Author, Decision, Event, Rejected, ReviewSpec, RunPurpose, Snapshot,
};
use crate::fact::Fact;
use crate::ids::*;
use crate::source::SourceFamily;
use crate::views::packet::FeedbackItem;
use crate::views::{GateState, JobViews, Projection};

#[derive(Default, Debug)]
pub struct ReviewBoard {
    base: Option<CommitOid>,
    snapshots: Vec<SnapshotEntry>,
    /// Snapshot run -> its `RunStarted` stamp, which becomes the entry's `cut`.
    starting: BTreeMap<RunId, Stamp>,
    reviews: BTreeMap<RunId, ReviewRun>,
    threads: BTreeMap<ThreadId, Thread>,
    dismissed: BTreeSet<FindingId>,
    decisions: Vec<DecisionEntry>,
}

#[derive(Debug)]
pub struct SnapshotEntry {
    pub run: RunId,
    pub at: UnixMillis,
    /// Edits observed at or before this stamp are in the tree. The revision window of a
    /// snapshot is `(previous.cut, cut]` (`Transcript::edits_between`). An edit racing the
    /// snapshot lands in the next window; provenance searches every window up to head.
    pub cut: Stamp,
    pub snapshot: Snapshot,
}

#[derive(Debug)]
pub struct ReviewRun {
    pub spec: ReviewSpec,
    pub output: ReviewOutput,
    /// The reviewer's own transcript is a normal source: its raw log is one click away.
    pub sources: Vec<SourceId>,
}

#[derive(Debug)]
pub enum ReviewOutput {
    Running,
    Findings(Vec<Finding>),
    /// The model ignored the output contract: its whole answer is shown, never dropped.
    Unstructured {
        text: String,
        fact: FactId,
    },
    Failed {
        reason: String,
    },
}

/// What `adapters::llm::parse_findings` yields before ids are assigned.
#[derive(Clone, Debug)]
pub struct RawFinding {
    pub path: Option<RepoPath>,
    pub lines: Option<LineRange>,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    High,
    Medium,
    Low,
    Nit,
}

#[derive(Debug)]
pub struct Finding {
    pub id: FindingId,
    /// `Code` anchors name the reviewed tree. `None`: no usable location.
    pub anchor: Option<Anchor>,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct Thread {
    pub anchor: Anchor,
    pub origin: ThreadOrigin,
    pub comments: Vec<Comment>,
    pub resolved: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ThreadOrigin {
    Human,
    /// The human promoted this AI finding. The only way a finding reaches the agent.
    Promoted(FindingId),
}

#[derive(Debug)]
pub struct Comment {
    pub id: CommentId,
    pub at: UnixMillis,
    pub body: String,
    /// `None`: a draft. Set by the `RequestChanges` decision that sent it.
    pub sent_by: Option<DecisionId>,
}

#[derive(Debug)]
pub struct DecisionEntry {
    pub id: DecisionId,
    pub fact: FactId,
    pub at: UnixMillis,
    pub tree: Option<TreeOid>,
    pub verdict: Decision,
    pub note: String,
}

impl ReviewBoard {
    pub fn head(&self) -> Option<&SnapshotEntry> {
        self.snapshots.last()
    }
    pub fn snapshots(&self) -> &[SnapshotEntry] {
        &self.snapshots
    }
    /// What the host must diff (outside the sequencer) to build the review packet.
    pub fn diff_request(&self) -> Option<DiffRequest> {
        let head = self.head()?;
        Some(DiffRequest {
            base: head.snapshot.base.clone(),
            tree: head.snapshot.tree.clone(),
        })
    }
    pub fn approved_head(&self) -> Option<ApprovedHead> {
        let head = self.head()?;
        let latest = self
            .decisions
            .iter()
            .rev()
            .find(|d| d.tree.as_ref() == Some(&head.snapshot.tree))?;
        (latest.verdict == Decision::Approve)
            .then(|| ApprovedHead::mint(head.snapshot.tree.clone(), head.snapshot.base.clone()))
    }
    pub fn decisions(&self) -> &[DecisionEntry] {
        &self.decisions
    }
    pub fn reviews(&self) -> impl Iterator<Item = (&RunId, &ReviewRun)> {
        self.reviews.iter()
    }
    pub fn threads(&self) -> impl Iterator<Item = (&ThreadId, &Thread)> {
        self.threads.iter()
    }
    pub fn is_dismissed(&self, finding: &FindingId) -> bool {
        self.dismissed.contains(finding)
    }
    /// What `RequestChanges` would send now (`decision = None`, the preview), or what
    /// decision `d` did send (delivery). One function, so preview and delivery agree.
    pub fn feedback(&self, decision: Option<DecisionId>) -> Vec<FeedbackItem<'_>> {
        // TODO: every comment with `sent_by == decision`, in thread order; `at` = the
        //       thread's code anchor re-anchored to head (`reanchor`), `None` if outdated.
        todo!("not implemented")
    }
    /// Green iff a review of `head` finished and no High/Medium finding or sent thread is
    /// open on it.
    pub fn gate(&self, head: Option<&TreeOid>) -> GateState {
        todo!("not implemented")
    }

    /// Maps `lines` in tree `from` to tree `to` through the snapshot chain's `delta` hunk
    /// headers (pure; no git call). `None` when an edit overlapped the range. Used for
    /// human threads only: machine findings carry forward only on identical files.
    pub(crate) fn reanchor(
        &self,
        from: &TreeOid,
        to: &TreeOid,
        path: &RepoPath,
        lines: LineRange,
    ) -> Option<LineRange> {
        todo!("not implemented")
    }
}

/// `ViewSet::admit` for job ledgers (see the module doc). Reads the field directly, so an
/// admission check is not recorded as a subscription read.
pub(crate) fn admit(views: &JobViews, _author: &Author, event: &Event) -> Result<(), Rejected> {
    let board = &views.review;
    match event {
        Event::Decided { tree, verdict, .. } => {
            let head = board.head().map(|h| &h.snapshot.tree);
            let tree_required = *verdict != Decision::Abandon;
            if head == tree.as_ref() && (tree.is_some() || !tree_required) {
                Ok(())
            } else {
                Err(Rejected::NotHead {
                    head: head.cloned(),
                })
            }
        }
        Event::RunRequested {
            purpose: RunPurpose::Merge(token),
            ..
        } => {
            if board.approved_head().as_ref() == Some(token) {
                Ok(())
            } else {
                Err(Rejected::NotApproved)
            }
        }
        _ => Ok(()),
    }
}

impl Projection for ReviewBoard {
    const NAME: &'static str = "review";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentHeadless];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO:
        // - JobOpened -> base. RunRequested(Snapshot) + RunStarted -> starting[run] = stamp;
        //   RunFinished(Snapshot(s)) -> push SnapshotEntry unless s.tree == head's tree.
        // - RunRequested(Review) -> reviews[run] = Running. `Result` observed from a review
        //   run's source -> parse_findings -> Findings (ids = result fact + index, code
        //   anchors on spec.tree) or, when None, Unstructured { text, fact }.
        //   RunFinished without a result -> Failed.
        // - ThreadOpened / Replied -> draft comments. FindingPromoted -> a thread with
        //   origin Promoted, anchor = the finding's anchor (or Fact(finding fact)), body =
        //   title + body. ThreadResolved, FindingDismissed -> assignments.
        // - Decided -> push; RequestChanges sets `sent_by` on every draft comment.
        todo!("not implemented")
    }
}
