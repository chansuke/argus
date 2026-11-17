//! What happened when, across every lane of a job.
//!
//! Items are keyed by domain time, then fact id: insertion of an out-of-order fact (an
//! imported old transcript, a late subagent file) is O(log N), and identity is the fact
//! id, so nothing is ever searched for duplicates. Roll-ups are computed from items on read; only the
//! LLM-written text of a window is stored (as a `Summarize` run's output).

use std::collections::BTreeMap;

use crate::fact::Fact;
use crate::ids::*;
use crate::source::SourceFamily;
use crate::views::Projection;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum BucketWidth {
    M15,
    H1,
    H4,
}

/// A time window, aligned to its width in UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Window {
    pub width: BucketWidth,
    pub start: UnixMillis,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Lane {
    /// One coding agent (its subagents render as nested sub-lanes via `sidechain`).
    Agent(RunId),
    /// Review / check / summary / device runs.
    Run(RunId),
    Human,
    Host,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub lane: Lane,
    pub kind: ItemKind,
    /// One line; the raw view has the rest.
    pub title: String,
    /// Set when the matching end arrives (tool result, run finished).
    pub ended_at: Option<UnixMillis>,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Prompt,
    Reply,
    Thinking,
    ToolCall {
        name: ToolName,
        ok: Option<bool>,
    },
    PlanChange,
    Snapshot,
    RunLifecycle,
    Finding,
    CheckVerdict,
    Artifact,
    Comment,
    TaskChange,
    HumanInput,
    Decision,
    /// The agent rewrote its transcript; the tailer continued after a marker.
    Discontinuity,
    Unrecognized,
}

#[derive(Default, Debug)]
pub struct Timeline {
    /// Two levels so a time range needs no "smallest FactId" sentinel.
    items: BTreeMap<UnixMillis, BTreeMap<FactId, Item>>,
    /// Open tool calls -> their item key, so a result closes its call in O(log N).
    open_calls: BTreeMap<ToolCallId, (UnixMillis, FactId)>,
    /// Summarize run -> window; window -> result fact once the run finished.
    summary_runs: BTreeMap<RunId, Window>,
    summaries: BTreeMap<Window, FactId>,
}

#[derive(Clone, Debug)]
pub struct Bucket {
    pub window: Window,
    pub counts: BTreeMap<Lane, LaneCounts>,
    pub summary: Option<FactId>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LaneCounts {
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub replies: u32,
    pub human_inputs: u32,
}

impl Timeline {
    /// Items with `from <= at < to`, in (at, fact) order.
    pub fn range(
        &self,
        from: UnixMillis,
        to: UnixMillis,
    ) -> impl Iterator<Item = (UnixMillis, &FactId, &Item)> {
        self.items
            .range(from..to)
            .flat_map(|(at, items)| items.iter().map(move |(f, i)| (*at, f, i)))
    }
    /// M15 buckets come from items; H1 folds four M15, H4 folds four H1 (dsh-style
    /// roll-up, but derived, so it can never disagree with the items).
    pub fn buckets(&self, width: BucketWidth, from: UnixMillis, to: UnixMillis) -> Vec<Bucket> {
        todo!("not implemented")
    }
    /// Windows that ended, had activity, and have no summary run yet (policy input).
    pub fn unsummarized(&self, width: BucketWidth, now: UnixMillis) -> Vec<Window> {
        todo!("not implemented")
    }
}

impl Projection for Timeline {
    const NAME: &'static str = "timeline";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[
        SourceFamily::AgentSession,
        SourceFamily::AgentHeadless,
        SourceFamily::AgentHook,
        SourceFamily::Checker,
        SourceFamily::Device,
    ];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO: map each fact kind to an Item (or nothing, e.g. Usage); ToolCall -> insert
        //       and remember in open_calls; ToolResult -> set ended_at/ok on its call.
        todo!("not implemented")
    }
}
