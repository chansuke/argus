//! Conversation analytics. Per job: hourly counters per agent run (`JobStats`, a
//! checkpointable projection). Store-wide: `Analytics` = merge of every job's `JobStats`
//! (sums keyed by job/run, so merge order does not matter).

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{Event, RunPurpose};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::models::AgentCli;
use crate::period::{HourBucket, Period, UtcOffset};
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
    /// Calls per tool, per hour, so a period can be cut from them like `hours`.
    pub tools: BTreeMap<HourBucket, BTreeMap<ToolName, u64>>,
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
    fn add(&mut self, o: &Counters) {
        self.prompts += o.prompts;
        self.replies += o.replies;
        self.tool_calls += o.tool_calls;
        self.tool_errors += o.tool_errors;
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.human_inputs += o.human_inputs;
        self.review_findings += o.review_findings;
    }
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
    const VERSION: u32 = 2;
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
                            .entry(hour)
                            .or_default()
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
    /// Sums over the whole hours inside `period` (`Period::hours`), one row per key, sorted
    /// by key. `runs` counts the runs with anything in a row's hours. A run's model key is
    /// its models joined by `+` (counters are per run, not per model), `unknown` if none;
    /// a day is local to `tz`. Tool rows count only `tool_calls`.
    pub fn slice(&self, period: &Period, by: GroupBy, tz: UtcOffset) -> Vec<Row> {
        let (hours, _) = period.hours();
        let mut rows: BTreeMap<String, (Counters, BTreeSet<RunId>)> = BTreeMap::new();
        let mut add = |key: String, run: RunId, c: &Counters| {
            let (sum, runs) = rows.entry(key).or_default();
            sum.add(c);
            runs.insert(run);
        };
        for (job, stats) in &self.jobs {
            for (run, r) in &stats.runs {
                let inside = |h: &&HourBucket| hours.contains(&h.0);
                if by == GroupBy::Tool {
                    for (_, tools) in r.tools.iter().filter(|(h, _)| inside(h)) {
                        for (tool, n) in tools {
                            let calls = Counters {
                                tool_calls: *n,
                                ..Counters::default()
                            };
                            add(tool.0.clone(), *run, &calls);
                        }
                    }
                    continue;
                }
                for (hour, c) in r.hours.iter().filter(|(h, _)| inside(h)) {
                    let key = match by {
                        GroupBy::Agent => match r.cli {
                            Some(AgentCli::Claude) => "claude".into(),
                            Some(AgentCli::Codex) => "codex".into(),
                            None => "unknown".into(),
                        },
                        GroupBy::Model if r.models.is_empty() => "unknown".into(),
                        GroupBy::Model => {
                            let names: Vec<&str> = r.models.iter().map(|m| m.0.as_str()).collect();
                            names.join("+")
                        }
                        GroupBy::Job => job.0.to_string(),
                        GroupBy::Day => tz.date(UnixMillis(hour.0 * 3_600_000)),
                        GroupBy::Tool => unreachable!("handled above"),
                    };
                    add(key, *run, c);
                }
            }
        }
        rows.into_iter()
            .map(|(key, (counters, runs))| Row {
                key,
                counters,
                runs: u32::try_from(runs.len()).unwrap_or(u32::MAX),
            })
            .collect()
    }
}
