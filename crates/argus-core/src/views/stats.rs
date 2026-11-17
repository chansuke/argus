//! Conversation analytics. Per job: hourly counters per agent run (`JobStats`, a
//! checkpointable projection). Store-wide: `Analytics` = merge of every job's `JobStats`
//! (sums keyed by job/run, so merge order does not matter).

use std::collections::{BTreeMap, BTreeSet};

use crate::fact::Fact;
use crate::ids::*;
use crate::models::AgentCli;
use crate::period::{HourBucket, Period};
use crate::source::SourceFamily;
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
}

impl Projection for JobStats {
    const NAME: &'static str = "stats";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentSession];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO: bucket = HourBucket::of(fact.at); bump counters by observation kind.
        //       Repeated records never arrive (`ledger::Applied`), so nothing is skipped here.
        todo!("not implemented")
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
