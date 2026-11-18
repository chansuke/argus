//! The one-glance state of a job and what needs the human, composed at read time from
//! several views; never stored or synced (the views are the single source).

use crate::event::{Decision, RunOutcome, RunPurpose, TaskState};
use crate::ids::*;
use crate::views::checks::CheckVerdict;
use crate::views::progress::{Activity, PlanNode, Share, WaitReason};
use crate::views::{GateState, JobViews};

#[derive(Clone, Debug)]
pub struct Glance {
    pub state: JobState,
    /// `None`: the job has no plan to measure against.
    pub share: Option<Share>,
    pub now: Vec<(RunId, Activity)>,
    pub review: GateState,
    pub checks: GateState,
    /// Agent runs and how many tool calls since their plan last changed.
    pub stale_plans: Vec<(RunId, u32)>,
    pub blocked: Vec<(TaskKey, String)>,
    /// Billed sessions currently open (should be 0 when nothing is running).
    pub open_leases: u32,
    /// Most urgent first. Empty when nothing needs the human.
    pub attention: Vec<Attention>,
}

/// Derived job lifecycle (no `JobClosed` event: merge and abandon are already facts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState {
    Working,
    /// The head snapshot has no decision yet.
    AwaitingDecision,
    ChangesRequested {
        delivered: bool,
    },
    /// The head is approved; `merge` is admitted.
    Approved,
    Merging,
    Merged,
    Abandoned,
}

/// Why the human is needed. Declaration order is urgency (derived `Ord`). Every variant
/// starts with `since`, so within a variant the longest wait sorts first. (An elapsed
/// "idle for" field would sort the shortest wait first.)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Attention {
    AwaitingPermission {
        since: UnixMillis,
        run: RunId,
    },
    /// A refuted TLA+ property or a failed proof on the head snapshot.
    Counterexample {
        since: UnixMillis,
    },
    ChecksFailing {
        since: UnixMillis,
    },
    /// A feedback delivery run was abandoned or failed to start: nothing was resent.
    DeliveryFailed {
        since: UnixMillis,
        decision: DecisionId,
    },
    AwaitingDecision {
        since: UnixMillis,
    },
    /// The turn ended with nothing new to review (the agent may have asked a question).
    AgentWaiting {
        since: UnixMillis,
        run: RunId,
    },
    Stalled {
        since: UnixMillis,
        run: RunId,
    },
    RunFailed {
        since: UnixMillis,
        run: RunId,
    },
}

/// An agent with no activity for this long, and not waiting on the human, is `Stalled`.
pub const STALL_AFTER_MS: i64 = 10 * 60 * 1000;

impl JobViews {
    /// Composed from runs, progress, review, and checks (the reads are recorded, so a
    /// subscription to the glance depends on exactly those views).
    ///
    /// State, first match wins: a finished merge -> `Merged`; the latest decision is
    /// `Abandon` -> `Abandoned`; a merge run not finished -> `Merging`; an approved head ->
    /// `Approved`; the latest decision on head is `RequestChanges` -> `ChangesRequested`
    /// (`delivered` once its delivery run started); a head without a decision ->
    /// `AwaitingDecision`; else `Working`.
    ///
    /// Attention:
    /// - an agent waiting at a permission prompt -> `AwaitingPermission`
    /// - latest checks on head: `Refuted`/`Holes` -> `Counterexample`, other failures ->
    ///   `ChecksFailing` (one each, from the earliest verdict)
    /// - a delivery run that was abandoned or failed to start -> `DeliveryFailed`
    /// - `AwaitingDecision { since: head.at }`
    /// - an agent at its prompt, and no head waiting for a decision -> `AgentWaiting`
    /// - a running agent with no activity for [`STALL_AFTER_MS`] that is not waiting on
    ///   the human -> `Stalled`
    /// - the latest agent run (not a delivery) exited non-zero, failed to start, or was
    ///   killed without a cancel -> `RunFailed`
    pub fn glance(&self, now: UnixMillis) -> Glance {
        let (runs, progress, review, checks) =
            (self.runs(), self.progress(), self.review(), self.checks());
        let head = review.head();
        let head_tree = head.map(|h| &h.snapshot.tree);
        let on_head = |tree: &Option<TreeOid>| tree.as_ref().is_some_and(|t| Some(t) == head_tree);
        let latest_on_head = review.decisions().iter().rev().find(|d| on_head(&d.tree));
        let deliveries: Vec<(DecisionId, RunId)> = review
            .decisions()
            .iter()
            .filter(|d| d.verdict == Decision::RequestChanges)
            .map(|d| (d.id, d.id.delivery_run()))
            .collect();
        let last = |run: &RunId| runs.get(run).and_then(|r| r.attempts.last());
        let live = |run: &RunId| last(run).is_some_and(|a| a.finished.is_none());

        let merges = runs
            .iter()
            .filter(|(_, r)| matches!(r.purpose, RunPurpose::Merge(_)));
        let mut merged = false;
        let mut merging = false;
        for (id, _) in merges {
            match last(id).and_then(|a| a.finished.as_ref()) {
                Some((_, RunOutcome::Merged { .. })) => merged = true,
                Some(_) => {}
                None => merging = true,
            }
        }
        let state = if merged {
            JobState::Merged
        } else if review
            .decisions()
            .last()
            .is_some_and(|d| d.verdict == Decision::Abandon)
        {
            JobState::Abandoned
        } else if merging {
            JobState::Merging
        } else if review.approved_head().is_some() {
            JobState::Approved
        } else if let Some(d) = latest_on_head.filter(|d| d.verdict == Decision::RequestChanges) {
            JobState::ChangesRequested {
                delivered: last(&d.id.delivery_run()).is_some(),
            }
        } else if head.is_some() && latest_on_head.is_none() {
            JobState::AwaitingDecision
        } else {
            JobState::Working
        };

        let mut attention = Vec::new();
        let activities: Vec<(RunId, Activity)> = progress
            .activities()
            .filter(|(_, a)| !matches!(a, Activity::Exited { .. }))
            .map(|(r, a)| (*r, a.clone()))
            .collect();
        for (run, activity) in &activities {
            match activity {
                Activity::AwaitingHuman {
                    since,
                    why: WaitReason::Permission { .. },
                } => attention.push(Attention::AwaitingPermission {
                    since: *since,
                    run: *run,
                }),
                Activity::AwaitingHuman {
                    since,
                    why: WaitReason::TurnEnded,
                } if state != JobState::AwaitingDecision => {
                    attention.push(Attention::AgentWaiting {
                        since: *since,
                        run: *run,
                    })
                }
                Activity::AwaitingHuman { .. } => {}
                other if live(run) && now.0 - other.since().0 > STALL_AFTER_MS => {
                    attention.push(Attention::Stalled {
                        since: other.since(),
                        run: *run,
                    })
                }
                _ => {}
            }
        }
        let (mut refuted, mut failing): (Option<UnixMillis>, Option<UnixMillis>) = (None, None);
        for (_, check) in checks.latest() {
            let Some((at, verdict)) = &check.verdict else {
                continue;
            };
            if Some(&check.request.tree) != head_tree {
                continue;
            }
            let slot = match verdict {
                CheckVerdict::Proved | CheckVerdict::Passed => continue,
                CheckVerdict::Refuted { .. } | CheckVerdict::Holes { .. } => &mut refuted,
                CheckVerdict::ToolError { .. } | CheckVerdict::Cancelled => &mut failing,
            };
            *slot = Some(slot.map_or(*at, |s| s.min(*at)));
        }
        attention.extend(refuted.map(|since| Attention::Counterexample { since }));
        attention.extend(failing.map(|since| Attention::ChecksFailing { since }));
        for (decision, delivery) in &deliveries {
            if let Some((since, RunOutcome::Abandoned | RunOutcome::SpawnFailed { .. })) =
                last(delivery).and_then(|a| a.finished.as_ref())
            {
                attention.push(Attention::DeliveryFailed {
                    since: *since,
                    decision: *decision,
                });
            }
        }
        if let (JobState::AwaitingDecision, Some(head)) = (state, head) {
            attention.push(Attention::AwaitingDecision { since: head.at });
        }
        if let Some((run, _)) = runs.latest_agent()
            && !deliveries.iter().any(|(_, d)| d == run)
            && let Some((since, outcome)) = last(run).and_then(|a| a.finished.as_ref())
        {
            let failed = match outcome {
                RunOutcome::Exited { code } => *code != 0,
                RunOutcome::SpawnFailed { .. } => true,
                RunOutcome::Killed { cancelled, .. } => !cancelled,
                _ => false,
            };
            if failed {
                attention.push(Attention::RunFailed {
                    since: *since,
                    run: *run,
                });
            }
        }
        attention.sort();

        let mut blocked = Vec::new();
        fn walk(node: &PlanNode<'_>, out: &mut Vec<(TaskKey, String)>) {
            if let PlanNode::Task {
                key,
                node,
                children,
                ..
            } = node
            {
                if let TaskState::Blocked { reason } = &node.state {
                    out.push((**key, reason.clone()));
                }
                children.iter().for_each(|c| walk(c, out));
            }
        }
        progress
            .tree()
            .roots
            .iter()
            .for_each(|n| walk(n, &mut blocked));

        Glance {
            state,
            share: progress.share(),
            now: activities,
            review: review.gate(head_tree),
            checks: checks.gate(head_tree),
            stale_plans: progress.staleness().map(|(r, n)| (*r, n)).collect(),
            blocked,
            open_leases: runs.unclosed_leases().count() as u32,
            attention,
        }
    }
}

/// The overview queue, which is also the phone's home screen: every job's attention
/// items, most urgent first across jobs (ties by job id, so the order is stable).
pub fn queue<'a>(
    jobs: impl IntoIterator<Item = (JobId, &'a Glance)>,
) -> Vec<(JobId, &'a Attention)> {
    let mut q: Vec<(JobId, &Attention)> = jobs
        .into_iter()
        .flat_map(|(job, g)| g.attention.iter().map(move |a| (job, a)))
        .collect();
    q.sort_by(|(ja, a), (jb, b)| a.cmp(b).then(ja.cmp(jb)));
    q
}
