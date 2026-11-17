//! Formal verification (TLA+, Lean) and device checks (EAS Simulators): one result per
//! (target, tree, engine), verdicts derived from captured output on every replay.

use std::collections::BTreeMap;

use crate::event::{CheckRequest, CheckTarget};
use crate::fact::Fact;
use crate::ids::*;
use crate::source::{DeviceAction, LeanMessage, SourceFamily, TlcMessage};
use crate::views::runs::ArtifactRef;
use crate::views::{GateState, Projection};

#[derive(Default, Debug)]
pub struct Checks {
    runs: BTreeMap<RunId, CheckRun>,
    /// Latest run per target, for the gate.
    latest: BTreeMap<CheckTarget, RunId>,
}

#[derive(Debug)]
pub struct CheckRun {
    pub request: CheckRequest,
    pub output: CheckOutput,
    pub verdict: Option<(UnixMillis, CheckVerdict)>,
    /// Screenshots, recordings, logs, trace dumps: evidence shown next to the review.
    pub evidence: Vec<ArtifactRef>,
}

#[derive(Debug)]
pub enum CheckOutput {
    Tlc(Vec<(FactId, TlcMessage)>),
    Lean(Vec<(FactId, LeanMessage)>),
    Device(Vec<(FactId, DeviceAction)>),
}

#[derive(Clone, PartialEq, Debug)]
pub enum CheckVerdict {
    Proved,
    /// Builds, but proofs use `sorry`: never displayed as proved.
    Holes {
        sorries: u32,
    },
    Refuted {
        summary: String,
        trace: Vec<FactId>,
    },
    /// Device driver exited 0 (scripted assertions held / agent reported success).
    Passed,
    ToolError {
        summary: String,
    },
    Cancelled,
}

impl Checks {
    pub fn run(&self, run: &RunId) -> Option<&CheckRun> {
        self.runs.get(run)
    }
    pub fn latest(&self) -> impl Iterator<Item = (&CheckTarget, &CheckRun)> {
        self.latest
            .iter()
            .filter_map(|(t, r)| self.runs.get(r).map(|c| (t, c)))
    }
    /// Same target and tree, different engines, different verdicts (TLC vs tlars).
    pub fn disagreements(&self) -> Vec<(&CheckTarget, &TreeOid)> {
        todo!("not implemented")
    }
    pub fn gate(&self, head: Option<&TreeOid>) -> GateState {
        todo!("not implemented")
    }
    /// Messages with a location (Lean diagnostics, device steps), for hunk signals.
    pub fn located(&self, run: &RunId) -> Vec<(RepoPath, LineRange)> {
        todo!("not implemented")
    }
}

impl Projection for Checks {
    const NAME: &'static str = "checks";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::Checker, SourceFamily::Device];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO: RunRequested(Check) -> runs; observations from its sources -> output;
        //       ArtifactCaptured -> evidence; RunFinished -> verdict via
        //       adapters::{tlc,lean}::verdict(messages, exit) or Passed/ToolError for device.
        todo!("not implemented")
    }
}
