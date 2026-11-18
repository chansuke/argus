//! Run, lease, and artifact lifecycle. Drives the host: `due` says what to start, abandon,
//! release, or kill. Crash recovery is this projection plus the host's in-memory set of
//! runs it is executing; there is no separate "job queue" state anywhere.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::*;
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::SourceFamily;
use crate::views::Projection;

/// Retries after a crash stop here; a human can request a fresh run.
pub const MAX_ATTEMPTS: u16 = 3;

#[derive(Default, Debug)]
pub struct Runs {
    runs: BTreeMap<RunId, RunState>,
    leases: BTreeMap<LeaseId, LeaseState>,
    /// The agent run requested last (request order, not id order).
    latest_agent: Option<RunId>,
}

#[derive(Debug)]
pub struct RunState {
    pub purpose: RunPurpose,
    pub requested_by: Author,
    pub requested_at: UnixMillis,
    pub attempts: Vec<AttemptState>,
    pub cancel_requested: bool,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Debug)]
pub struct AttemptState {
    pub attempt: Attempt,
    pub started_at: UnixMillis,
    pub pid: Option<u32>,
    pub finished: Option<(UnixMillis, RunOutcome)>,
}

#[derive(Clone, Debug)]
pub struct ArtifactRef {
    pub blob: BlobId,
    pub name: String,
    pub media_type: String,
    pub len: u64,
    pub fact: FactId,
}

#[derive(Debug)]
pub struct LeaseState {
    pub run: RunId,
    pub provider: LeaseProvider,
    pub name: String,
    pub remote_id: Option<String>,
    pub preview_url: Option<String>,
    pub closed: Option<LeaseEnd>,
}

/// An action the host owes the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Due {
    Start {
        run: RunId,
        attempt: Attempt,
    },
    /// An attempt that was running when the host died: close it as `Abandoned`.
    Abandon {
        run: RunId,
        attempt: Attempt,
    },
    Kill {
        run: RunId,
    },
    /// An open lease whose run is not executing: stop the remote session (or `find` it
    /// first if it never got bound).
    Release {
        lease: LeaseId,
    },
}

impl Runs {
    pub fn get(&self, run: &RunId) -> Option<&RunState> {
        self.runs.get(run)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RunId, &RunState)> {
        self.runs.iter()
    }
    pub fn lease(&self, lease: &LeaseId) -> Option<&LeaseState> {
        self.leases.get(lease)
    }
    pub fn unclosed_leases(&self) -> impl Iterator<Item = (&LeaseId, &LeaseState)> {
        self.leases.iter().filter(|(_, l)| l.closed.is_none())
    }
    /// The job's most recently requested agent run: what a feedback delivery resumes.
    pub fn latest_agent(&self) -> Option<(&RunId, &RunState)> {
        self.runs.get_key_value(self.latest_agent.as_ref()?)
    }

    /// What the host owes, given the runs it is executing right now (`live`; empty after a
    /// restart). Per run:
    /// - not live, no attempts, not cancelled -> `Start(0)`
    /// - not live, last attempt unfinished -> `Abandon(last)`
    /// - not live, last attempt `Abandoned`, `on_interrupt() == Retry`, fewer than
    ///   [`MAX_ATTEMPTS`], not cancelled -> `Start(next)`
    /// - live, cancel requested, last attempt unfinished -> `Kill`
    ///
    /// Per lease: unclosed and its run not live -> `Release`. Ordered by id, so two calls
    /// in a row agree.
    pub fn due(&self, live: &BTreeSet<RunId>) -> Vec<Due> {
        let mut out = Vec::new();
        for (run, state) in &self.runs {
            let last = state.attempts.last();
            let running = last.is_some_and(|a| a.finished.is_none());
            if live.contains(run) {
                if state.cancel_requested && running {
                    out.push(Due::Kill { run: *run });
                }
                continue;
            }
            match last {
                None if !state.cancel_requested => out.push(Due::Start {
                    run: *run,
                    attempt: Attempt(0),
                }),
                Some(a) if a.finished.is_none() => out.push(Due::Abandon {
                    run: *run,
                    attempt: a.attempt,
                }),
                Some(AttemptState {
                    attempt,
                    finished: Some((_, RunOutcome::Abandoned)),
                    ..
                }) if !state.cancel_requested
                    && state.purpose.on_interrupt() == OnInterrupt::Retry
                    && state.attempts.len() < usize::from(MAX_ATTEMPTS) =>
                {
                    out.push(Due::Start {
                        run: *run,
                        attempt: Attempt(attempt.0 + 1),
                    })
                }
                _ => {}
            }
        }
        out.extend(
            self.unclosed_leases()
                .filter(|(_, l)| !live.contains(&l.run))
                .map(|(lease, _)| Due::Release { lease: *lease }),
        );
        out
    }
}

impl Projection for Runs {
    const NAME: &'static str = "runs";
    const VERSION: u32 = 1;
    /// Lifecycle comes from authored events; source ids come from log headers at open.
    const READS: &'static [SourceFamily] = &[];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        let FactBody::Authored { author, event } = &fact.body else {
            return false;
        };
        let at = fact.at;
        match event {
            Event::RunRequested { run, purpose } => {
                if self.runs.contains_key(run) {
                    return false;
                }
                if let RunPurpose::Agent(_) = purpose {
                    self.latest_agent = Some(*run);
                }
                self.runs.insert(
                    *run,
                    RunState {
                        purpose: purpose.clone(),
                        requested_by: (*author).clone(),
                        requested_at: at,
                        attempts: Vec::new(),
                        cancel_requested: false,
                        artifacts: Vec::new(),
                    },
                );
                true
            }
            Event::RunStarted { run, attempt, pid } => {
                let Some(state) = self.runs.get_mut(run) else {
                    return false;
                };
                if state.attempts.iter().any(|a| a.attempt == *attempt) {
                    return false;
                }
                state.attempts.push(AttemptState {
                    attempt: *attempt,
                    started_at: at,
                    pid: *pid,
                    finished: None,
                });
                true
            }
            Event::RunFinished {
                run,
                attempt,
                outcome,
            } => {
                let found = self
                    .runs
                    .get_mut(run)
                    .and_then(|s| s.attempts.iter_mut().find(|a| a.attempt == *attempt));
                match found {
                    Some(a) if a.finished.is_none() => {
                        a.finished = Some((at, outcome.clone()));
                        true
                    }
                    _ => false,
                }
            }
            Event::RunCancelRequested { run } => match self.runs.get_mut(run) {
                Some(state) if !state.cancel_requested => {
                    state.cancel_requested = true;
                    true
                }
                _ => false,
            },
            Event::LeaseOpened {
                lease,
                run,
                provider,
                name,
                ..
            } => {
                let state = LeaseState {
                    run: *run,
                    provider: provider.clone(),
                    name: name.clone(),
                    remote_id: None,
                    preview_url: None,
                    closed: None,
                };
                self.leases.insert(*lease, state).is_none()
            }
            Event::LeaseBound {
                lease,
                remote_id,
                preview_url,
            } => match self.leases.get_mut(lease) {
                Some(state) => {
                    state.remote_id = Some(remote_id.clone());
                    state.preview_url = preview_url.clone();
                    true
                }
                None => false,
            },
            Event::LeaseClosed { lease, how } => match self.leases.get_mut(lease) {
                Some(state) if state.closed.is_none() => {
                    state.closed = Some(*how);
                    true
                }
                _ => false,
            },
            Event::ArtifactCaptured {
                run,
                blob,
                name,
                media_type,
                len,
                ..
            } => match self.runs.get_mut(run) {
                Some(state) => {
                    state.artifacts.push(ArtifactRef {
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
        }
    }
}
