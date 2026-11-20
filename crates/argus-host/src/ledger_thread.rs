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

/// Opens (or reopens) a source on the ledger thread and returns where ingestion resumes.
pub(crate) fn open_source(ledger: &Sender<Cmd>, meta: SourceMeta) -> std::io::Result<SourceCursor> {
    let (reply, answer) = std::sync::mpsc::channel();
    let gone = || std::io::Error::other("the ledger thread stopped");
    ledger
        .send(Cmd::OpenSource { meta, reply })
        .map_err(|_| gone())?;
    answer
        .recv()
        .map_err(|_| gone())?
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

/// Hands frames to the ledger thread. A stopped ledger thread means the host is shutting
/// down; the frames are lost with the process, as they would be with a crash.
pub(crate) fn observe(ledger: &Sender<Cmd>, source: SourceId, frames: Vec<RawFrame>) {
    if !frames.is_empty() {
        let _ = ledger.send(Cmd::Observe { source, frames });
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{self, Sender};
    use std::thread::JoinHandle;

    use argus_core::ids::SourceId;
    use argus_core::ledger::SourceCursor;
    use argus_core::source::{RawFrame, SourceMeta};

    use super::Cmd;

    /// What a fake ledger thread received: per source, its header and stored frames.
    pub(crate) type Received = BTreeMap<SourceId, (SourceMeta, Vec<RawFrame>)>;

    /// A ledger thread that opens every source fresh and keeps what it is sent.
    pub(crate) fn fake_ledger() -> (Sender<Cmd>, JoinHandle<Received>) {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut got = Received::new();
            for cmd in rx {
                match cmd {
                    Cmd::OpenSource { meta, reply } => {
                        got.insert(meta.id, (meta.clone(), Vec::new()));
                        let cursor = SourceCursor {
                            meta,
                            resume_at: 0,
                            last: None,
                        };
                        reply.send(Ok(cursor)).unwrap();
                    }
                    Cmd::Observe { source, frames } => {
                        let stored = &mut got.get_mut(&source).expect("observed after open").1;
                        stored.extend(frames);
                    }
                    _ => {}
                }
            }
            got
        });
        (tx, thread)
    }

    /// The bytes of every `Bytes` frame, in order.
    pub(crate) fn bytes(frames: &[RawFrame]) -> Vec<Vec<u8>> {
        frames
            .iter()
            .filter_map(|f| match f {
                RawFrame::Bytes { bytes, .. } => Some(bytes.clone()),
                RawFrame::Discontinuity { .. } => None,
            })
            .collect()
    }
}
