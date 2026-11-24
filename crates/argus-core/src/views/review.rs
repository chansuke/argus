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

use crate::adapters::llm;
use crate::diff::DiffRequest;
use crate::event::{
    Anchor, ApprovedHead, Author, Decision, Event, Rejected, ReviewSpec, RunOutcome, RunPurpose,
    Snapshot,
};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{AgentWhat, Observation, SourceFamily, SourceMeta};
use crate::views::packet::FeedbackItem;
use crate::views::{GateState, JobViews, Projection};

#[derive(Default, Debug)]
pub struct ReviewBoard {
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
    ///
    /// Threads in the order they were opened, comments in order. A resolved thread's
    /// drafts are not sent. `at` is the thread's code anchor carried to the head, `None`
    /// when an edit touched those lines or the anchor is not code.
    pub fn feedback(&self, decision: Option<DecisionId>) -> Vec<FeedbackItem<'_>> {
        let head = self.head().map(|h| &h.snapshot.tree);
        let mut threads: Vec<&Thread> = self.threads.values().collect();
        threads.sort_by_key(|t| t.comments.first().map(|c| c.at));
        let mut out = Vec::new();
        for thread in threads {
            if decision.is_none() && thread.resolved {
                continue;
            }
            let at = match (&thread.anchor, head) {
                (Anchor::Code { tree, path, lines }, Some(head)) => {
                    self.reanchor(tree, head, path, *lines).map(|l| (path, l))
                }
                _ => None,
            };
            out.extend(
                thread
                    .comments
                    .iter()
                    .filter(|c| c.sent_by == decision)
                    .map(|c| FeedbackItem {
                        at,
                        body: &c.body,
                        from_finding: matches!(thread.origin, ThreadOrigin::Promoted(_)),
                    }),
            );
        }
        out
    }

    /// Green iff a review of `head` finished and no High/Medium finding or sent thread is
    /// open on it.
    ///
    /// - `NotConfigured`: no head, or no review was ever requested.
    /// - `Stale`: reviews exist, none of them of `head`.
    /// - `Pending`: a review of `head` is running and none has finished.
    /// - `Red { open }`: open High/Medium findings (not dismissed, not promoted: a promoted
    ///   finding counts once, as its thread) plus sent unresolved threads, plus one per
    ///   unstructured answer, which only a human can judge. `open: 0` means every review of
    ///   `head` failed.
    pub fn gate(&self, head: Option<&TreeOid>) -> GateState {
        let Some(head) = head else {
            return GateState::NotConfigured;
        };
        if self.reviews.is_empty() {
            return GateState::NotConfigured;
        }
        let of_head: Vec<&ReviewRun> = self
            .reviews
            .values()
            .filter(|r| &r.spec.tree == head)
            .collect();
        if of_head.is_empty() {
            return GateState::Stale;
        }
        let promoted: BTreeSet<&FindingId> = self
            .threads
            .values()
            .filter_map(|t| match &t.origin {
                ThreadOrigin::Promoted(f) => Some(f),
                ThreadOrigin::Human => None,
            })
            .collect();
        let mut finished = false;
        let mut open = 0u32;
        for review in &of_head {
            match &review.output {
                ReviewOutput::Running | ReviewOutput::Failed { .. } => {}
                ReviewOutput::Unstructured { .. } => {
                    finished = true;
                    open += 1;
                }
                ReviewOutput::Findings(findings) => {
                    finished = true;
                    let blocking = findings.iter().filter(|f| {
                        f.severity <= Severity::Medium
                            && !self.dismissed.contains(&f.id)
                            && !promoted.contains(&f.id)
                    });
                    open += blocking.count() as u32;
                }
            }
        }
        if !finished {
            return if of_head
                .iter()
                .any(|r| matches!(r.output, ReviewOutput::Running))
            {
                GateState::Pending
            } else {
                GateState::Red { open: 0 }
            };
        }
        let sent_open = self
            .threads
            .values()
            .filter(|t| !t.resolved && t.comments.iter().any(|c| c.sent_by.is_some()));
        open += sent_open.count() as u32;
        if open == 0 {
            GateState::Green
        } else {
            GateState::Red { open }
        }
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
        if from == to {
            return Some(lines);
        }
        let start = self
            .snapshots
            .iter()
            .position(|s| &s.snapshot.tree == from)?;
        let mut range = (i64::from(lines.start()), i64::from(lines.end()));
        for entry in &self.snapshots[start + 1..] {
            let delta = &entry.snapshot.delta;
            // A rename would need the new path in the answer; the thread is outdated.
            if delta.iter().any(|d| d.renamed_from.as_ref() == Some(path)) {
                return None;
            }
            if let Some(file) = delta.iter().find(|d| &d.path == path) {
                range = shift(range, &file.hunks)?;
            }
            if &entry.snapshot.tree == to {
                let start = u32::try_from(range.0).ok()?;
                let end = u32::try_from(range.1).ok()?;
                return LineRange::new(start, end);
            }
        }
        None
    }

    fn observed(&mut self, fact: &Fact<'_>, source: &SourceMeta, obs: &Observation) -> bool {
        let Some(review) = self.reviews.get_mut(&source.run) else {
            return false;
        };
        let mut changed = false;
        if !review.sources.contains(&source.id) {
            review.sources.push(source.id);
            changed = true;
        }
        let Observation::Agent(event) = obs else {
            return changed;
        };
        let AgentWhat::Result { text, is_error } = &event.what else {
            return changed;
        };
        review.output = if *is_error {
            ReviewOutput::Failed {
                reason: text.clone(),
            }
        } else {
            match llm::parse_findings(text) {
                Some(raw) => ReviewOutput::Findings(
                    raw.into_iter()
                        .enumerate()
                        .map(|(i, f)| Finding {
                            id: FindingId {
                                fact: fact.id.clone(),
                                index: i as u16,
                            },
                            anchor: f.path.zip(f.lines).map(|(path, lines)| Anchor::Code {
                                tree: review.spec.tree.clone(),
                                path,
                                lines,
                            }),
                            severity: f.severity,
                            title: f.title,
                            body: f.body,
                        })
                        .collect(),
                ),
                None => ReviewOutput::Unstructured {
                    text: text.clone(),
                    fact: fact.id.clone(),
                },
            }
        };
        true
    }

    fn finding(&self, id: &FindingId) -> Option<&Finding> {
        self.reviews.values().find_map(|r| match &r.output {
            ReviewOutput::Findings(fs) => fs.iter().find(|f| &f.id == id),
            _ => None,
        })
    }

    fn authored(&mut self, fact: &Fact<'_>, event: &Event) -> bool {
        let at = fact.at;
        let comment = |id: CommentId, body: &str| Comment {
            id,
            at,
            body: body.to_owned(),
            sent_by: None,
        };
        match event {
            Event::RunRequested { run, purpose } => match purpose {
                RunPurpose::Snapshot { .. } => {
                    self.starting.insert(*run, fact.stamp);
                    false
                }
                RunPurpose::Review(spec) => {
                    let review = ReviewRun {
                        spec: spec.clone(),
                        output: ReviewOutput::Running,
                        sources: Vec::new(),
                    };
                    self.reviews.entry(*run).or_insert(review);
                    true
                }
                _ => false,
            },
            Event::RunStarted { run, .. } => match self.starting.get_mut(run) {
                Some(cut) => {
                    *cut = fact.stamp;
                    false
                }
                None => false,
            },
            Event::RunFinished { run, outcome, .. } => {
                if let RunOutcome::Snapshot(snapshot) = outcome {
                    let Some(cut) = self.starting.remove(run) else {
                        return false;
                    };
                    if self.head().map(|h| &h.snapshot.tree) == Some(&snapshot.tree) {
                        return false;
                    }
                    self.snapshots.push(SnapshotEntry {
                        run: *run,
                        at,
                        cut,
                        snapshot: snapshot.clone(),
                    });
                    return true;
                }
                match self.reviews.get_mut(run) {
                    Some(review) if matches!(review.output, ReviewOutput::Running) => {
                        review.output = ReviewOutput::Failed {
                            reason: format!("the reviewer ended without an answer: {outcome:?}"),
                        };
                        true
                    }
                    _ => false,
                }
            }
            Event::ThreadOpened {
                thread,
                comment: id,
                anchor,
                body,
            } => {
                if self.threads.contains_key(thread) {
                    return false;
                }
                let new = Thread {
                    anchor: anchor.clone(),
                    origin: ThreadOrigin::Human,
                    comments: vec![comment(*id, body)],
                    resolved: false,
                };
                self.threads.insert(*thread, new);
                true
            }
            Event::Replied {
                comment: id,
                thread,
                body,
            } => match self.threads.get_mut(thread) {
                Some(t) => {
                    t.comments.push(comment(*id, body));
                    true
                }
                None => false,
            },
            Event::FindingPromoted {
                finding,
                thread,
                comment: id,
            } => {
                if self.threads.contains_key(thread) {
                    return false;
                }
                let Some(f) = self.finding(finding) else {
                    return false;
                };
                let anchor = f
                    .anchor
                    .clone()
                    .unwrap_or(Anchor::Fact(finding.fact.clone()));
                let body = match f.body.is_empty() {
                    true => f.title.clone(),
                    false => format!("{}\n\n{}", f.title, f.body),
                };
                let new = Thread {
                    anchor,
                    origin: ThreadOrigin::Promoted(finding.clone()),
                    comments: vec![comment(*id, &body)],
                    resolved: false,
                };
                self.threads.insert(*thread, new);
                true
            }
            Event::ThreadResolved { thread, resolved } => match self.threads.get_mut(thread) {
                Some(t) if t.resolved != *resolved => {
                    t.resolved = *resolved;
                    true
                }
                _ => false,
            },
            Event::FindingDismissed { finding, dismissed } => match dismissed {
                true => self.dismissed.insert(finding.clone()),
                false => self.dismissed.remove(finding),
            },
            Event::Decided {
                decision,
                tree,
                verdict,
                note,
            } => {
                if *verdict == Decision::RequestChanges {
                    let drafts = self
                        .threads
                        .values_mut()
                        .filter(|t| !t.resolved)
                        .flat_map(|t| t.comments.iter_mut())
                        .filter(|c| c.sent_by.is_none());
                    for c in drafts {
                        c.sent_by = Some(*decision);
                    }
                }
                self.decisions.push(DecisionEntry {
                    id: *decision,
                    fact: fact.id.clone(),
                    at,
                    tree: tree.clone(),
                    verdict: *verdict,
                    note: note.clone(),
                });
                true
            }
            _ => false,
        }
    }
}

/// Carries an inclusive line range of the old file through one file's hunks (old-side
/// numbers, ascending). `None` when a hunk overlaps the range, or inserts inside it.
fn shift((start, end): (i64, i64), hunks: &[crate::diff::Hunk]) -> Option<(i64, i64)> {
    let mut offset = 0i64;
    for h in hunks {
        let (old_start, old_len) = (i64::from(h.old_start), i64::from(h.old_len));
        let grew = i64::from(h.new_len) - old_len;
        if old_len == 0 {
            // A pure insertion after line `old_start`.
            if old_start < start {
                offset += grew;
            } else if old_start < end {
                return None;
            }
        } else if old_start + old_len - 1 < start {
            offset += grew;
        } else if old_start <= end {
            return None;
        }
    }
    Some((start + offset, end + offset))
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
        match &fact.body {
            FactBody::Authored { event, .. } => self.authored(fact, event),
            FactBody::Observed { source, obs, .. } => self.observed(fact, source, obs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shift;
    use crate::diff::Hunk;

    fn hunk(old_start: u32, old_len: u32, new_len: u32) -> Hunk {
        Hunk {
            old_start,
            old_len,
            new_start: old_start,
            new_len,
        }
    }

    /// Lines 10-12 carried through one hunk at each position around them.
    #[test]
    fn a_range_moves_with_edits_around_it_and_ends_when_one_touches_it() {
        let range = (10, 12);
        // Insertions (old_len 0) go after line `old_start`.
        assert_eq!(shift(range, &[hunk(9, 0, 2)]), Some((12, 14)));
        assert_eq!(shift(range, &[hunk(10, 0, 2)]), None);
        assert_eq!(shift(range, &[hunk(11, 0, 2)]), None);
        assert_eq!(shift(range, &[hunk(12, 0, 2)]), Some((10, 12)));
        // Replacements: lines 8-9 become five, just above the range.
        assert_eq!(shift(range, &[hunk(8, 2, 5)]), Some((13, 15)));
        // Lines 1-3 deleted.
        assert_eq!(shift(range, &[hunk(1, 3, 0)]), Some((7, 9)));
        // Touching the first or the last line, or just below.
        assert_eq!(shift(range, &[hunk(9, 2, 2)]), None);
        assert_eq!(shift(range, &[hunk(12, 1, 1)]), None);
        assert_eq!(shift(range, &[hunk(13, 1, 4)]), Some((10, 12)));
        // Offsets add up over several hunks.
        assert_eq!(
            shift(range, &[hunk(1, 1, 3), hunk(5, 2, 0), hunk(20, 1, 9)]),
            Some((10, 12))
        );
        assert_eq!(
            shift(range, &[hunk(1, 1, 4), hunk(5, 1, 2)]),
            Some((14, 16))
        );
    }
}
