//! Fact builders shared by the view tests: fresh ids and increasing stamps, the way the
//! ledger hands facts to projections.

#![allow(dead_code)]

use std::num::NonZeroU32;
use std::path::PathBuf;

use argus_core::event::{
    AgentLaunch, Author, Event, LlmCall, RunOutcome, RunPurpose, TaskPatch, TaskState, Weight,
};
use argus_core::fact::{Fact, FactBody};
use argus_core::ids::{
    Attempt, EventId, FactId, Id128, ModelId, RunId, Stamp, TaskKey, ToolCallId, ToolName,
    UnixMillis,
};
use argus_core::models::{AgentCli, ModelChoice};
use argus_core::source::{
    AgentEvent, AgentWhat, Observation, PlanItem, PlanOp, PlanStatus, SourceKind, SourceMeta,
    SourceOrigin,
};
use argus_core::views::Projection;

/// Hands facts to a projection with fresh ids and increasing stamps, the way the ledger does.
#[derive(Default)]
pub struct Feed {
    n: u64,
}

impl Feed {
    pub fn next_id(&mut self) -> (FactId, Stamp) {
        self.n += 1;
        let id = FactId {
            event: EventId::parse(&format!("host:{}", self.n)).unwrap(),
            part: 0,
        };
        let stamp = Stamp {
            wall_ns: self.n,
            logical: 0,
        };
        (id, stamp)
    }

    pub fn authored<P: Projection>(
        &mut self,
        view: &mut P,
        at: i64,
        author: Author,
        event: Event,
    ) -> (FactId, bool) {
        let (id, stamp) = self.next_id();
        let fact = Fact {
            id: id.clone(),
            stamp,
            at: UnixMillis(at),
            body: FactBody::Authored {
                author: &author,
                event: &event,
            },
        };
        (id, view.apply(&fact))
    }

    pub fn observed<P: Projection>(
        &mut self,
        view: &mut P,
        source: &SourceMeta,
        at: i64,
        obs: Observation,
    ) -> (FactId, bool) {
        let (id, stamp) = self.next_id();
        let fact = Fact {
            id: id.clone(),
            stamp,
            at: UnixMillis(at),
            body: FactBody::Observed {
                source,
                obs: &obs,
                raw: b"",
            },
        };
        (id, view.apply(&fact))
    }

    pub fn agent<P: Projection>(
        &mut self,
        view: &mut P,
        source: &SourceMeta,
        at: i64,
        what: AgentWhat,
    ) -> (FactId, bool) {
        let obs = Observation::Agent(AgentEvent {
            record: None,
            parent: None,
            at: Some(UnixMillis(at)),
            sidechain: false,
            what,
        });
        self.observed(view, source, at, obs)
    }
}

pub fn run(n: u128) -> RunId {
    RunId(Id128(n))
}

pub fn task(n: u128) -> TaskKey {
    TaskKey(Id128(n))
}

pub fn source(run_id: RunId, kind: SourceKind, origin: SourceOrigin) -> SourceMeta {
    SourceMeta {
        id: SourceMeta::derive_id(run_id, Attempt(0), &origin),
        run: run_id,
        attempt: Attempt(0),
        kind,
        origin,
    }
}

pub fn transcript(run_id: RunId) -> SourceMeta {
    source(
        run_id,
        SourceKind::ClaudeTranscript,
        SourceOrigin::File(PathBuf::from("/h/.claude/projects/w/s.jsonl")),
    )
}

pub fn hook(run_id: RunId) -> SourceMeta {
    source(run_id, SourceKind::ClaudeHook, SourceOrigin::Hook)
}

pub fn headless(run_id: RunId) -> SourceMeta {
    source(run_id, SourceKind::ClaudeStreamJson, SourceOrigin::Stdout)
}

pub fn agent_requested(n: u128) -> Event {
    Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Agent(AgentLaunch {
            cli: AgentCli::Claude,
            model: None,
            prompt: "go".into(),
            resume: None,
        }),
    }
}

pub fn llm() -> LlmCall {
    LlmCall {
        model: ModelChoice {
            cli: AgentCli::Claude,
            model: ModelId("sonnet".into()),
            effort: None,
        },
        prompt: "summarize".into(),
    }
}

pub fn started(n: u128) -> Event {
    Event::RunStarted {
        run: run(n),
        attempt: Attempt(0),
        pid: None,
    }
}

pub fn finished(n: u128, code: i32) -> Event {
    Event::RunFinished {
        run: run(n),
        attempt: Attempt(0),
        outcome: RunOutcome::Exited { code },
    }
}

pub fn call(id: &str, name: &str) -> AgentWhat {
    AgentWhat::ToolCall {
        call: ToolCallId(id.into()),
        name: ToolName(name.into()),
        input_json: "{}".into(),
        touches: Vec::new(),
    }
}

pub fn result(id: &str, is_error: bool) -> AgentWhat {
    AgentWhat::ToolResult {
        call: ToolCallId(id.into()),
        is_error,
        output: String::new(),
    }
}

pub fn reply(text: &str) -> AgentWhat {
    AgentWhat::Reply {
        text: text.into(),
        model: None,
    }
}

pub fn todo(items: &[(&str, PlanStatus)]) -> AgentWhat {
    AgentWhat::Plan(PlanOp::Replace(
        items
            .iter()
            .map(|(content, status)| PlanItem {
                content: (*content).into(),
                status: *status,
            })
            .collect(),
    ))
}

pub fn set(key: u128, parent: Option<u128>, weight: u32, state: TaskState) -> Event {
    Event::TaskSet {
        task: task(key),
        patch: TaskPatch {
            parent: Some(parent.map(task)),
            title: Some(format!("task {key}")),
            weight: Some(Weight(NonZeroU32::new(weight).unwrap())),
            state: Some(state),
        },
    }
}

pub const DONE: TaskState = TaskState::Done { result: None };
pub const M15: i64 = 15 * 60 * 1000;
/// 2026-10-24T03:00:00Z, aligned to every bucket width.
pub const T0: i64 = 1_792_810_800_000;
