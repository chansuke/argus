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

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use crate::event::{Author, Event, RunOutcome, RunPurpose, TaskPatch, TaskState, Weight};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{AgentWhat, Observation, PlanOp, PlanStatus, SourceFamily};
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
    /// Agent task id (or zero-padded list position for `TodoWrite`, so keys sort in list
    /// order) -> (subject, status).
    pub items: BTreeMap<String, (String, PlanStatus)>,
    pub changed_at: Option<UnixMillis>,
}

#[derive(Default, Debug)]
pub struct Progress {
    tasks: BTreeMap<TaskKey, TaskNode>,
    observed: BTreeMap<RunId, ObservedPlan>,
    activity: BTreeMap<RunId, Activity>,
    calls_since_plan_change: BTreeMap<RunId, u32>,
    /// Interactive agent runs. Only these have activity: an imported transcript is history.
    agents: BTreeSet<RunId>,
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
    /// Tasks by key (a task whose parent is unknown or on a parent cycle is a root), then
    /// each run's observed plan.
    pub fn tree(&self) -> PlanTree<'_> {
        let mut children: BTreeMap<&TaskKey, Vec<&TaskKey>> = BTreeMap::new();
        let mut roots = Vec::new();
        for key in self.tasks.keys() {
            match self.effective_parent(key) {
                Some(parent) => children.entry(parent).or_default().push(key),
                None => roots.push(key),
            }
        }
        let mut nodes: Vec<PlanNode<'_>> = roots
            .into_iter()
            .map(|key| self.task_node(key, &children))
            .collect();
        nodes.extend(self.observed.iter().map(|(run, plan)| PlanNode::Observed {
            run,
            plan,
            share: plan.share().unwrap_or(Share::new(0.0)),
            counted: self.counts_observed(run, plan),
        }));
        PlanTree { roots: nodes }
    }

    /// The job's share: the weighted share of the top-level tasks and of every counted
    /// observed plan (weight 1 each). `None` with nothing to count: most agents never
    /// write a plan, and "no plan" must not read as "nothing done".
    pub fn share(&self) -> Option<Share> {
        let tree = self.tree();
        let parts = tree.roots.iter().filter_map(|node| match node {
            PlanNode::Task { node, share, .. } => {
                (node.state != TaskState::Dropped).then_some((node.weight.0.get(), *share))
            }
            PlanNode::Observed { share, counted, .. } => counted.then_some((1, *share)),
        });
        weighted(parts)
    }

    fn task_node<'a>(
        &'a self,
        key: &'a TaskKey,
        children: &BTreeMap<&'a TaskKey, Vec<&'a TaskKey>>,
    ) -> PlanNode<'a> {
        let node = &self.tasks[key];
        let kids: Vec<PlanNode<'a>> = children
            .get(key)
            .into_iter()
            .flatten()
            .map(|child| self.task_node(child, children))
            .collect();
        let counted = kids.iter().filter_map(|kid| match kid {
            PlanNode::Task { node, share, .. } => {
                (node.state != TaskState::Dropped).then_some((node.weight.0.get(), *share))
            }
            PlanNode::Observed { .. } => None,
        });
        let leaf = Share::new(if matches!(node.state, TaskState::Done { .. }) {
            1.0
        } else {
            0.0
        });
        PlanNode::Task {
            key,
            node,
            share: weighted(counted).unwrap_or(leaf),
            children: kids,
        }
    }

    /// The parent to draw `key` under, or `None` if it has none, its parent is unknown, or
    /// its ancestors loop (a parent cycle made of assignments, each valid on its own).
    fn effective_parent(&self, key: &TaskKey) -> Option<&TaskKey> {
        let parent = self.tasks.get(key)?.parent.as_ref()?;
        let mut seen = BTreeSet::from([key]);
        let mut at = parent;
        loop {
            if !seen.insert(at) {
                return None;
            }
            match self.tasks.get(at)?.parent.as_ref() {
                Some(next) => at = next,
                None => return self.tasks.contains_key(parent).then_some(parent),
            }
        }
    }

    /// One precedence rule: a human plan is the skeleton, and an agent's self-reported
    /// tasks replace its observed plan. Otherwise the observed plan counts.
    fn counts_observed(&self, run: &RunId, plan: &ObservedPlan) -> bool {
        let reported = |by: PlanAuthor| self.tasks.values().any(|t| t.by == by);
        plan.share().is_some() && !reported(PlanAuthor::Human) && !reported(PlanAuthor::Agent(*run))
    }

    fn set_task(&mut self, at: UnixMillis, by: PlanAuthor, key: TaskKey, patch: &TaskPatch) {
        let node = self.tasks.entry(key).or_insert_with(|| TaskNode {
            parent: None,
            title: String::new(),
            weight: Weight(NonZeroU32::MIN),
            state: TaskState::Pending,
            by,
            updated_at: at,
        });
        if let Some(parent) = &patch.parent {
            node.parent = *parent;
        }
        if let Some(title) = &patch.title {
            node.title = title.clone();
        }
        if let Some(weight) = patch.weight {
            node.weight = weight;
        }
        if let Some(state) = &patch.state {
            node.state = state.clone();
        }
        node.by = by;
        node.updated_at = at;
    }

    fn authored(&mut self, at: UnixMillis, author: &Author, event: &Event) -> bool {
        match event {
            Event::TaskSet { task, patch } => {
                let by = match author {
                    Author::Agent(run) => PlanAuthor::Agent(*run),
                    // `Event::check` admits only the human and agents.
                    _ => PlanAuthor::Human,
                };
                self.set_task(at, by, *task, patch);
                true
            }
            Event::RunRequested {
                run,
                purpose: RunPurpose::Agent(_),
            } => {
                self.agents.insert(*run);
                false
            }
            Event::RunStarted { run, .. } if self.agents.contains(run) => {
                self.activity.insert(*run, Activity::Starting { since: at });
                true
            }
            Event::RunFinished { run, outcome, .. } if self.agents.contains(run) => {
                let ok = matches!(outcome, RunOutcome::Exited { code: 0 });
                self.activity.insert(*run, Activity::Exited { at, ok });
                true
            }
            _ => false,
        }
    }

    fn observed(&mut self, fact: &Fact<'_>, run: RunId, what: &AgentWhat) -> bool {
        let at = fact.at;
        if let AgentWhat::Plan(op) = what {
            let plan = self.observed.entry(run).or_default();
            match op {
                PlanOp::Replace(items) => {
                    plan.items = items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| (format!("{i:05}"), (item.content.clone(), item.status)))
                        .collect();
                }
                PlanOp::Upsert {
                    task,
                    subject,
                    status,
                } => {
                    let entry = plan
                        .items
                        .entry(task.clone())
                        .or_insert_with(|| (String::new(), PlanStatus::Pending));
                    if let Some(subject) = subject {
                        entry.0 = subject.clone();
                    }
                    if let Some(status) = status {
                        entry.1 = *status;
                    }
                }
            }
            plan.changed_at = Some(at);
            self.calls_since_plan_change.insert(run, 0);
            return true;
        }
        if let AgentWhat::ToolCall { .. } = what {
            *self.calls_since_plan_change.entry(run).or_default() += 1;
        }
        if !self.agents.contains(&run)
            || matches!(self.activity.get(&run), Some(Activity::Exited { .. }))
        {
            return matches!(what, AgentWhat::ToolCall { .. });
        }
        let next = match what {
            AgentWhat::ToolCall { name, .. } => Activity::Tool {
                name: name.clone(),
                since: at,
                call: fact.id.clone(),
            },
            AgentWhat::Prompt { .. }
            | AgentWhat::Thinking { .. }
            | AgentWhat::ToolResult { .. } => Activity::Thinking { since: at },
            AgentWhat::Reply { .. } | AgentWhat::Result { .. } => Activity::Writing { since: at },
            AgentWhat::TurnEnded { .. } => Activity::AwaitingHuman {
                since: at,
                why: WaitReason::TurnEnded,
            },
            AgentWhat::PermissionWait { message } => Activity::AwaitingHuman {
                since: at,
                why: WaitReason::Permission {
                    message: message.clone(),
                },
            },
            AgentWhat::Usage { .. } | AgentWhat::Other { .. } | AgentWhat::Plan(_) => {
                return false;
            }
        };
        self.activity.insert(run, next);
        true
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
        match &fact.body {
            FactBody::Authored { author, event } => self.authored(fact.at, author, event),
            FactBody::Observed {
                source,
                obs: Observation::Agent(event),
                ..
            } => self.observed(fact, source.run, &event.what),
            FactBody::Observed { .. } => false,
        }
    }
}

impl ObservedPlan {
    /// Completed over live (not deleted) items; `None` with no live items.
    pub fn share(&self) -> Option<Share> {
        let live = self
            .items
            .values()
            .filter(|(_, s)| *s != PlanStatus::Deleted);
        let parts = live.map(|(_, s)| {
            (
                1,
                Share::new(f64::from(u8::from(*s == PlanStatus::Completed))),
            )
        });
        weighted(parts)
    }
}

/// Weighted mean of shares; `None` when there is nothing to weigh.
fn weighted(parts: impl Iterator<Item = (u32, Share)>) -> Option<Share> {
    let (mut sum, mut total) = (0.0, 0u64);
    for (weight, share) in parts {
        sum += f64::from(weight) * share.get();
        total += u64::from(weight);
    }
    (total > 0).then(|| Share::new(sum / total as f64))
}
