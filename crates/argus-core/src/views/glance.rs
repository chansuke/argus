//! The one-glance state of a job and what needs the human, composed at read time from
//! several views; never stored or synced (the views are the single source).

use crate::ids::*;
use crate::views::progress::{Activity, Share};
use crate::views::{GateState, JobViews};

#[derive(Clone, Debug)]
pub struct Glance {
    pub state: JobState,
    pub share: Share,
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
    /// TODO: share/now/staleness from progress; head = review.head(); review gate and
    /// checks gate on head; open_leases from runs; state from runs (live agent, merge
    /// run) + review (decisions on head, approved_head); attention:
    /// - Activity::AwaitingHuman(Permission) -> AwaitingPermission
    /// - checks on head: Refuted/Holes -> Counterexample, other Red -> ChecksFailing
    /// - a RequestChanges whose delivery run ended Abandoned/SpawnFailed -> DeliveryFailed
    /// - head with no decision -> AwaitingDecision { since: head.at }
    /// - AwaitingHuman(TurnEnded) and no undecided head -> AgentWaiting
    /// - live agent with `now - activity.since() > STALL_AFTER_MS` -> Stalled
    /// - last agent attempt Exited non-zero / SpawnFailed (not cancelled) -> RunFailed
    ///   then sort.
    pub fn glance(&self, now: UnixMillis) -> Glance {
        todo!("not implemented")
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
