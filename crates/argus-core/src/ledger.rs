//! The ledger: one directory of per-actor logs with one sequencer, plus the live views
//! folded over it. The only way to write facts and the only way to read state.
//!
//! Hidden behind `submit` / `observe` / `views`: actor-to-file mapping, framing, stamps,
//! authorship and scope validation, view-dependent admission, submission dedup, record
//! dedup, fsync policy, decoding, cursor bookkeeping, and fan-out to every view.

use std::collections::BTreeMap;
use std::path::Path;

use argus_log::{ActorKey, BlobStore, Clock, Cursor, LogDir, LogError, LogWriter};

use crate::event::{self, Author, DedupKey, Event, Rejected};
use crate::fact::Fact;
use crate::ids::*;
use crate::source::{Decoded, Observation, RawFrame, SourceMeta};
use crate::views::{JobViews, MetaViews, Projection, ViewSet};

pub type JobLedger = Ledger<JobViews>;
pub type MetaLedger = Ledger<MetaViews>;

pub struct Ledger<V: ViewSet> {
    dir: LogDir,
    clock: Clock,
    writers: BTreeMap<ActorKey, LogWriter>,
    sources: BTreeMap<SourceId, SourceMeta>,
    applied: Applied,
    /// Submission identities already stored -> where. Ordered index, O(log N) per check.
    seen: BTreeMap<DedupKey, EventId>,
    views: V,
}

/// What has been applied: every frame once (`Cursor`), and every agent record once.
///
/// The record rule, stated only here: an agent record key (Claude `uuid`) is applied once
/// per ledger. The first frame that carries it owns it. A later frame carrying the same
/// key (a `--resume` that copies history into a new file, a transcript re-read after a
/// `RawFrame::Discontinuity`, a subagent file seen twice) is stored and visible in the raw
/// view, but its observations never reach a projection. Live apply, replay, and
/// checkpoints all carry this state, so every view counts each record exactly once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub frames: Cursor,
    records: BTreeMap<RecordKey, EventId>,
    repeats: u64,
}

impl Applied {
    /// Whether `obs`, decoded from `frame`, may reach projections. O(log R).
    pub(crate) fn admit_record(&mut self, frame: &EventId, obs: &Observation) -> bool {
        let Observation::Agent(ev) = obs else {
            return true;
        };
        let Some(key) = &ev.record else { return true };
        match self.records.get(key) {
            None => {
                self.records.insert(key.clone(), frame.clone());
                true
            }
            Some(owner) if owner == frame => true,
            Some(_) => {
                self.repeats += 1;
                false
            }
        }
    }
    /// Observations dropped as repeats (shown as a count next to the transcript).
    pub fn repeats(&self) -> u64 {
        self.repeats
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Submitted {
    Appended(EventId),
    /// Same identity already stored (a retried submission). Nothing was written.
    Duplicate(EventId),
}

/// Where ingestion of a source resumes (see `RawFrame` for the rule), plus the last stored
/// bytes, which the tailer compares with the file to detect a rewrite.
#[derive(Debug)]
pub struct SourceCursor {
    pub meta: SourceMeta,
    pub resume_at: u64,
    pub last: Option<(u64, Vec<u8>)>,
}

/// A stored frame with its verbatim bytes and argus's reading of them, side by side.
#[derive(Debug)]
pub struct RawRecord {
    pub id: EventId,
    pub stamp: Stamp,
    pub bytes: Vec<u8>,
    pub reading: Reading,
}

#[derive(Debug)]
pub enum Reading {
    Authored {
        author: Author,
        event: Event,
    },
    /// `repeat`: a record already applied from another frame (`Applied`), shown, not applied.
    Observed {
        source: SourceMeta,
        decoded: Decoded,
        repeat: bool,
    },
}

#[derive(Debug)]
pub enum LedgerError {
    Log(LogError),
    Rejected(Rejected),
    UnknownSource(SourceId),
    /// Retention removed this source's bytes (`Event::SourcePruned`).
    Pruned(SourceId),
    /// A gap in an actor's seqs or an undecodable authored payload: the store is damaged.
    Corrupt(String),
}

impl From<LogError> for LedgerError {
    fn from(e: LogError) -> Self {
        LedgerError::Log(e)
    }
}

impl<V: ViewSet> Ledger<V> {
    /// Opens (creating if absent) and takes every actor's writer lock.
    ///
    /// TODO:
    /// 1. for each actor in `dir.actors()`: `LogWriter::open` (lock + torn-tail repair);
    ///    parse its header into `ActorHeader`; register sources.
    /// 2. `clock = Clock::starting_after(max last stamp over all logs)`.
    /// 3. `fold(dir, Applied::default(), V::wants, |fact| views.apply(fact))`, also
    ///    rebuilding `seen` from authored events' `dedup_key`.
    pub fn open(dir: &Path) -> Result<Self, LedgerError> {
        todo!("not implemented")
    }

    /// Validates, dedups, appends, applies. The one write path for authored events.
    ///
    /// TODO:
    /// 1. `event.check(&author, V::KIND)?` (static authorship and scope).
    /// 2. if `dedup_key` is in `seen` -> `Duplicate(existing)` (before admission: a retried
    ///    decision whose head has since moved still reports its original success).
    /// 3. `self.views.admit(&author, &event)?` (head-only decisions, current approval).
    ///    Runs on the sequencer, so check and append are atomic.
    /// 4. sync every dirty source log first (anything this event refers to is durable
    ///    before the event is).
    /// 5. stamp = clock.tick(); writer(author).append(stamp, encode(event)); sync.
    /// 6. applied.frames.admit; views.apply(&fact); record in `seen`.
    pub fn submit(&mut self, author: Author, event: Event) -> Result<Submitted, LedgerError> {
        todo!("not implemented")
    }

    /// Creates or reopens a source log. Reopen requires an identical header.
    pub fn open_source(&mut self, meta: SourceMeta) -> Result<SourceCursor, LedgerError> {
        todo!("not implemented")
    }

    /// Appends raw frames verbatim, decodes them, applies the observations that pass
    /// `Applied::admit_record`. No fsync here: a lost unsynced tail is re-read from the
    /// source file on restart (resume point = last durable frame), except for pipes and
    /// PTYs, which the host flushes on a short timer via [`Ledger::flush`].
    pub fn observe(
        &mut self,
        source: SourceId,
        frames: Vec<RawFrame>,
    ) -> Result<Vec<EventId>, LedgerError> {
        todo!("not implemented")
    }

    pub fn flush(&mut self) -> Result<(), LedgerError> {
        todo!("not implemented")
    }

    /// Retention: submits `SourcePruned` (Host), then drops the writer and deletes the
    /// whole log file. Never a partial truncation. Only for families no projection reads
    /// (terminal bytes), so replay and checkpoints are unaffected.
    pub fn prune_source(&mut self, source: SourceId) -> Result<(), LedgerError> {
        todo!("not implemented")
    }

    pub fn views(&self) -> &V {
        &self.views
    }
    pub fn applied(&self) -> &Applied {
        &self.applied
    }
    pub fn source(&self, id: SourceId) -> Option<&SourceMeta> {
        self.sources.get(&id)
    }

    /// Any stored frame, verbatim, with its reading. Authored events are shown as their
    /// stored JSON: the raw view has no "summarized" layer anywhere.
    pub fn raw(&self, id: &EventId) -> Result<RawRecord, LedgerError> {
        todo!("not implemented")
    }

    /// A page of one source in source order (the per-session raw log view).
    pub fn raw_page(
        &self,
        source: SourceId,
        from: Seq,
        limit: usize,
    ) -> Result<Vec<RawRecord>, LedgerError> {
        todo!("not implemented")
    }

    pub fn blobs(&self) -> BlobStore {
        self.dir.blobs()
    }
}

/// Folds one projection over a ledger directory without locking it (read-only; safe while
/// a host is writing). Starts from a checkpoint when given one. Used for store-wide
/// analytics and offline tools; the live host uses `Ledger`.
pub fn replay<P: Projection>(
    dir: &Path,
    start: Option<(P, Applied)>,
) -> Result<(P, Applied), LedgerError> {
    todo!("not implemented")
}

/// Shared by `Ledger::open` and `replay`.
///
/// TODO:
/// - readers = every actor log whose header is an author, or a source with `wants(meta)`.
/// - for frame in merge_by_stamp(readers): match applied.frames.admit(&frame.id) {
///   Seen => continue, Gap{..} => Err(Corrupt), Fresh => {} }
/// - authored -> one Fact; source -> `RawFrame::decode`: `Bytes` ->
///   `meta.kind.decode(bytes)`, one Fact per observation that passes
///   `applied.admit_record` (`part` = index); `Discontinuity` -> one Fact whose
///   observation is `Observation::Discontinuity` (raw = empty).
fn fold(
    dir: &LogDir,
    applied: Applied,
    wants: fn(&SourceMeta) -> bool,
    apply: &mut dyn FnMut(&Fact<'_>),
) -> Result<Applied, LedgerError> {
    todo!("not implemented")
}

/// Header of every log; decides how its frames are read.
enum ActorHeader {
    Author(Author),
    Source(SourceMeta),
}

/// `human`, `host`, `policy`, `agent-<run>`, `src-<source>`. The only place that maps
/// domain actors to files.
fn actor_key(header: &ActorHeader) -> ActorKey {
    todo!("not implemented")
}
