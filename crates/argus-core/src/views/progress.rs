//! How far along, and what is happening now.
//!
//! Plan sources, merged at read time (each kept separately, per actor):
//! - **Human plan**: `TaskSet` by `Author::Human`. If present, it is the job's skeleton.
//! - **Self-reported plan**: `TaskSet` by `Author::Agent(run)` via `argus task` / MCP.
//!   Needed for what observation cannot give: weights, blockers, results, attaching work
//!   under a human task. Optional.
//! - **Observed plan**: the agent's native plan tool (`TodoWrite`, `TaskCreate`/`TaskUpdate`)
//!   decoded from its transcript. Zero cooperation required; the default.
//!
//! Precedence per agent run (one rule, here only): self-reported tasks, when any exist,
//! replace the observed plan for that run in the share computation; the observed plan is
//! still listed. This avoids double counting when an agent uses both.
//!
//! Share (dsh-timeline semantics): leaf = 1 if Done else 0; node = sum over non-dropped
//! children of `weight / sum(weights) * share(child)`. Observed items weigh 1 each.
//!
//! Activity is derived from the tail of the agent's own stream, so it needs no self-report.

use std::collections::BTreeMap;

use crate::event::{TaskState, Weight};
use crate::fact::Fact;
use crate::ids::*;
use crate::source::{PlanStatus, SourceFamily};
use crate::views::Projection;

/// Fraction done in `[0, 1]`. Display only, so `f64` is fine.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Share(f64);

impl Share {
    pub fn new(x: f64) -> Self {
        Share(x.clamp(0.0, 1.0))
    }
    pub fn get(self) -> f64 {
        self.0
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Activity {
    Starting {
        since: UnixMillis,
    },
    Thinking {
        since: UnixMillis,
    },
    Writing {
        since: UnixMillis,
    },
    Tool {
        name: ToolName,
        since: UnixMillis,
        call: FactId,
    },
    /// The agent waits for the human.
    AwaitingHuman {
        since: UnixMillis,
        why: WaitReason,
    },
    Exited {
        at: UnixMillis,
        ok: bool,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub enum WaitReason {
    /// A tool call is open and a `PermissionWait` arrived.
    Permission { message: String },
    /// The turn ended (an answer, a question, or done).
    TurnEnded,
}

impl Activity {
    /// When the agent last did anything (stall detection in `glance`).
    pub fn since(&self) -> UnixMillis {
        match self {
            Activity::Starting { since }
            | Activity::Thinking { since }
            | Activity::Writing { since }
            | Activity::Tool { since, .. }
            | Activity::AwaitingHuman { since, .. } => *since,
            Activity::Exited { at, .. } => *at,
        }
    }
}

#[derive(Debug)]
pub struct TaskNode {
    pub parent: Option<TaskKey>,
    pub title: String,
    pub weight: Weight,
    pub state: TaskState,
    pub by: PlanAuthor,
    pub updated_at: UnixMillis,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlanAuthor {
    Human,
    Agent(RunId),
}

#[derive(Default, Debug)]
pub struct ObservedPlan {
    /// Agent task id (or list position for `TodoWrite`) -> (subject, status).
    pub items: BTreeMap<String, (String, PlanStatus)>,
    pub changed_at: Option<UnixMillis>,
}

#[derive(Default, Debug)]
pub struct Progress {
    tasks: BTreeMap<TaskKey, TaskNode>,
    observed: BTreeMap<RunId, ObservedPlan>,
    activity: BTreeMap<RunId, Activity>,
    calls_since_plan_change: BTreeMap<RunId, u32>,
}

/// Read-time merged plan, for the UI tree.
#[derive(Debug)]
pub struct PlanTree<'a> {
    pub roots: Vec<PlanNode<'a>>,
}

#[derive(Debug)]
pub enum PlanNode<'a> {
    Task {
        key: &'a TaskKey,
        node: &'a TaskNode,
        share: Share,
        children: Vec<PlanNode<'a>>,
    },
    Observed {
        run: &'a RunId,
        plan: &'a ObservedPlan,
        share: Share,
        counted: bool,
    },
}

impl Progress {
    pub fn tree(&self) -> PlanTree<'_> {
        todo!("not implemented")
    }
    pub fn share(&self) -> Share {
        todo!("not implemented")
    }
    pub fn activities(&self) -> impl Iterator<Item = (&RunId, &Activity)> {
        self.activity.iter()
    }
    pub fn staleness(&self) -> impl Iterator<Item = (&RunId, u32)> {
        self.calls_since_plan_change.iter().map(|(r, n)| (r, *n))
    }
}

impl Projection for Progress {
    const NAME: &'static str = "progress";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentSession, SourceFamily::AgentHook];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO:
        // - TaskSet: apply patch fields (assignments) to tasks[key]; by = author.
        // - Plan(Replace) -> observed[run] = items by position; Plan(Upsert) -> one item;
        //   reset calls_since_plan_change[run].
        // - ToolCall -> activity Tool, calls_since += 1; ToolResult -> Thinking;
        //   Reply -> Writing; TurnEnded -> AwaitingHuman(TurnEnded);
        //   PermissionWait -> AwaitingHuman(Permission).
        // - RunStarted(agent) -> Starting; RunFinished(agent) -> Exited.
        // - Usage and anything else: return false (nothing visible changed).
        todo!("not implemented")
    }
}
