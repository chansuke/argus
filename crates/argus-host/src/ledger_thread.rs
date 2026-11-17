//! One thread per open ledger, owning it. All writes to a ledger are serialized here, so
//! the ledger's sequencer order is the order views see, live and on replay. Reads also
//! run here, but each is a short fold-state lookup: anything slow (git, LLMs, processes)
//! runs elsewhere and comes back as a command.

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, Sender};

use argus_core::event::{Author, Event};
use argus_core::ids::{RunId, SourceId};
use argus_core::ledger::{JobLedger, LedgerError, SourceCursor, Submitted};
use argus_core::source::{RawFrame, SourceMeta};

use crate::pulse::Dep;

pub(crate) enum Cmd {
    Submit {
        author: Author,
        event: Event,
        reply: Sender<Result<Submitted, LedgerError>>,
    },
    OpenSource {
        meta: SourceMeta,
        reply: Sender<Result<SourceCursor, LedgerError>>,
    },
    Observe {
        source: SourceId,
        frames: Vec<RawFrame>,
    },
    /// Read-only access for API handlers; runs on this thread, so it sees a consistent
    /// state. The thread wraps `f` in `take_reads()` calls and replies with the `Dep`.
    Read(Box<dyn FnOnce(&JobLedger) + Send>),
    /// The runner stopped executing a run (it already submitted `RunFinished`).
    Released {
        run: RunId,
    },
    Flush,
    /// Daily: `prune_source` for Terminal-family sources older than `Retention`.
    Retain,
}

#[derive(Clone)]
pub(crate) struct LedgerHandle {
    tx: Sender<Cmd>,
}

impl LedgerHandle {
    pub(crate) fn submit(&self, author: Author, event: Event) -> Result<Submitted, LedgerError> {
        todo!("not implemented")
    }
    /// Runs `f` on the ledger thread and appends what it read to `deps` (subscriptions
    /// keep them; one-shot calls drop them).
    #[expect(clippy::ptr_arg, reason = "sketch: the body pushes the read's Dep")]
    pub(crate) fn read<T: Send + 'static>(
        &self,
        deps: &mut Vec<Dep>,
        f: impl FnOnce(&JobLedger) -> T + Send + 'static,
    ) -> T {
        todo!("not implemented")
    }
    pub(crate) fn sender(&self) -> Sender<Cmd> {
        self.tx.clone()
    }
}

/// TODO:
/// loop {
///   drain inbox (batch); apply each Cmd to the ledger;
///   if anything changed:
///     for event in policy::decide(ledger.views(), cfg, now): submit(Policy, event)
///     for due in ledger.views().runs().due(&live): match due {
///       Start{..} => { submit(Host, RunStarted); live.insert(run); runner::start(...) }
///       Abandon{..} => submit(Host, RunFinished{ outcome: Abandoned })
///       Kill{..} => runner::kill(run)
///       Release{lease} => lease::release_orphan(...)  (on a worker thread)
///     }
///     registry.changed(LedgerRef::Job(job), &|mask| ledger.views().stamp(mask))
/// }
pub(crate) fn run(ledger: JobLedger, inbox: Receiver<Cmd>, live: BTreeSet<RunId>) {
    todo!("not implemented")
}
