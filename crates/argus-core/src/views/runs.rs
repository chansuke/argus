//! Run, lease, and artifact lifecycle. Drives the host: `due` says what to start, abandon,
//! release, or kill. Crash recovery is this projection plus the host's in-memory set of
//! runs it is executing; there is no separate "job queue" state anywhere.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::*;
use crate::fact::Fact;
use crate::ids::*;
use crate::source::SourceFamily;
use crate::views::Projection;

/// Retries after a crash stop here; a human can request a fresh run.
pub const MAX_ATTEMPTS: u16 = 3;

#[derive(Default, Debug)]
pub struct Runs {
    runs: BTreeMap<RunId, RunState>,
    leases: BTreeMap<LeaseId, LeaseState>,
}

#[derive(Debug)]
pub struct RunState {
    pub purpose: RunPurpose,
    pub requested_by: Author,
    pub requested_at: UnixMillis,
    pub attempts: Vec<AttemptState>,
    pub cancel_requested: bool,
    pub sources: Vec<SourceId>,
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
        todo!("not implemented")
    }

    /// TODO, per run:
    /// - no attempts -> Start(0)
    /// - last attempt unfinished and run not in `live` -> Abandon(last)
    /// - last attempt Abandoned, purpose.on_interrupt() == Retry, attempts < MAX -> Start(next)
    /// - cancel_requested and run in `live` -> Kill
    ///
    /// Per lease: unclosed and lease.run not in `live` -> Release.
    /// Deterministic order (BTreeMap), so two calls in a row agree.
    pub fn due(&self, live: &BTreeSet<RunId>) -> Vec<Due> {
        todo!("not implemented")
    }
}

impl Projection for Runs {
    const NAME: &'static str = "runs";
    const VERSION: u32 = 1;
    /// Lifecycle comes from authored events; source ids come from log headers at open.
    const READS: &'static [SourceFamily] = &[];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        // TODO: RunRequested -> insert; RunStarted -> push attempt; RunFinished -> close it;
        //       RunCancelRequested -> flag; Lease* -> leases; ArtifactCaptured -> artifacts.
        todo!("not implemented")
    }
}
