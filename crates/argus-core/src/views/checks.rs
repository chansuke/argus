//! Formal verification (TLA+, Lean) and device checks (EAS Simulators): one result per
//! (target, tree, engine), verdicts derived from captured output on every replay.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::discriminant;

use crate::adapters::{lean, tlc};
use crate::event::{
    CheckRequest, CheckSpec, CheckTarget, Event, RunOutcome, RunPurpose, TlaEngine,
};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{DeviceAction, LeanMessage, Observation, SourceFamily, TlcMessage};
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
    /// The attempt whose output is shown. A retry starts from empty output, so an
    /// abandoned attempt's partial run never mixes into the verdict.
    pub attempt: Option<Attempt>,
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
    /// Verdict kinds are compared, not their text: two engines word a counterexample
    /// differently.
    pub fn disagreements(&self) -> Vec<(&CheckTarget, &TreeOid)> {
        type Seen<'a> =
            BTreeMap<(&'a CheckTarget, &'a TreeOid), Vec<(Option<TlaEngine>, &'a CheckVerdict)>>;
        let mut seen: Seen<'_> = BTreeMap::new();
        for check in self.runs.values() {
            let Some((_, verdict)) = &check.verdict else {
                continue;
            };
            // Every requested target is a key of `latest`, which lets the answer borrow it.
            let Some((target, _)) = self.latest.get_key_value(&check.request.spec.target()) else {
                continue;
            };
            seen.entry((target, &check.request.tree))
                .or_default()
                .push((engine(&check.request.spec), verdict));
        }
        seen.into_iter()
            .filter(|(_, verdicts)| {
                let (first_engine, first) = verdicts[0];
                verdicts.iter().any(|(e, _)| *e != first_engine)
                    && verdicts
                        .iter()
                        .any(|(_, v)| discriminant(*v) != discriminant(first))
            })
            .map(|(key, _)| key)
            .collect()
    }

    /// For each target, the latest run decides:
    /// - `NotConfigured`: no head, or no check was ever requested.
    /// - `Red { open }`: latest runs on `head` without a passing verdict (`Proved` or
    ///   `Passed`). `Holes` is red: an unfinished proof is not a proof.
    /// - `Pending`: one is still running on `head`.
    /// - `Stale`: one judged an older tree.
    /// - `Green` otherwise.
    pub fn gate(&self, head: Option<&TreeOid>) -> GateState {
        let Some(head) = head else {
            return GateState::NotConfigured;
        };
        if self.latest.is_empty() {
            return GateState::NotConfigured;
        }
        let (mut open, mut pending, mut stale) = (0u32, false, false);
        for (_, check) in self.latest() {
            if &check.request.tree != head {
                stale = true;
                continue;
            }
            match &check.verdict {
                None => pending = true,
                Some((_, CheckVerdict::Proved | CheckVerdict::Passed)) => {}
                Some(_) => open += 1,
            }
        }
        if open > 0 {
            GateState::Red { open }
        } else if pending {
            GateState::Pending
        } else if stale {
            GateState::Stale
        } else {
            GateState::Green
        }
    }

    /// Messages with a location, for hunk signals: Lean diagnostics, made repo-relative
    /// with the package root. TLC and device output carry no source location.
    pub fn located(&self, run: &RunId) -> Vec<(RepoPath, LineRange)> {
        let Some(check) = self.runs.get(run) else {
            return Vec::new();
        };
        let (CheckOutput::Lean(messages), CheckSpec::Lean { root, .. }) =
            (&check.output, &check.request.spec)
        else {
            return Vec::new();
        };
        messages
            .iter()
            .filter_map(|(_, m)| {
                let path = RepoPath::parse(&format!("{}/{}", root.as_str(), m.path))?;
                Some((path, LineRange::new(m.line, m.line)?))
            })
            .collect()
    }

    fn finish(&mut self, run: &RunId, at: UnixMillis, outcome: &RunOutcome) -> bool {
        let Some(check) = self.runs.get_mut(run) else {
            return false;
        };
        let verdict = match outcome {
            RunOutcome::Exited { code } => match &check.output {
                CheckOutput::Tlc(messages) => tlc::verdict(messages, *code),
                CheckOutput::Lean(messages) => lean::verdict(messages, *code),
                CheckOutput::Device(_) if *code == 0 => CheckVerdict::Passed,
                CheckOutput::Device(_) => CheckVerdict::ToolError {
                    summary: format!("the device driver exited {code}"),
                },
            },
            RunOutcome::Killed {
                cancelled: true, ..
            } => CheckVerdict::Cancelled,
            RunOutcome::Killed { signal, .. } => CheckVerdict::ToolError {
                summary: format!("killed by signal {signal}"),
            },
            RunOutcome::SpawnFailed { error } => CheckVerdict::ToolError {
                summary: error.clone(),
            },
            RunOutcome::Failed { reason } => CheckVerdict::ToolError {
                summary: reason.clone(),
            },
            // The host died: a retry may follow, so there is no verdict yet.
            RunOutcome::Abandoned => return false,
            other => CheckVerdict::ToolError {
                summary: format!("unexpected outcome for a check: {other:?}"),
            },
        };
        check.verdict = Some((at, verdict));
        true
    }
}

fn engine(spec: &CheckSpec) -> Option<TlaEngine> {
    match spec {
        CheckSpec::Tla { engine, .. } => Some(*engine),
        CheckSpec::Lean { .. } | CheckSpec::Device { .. } => None,
    }
}

fn empty_output(spec: &CheckSpec) -> CheckOutput {
    match spec {
        CheckSpec::Tla { .. } => CheckOutput::Tlc(Vec::new()),
        CheckSpec::Lean { .. } => CheckOutput::Lean(Vec::new()),
        CheckSpec::Device { .. } => CheckOutput::Device(Vec::new()),
    }
}

impl Projection for Checks {
    const NAME: &'static str = "checks";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::Checker, SourceFamily::Device];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        match &fact.body {
            FactBody::Authored { event, .. } => match event {
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Check(request),
                } => {
                    if self.runs.contains_key(run) {
                        return false;
                    }
                    self.latest.insert(request.spec.target(), *run);
                    let check = CheckRun {
                        request: request.clone(),
                        attempt: None,
                        output: empty_output(&request.spec),
                        verdict: None,
                        evidence: Vec::new(),
                    };
                    self.runs.insert(*run, check);
                    true
                }
                Event::RunStarted { run, attempt, .. } => match self.runs.get_mut(run) {
                    Some(check) => {
                        check.attempt = Some(*attempt);
                        check.output = empty_output(&check.request.spec);
                        check.verdict = None;
                        true
                    }
                    None => false,
                },
                Event::RunFinished { run, outcome, .. } => self.finish(run, fact.at, outcome),
                Event::ArtifactCaptured {
                    run,
                    blob,
                    name,
                    media_type,
                    len,
                    ..
                } => match self.runs.get_mut(run) {
                    Some(check) => {
                        check.evidence.push(ArtifactRef {
                            blob: *blob,
                            name: name.clone(),
                            media_type: media_type.clone(),
                            len: *len,
                            fact: fact.id.clone(),
                        });
                        true
                    }
                    None => false,
                },
                _ => false,
            },
            FactBody::Observed { source, obs, .. } => {
                let Some(check) = self.runs.get_mut(&source.run) else {
                    return false;
                };
                if check.attempt != Some(source.attempt) {
                    return false;
                }
                let id = fact.id.clone();
                match (&mut check.output, obs) {
                    (CheckOutput::Tlc(out), Observation::Tlc(m)) => out.push((id, m.clone())),
                    (CheckOutput::Lean(out), Observation::Lean(m)) => out.push((id, m.clone())),
                    (CheckOutput::Device(out), Observation::Device(a)) => out.push((id, a.clone())),
                    // Stray lines stay in the raw log.
                    _ => return false,
                }
                true
            }
        }
    }
}
