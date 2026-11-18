//! Conversation analytics. Per job: hourly counters per agent run (`JobStats`, a
//! checkpointable projection). Store-wide: `Analytics` = merge of every job's `JobStats`
//! (sums keyed by job/run, so merge order does not matter).

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{Event, RunPurpose};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::models::AgentCli;
use crate::period::{HourBucket, Period};
use crate::source::{AgentWhat, Observation, SourceFamily, TokenUsage};
use crate::views::{Checkpoint, Projection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub prompts: u64,
    pub replies: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Human input typed into a running agent, and permission waits: intervention load.
    pub human_inputs: u64,
    pub review_findings: u64,
}

#[derive(Debug, Default)]
pub struct RunStats {
    pub cli: Option<AgentCli>,
    pub models: BTreeSet<ModelId>,
    pub hours: BTreeMap<HourBucket, Counters>,
    pub tools: BTreeMap<ToolName, u64>,
}

#[derive(Debug, Default)]
pub struct JobStats {
    pub job: Option<JobId>,
    pub runs: BTreeMap<RunId, RunStats>,
    /// The usage counted so far for each API message, and the hour it was counted in.
    /// Claude Code repeats a message's usage in every record of it, growing while it
    /// streams, so a later value replaces the earlier one (in the first hour, so a total
    /// never moves between hours).
    usage: BTreeMap<(RunId, String), (HourBucket, TokenUsage)>,
}

impl Counters {
    fn add_usage(&mut self, u: &TokenUsage) {
        self.input_tokens += u.input;
        self.output_tokens += u.output;
        self.cache_read_tokens += u.cache_read;
        self.cache_write_tokens += u.cache_write;
    }
    fn remove_usage(&mut self, u: &TokenUsage) {
        self.input_tokens -= u.input;
        self.output_tokens -= u.output;
        self.cache_read_tokens -= u.cache_read;
        self.cache_write_tokens -= u.cache_write;
    }
}

impl JobStats {
    fn counters(&mut self, run: RunId, hour: HourBucket) -> &mut Counters {
        self.runs
            .entry(run)
            .or_default()
            .hours
            .entry(hour)
            .or_default()
    }

    fn usage(&mut self, run: RunId, hour: HourBucket, message: Option<&String>, u: &TokenUsage) {
        let Some(message) = message else {
            self.counters(run, hour).add_usage(u);
            return;
        };
        let key = (run, message.clone());
        let hour = match self.usage.get(&key) {
            Some(&(first, previous)) => {
                self.counters(run, first).remove_usage(&previous);
                first
            }
            None => hour,
        };
        self.usage.insert(key, (hour, *u));
        self.counters(run, hour).add_usage(u);
    }
}

impl Projection for JobStats {
    const NAME: &'static str = "stats";
    const VERSION: u32 = 1;
    /// Hooks carry permission waits, which count as intervention load.
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentSession, SourceFamily::AgentHook];
    /// Repeated records never arrive (`ledger::Applied`); repeated usage inside one message
    /// is replaced, not added.
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        let hour = HourBucket::of(fact.at);
        match &fact.body {
            FactBody::Authored { event, .. } => match event {
                Event::JobOpened(info) => {
                    self.job = Some(info.job);
                    true
                }
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Agent(launch),
                } => {
                    let stats = self.runs.entry(*run).or_default();
                    stats.cli = Some(launch.cli);
                    stats.models.extend(launch.model.clone());
                    true
                }
                Event::RunInput { run, .. } => {
                    self.counters(*run, hour).human_inputs += 1;
                    true
                }
                _ => false,
            },
            FactBody::Observed {
                source,
                obs: Observation::Agent(event),
                ..
            } => {
                let run = source.run;
                match &event.what {
                    AgentWhat::Prompt { .. } => self.counters(run, hour).prompts += 1,
                    AgentWhat::Reply { model, .. } => {
                        self.counters(run, hour).replies += 1;
                        self.runs
                            .entry(run)
                            .or_default()
                            .models
                            .extend(model.clone());
                    }
                    AgentWhat::ToolCall { name, .. } => {
                        self.counters(run, hour).tool_calls += 1;
                        *self
                            .runs
                            .entry(run)
                            .or_default()
                            .tools
                            .entry(name.clone())
                            .or_default() += 1;
                    }
                    AgentWhat::ToolResult { is_error: true, .. } => {
                        self.counters(run, hour).tool_errors += 1
                    }
                    AgentWhat::PermissionWait { .. } => self.counters(run, hour).human_inputs += 1,
                    AgentWhat::Usage { message, usage } => {
                        self.usage(run, hour, message.as_ref(), usage)
                    }
                    _ => return false,
                }
                true
            }
            FactBody::Observed { .. } => false,
        }
    }
}

impl Checkpoint for JobStats {
    fn save(&self) -> Vec<u8> {
        todo!("not implemented")
    }
    fn load(bytes: &[u8]) -> Option<Self> {
        todo!("not implemented")
    }
}

#[derive(Debug, Default)]
pub struct Analytics {
    pub jobs: BTreeMap<JobId, JobStats>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupBy {
    Agent,
    Model,
    Job,
    Day,
    Tool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: String,
    pub counters: Counters,
    pub runs: u32,
}

impl Analytics {
    pub fn slice(&self, period: &Period, by: GroupBy) -> Vec<Row> {
        todo!("not implemented")
    }
}
