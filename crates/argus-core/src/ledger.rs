//! The ledger: one directory of per-actor logs with one sequencer, plus the live views
//! folded over it. The only way to write facts and the only way to read state.
//!
//! Hidden behind `submit` / `observe` / `views`: actor-to-file mapping, framing, stamps,
//! authorship and scope validation, view-dependent admission, submission dedup, record
//! dedup, fsync policy, decoding, cursor bookkeeping, and fan-out to every view.

use std::collections::BTreeMap;
use std::path::Path;

use argus_log::{
    ActorKey, Admit, BlobStore, Clock, Cursor, LogDir, LogError, LogReader, LogWriter,
    merge_by_stamp,
};

use crate::codec;
use crate::event::{self, Author, DedupKey, Event, Rejected};
use crate::fact::{Fact, FactBody};
use crate::ids::*;
use crate::source::{AgentEvent, Decoded, Observation, RawFrame, SourceMeta};
use crate::views::{JobViews, MetaViews, Projection, ViewSet, reads};

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
    /// Whether `obs`, read again from `frame` (the raw view), was dropped as a repeat.
    fn is_repeat(&self, frame: &EventId, obs: &Observation) -> bool {
        let Observation::Agent(AgentEvent {
            record: Some(key), ..
        }) = obs
        else {
            return false;
        };
        self.records.get(key).is_some_and(|owner| owner != frame)
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
    /// Each log is opened for writing first (lock, torn-tail repair), then all of them are
    /// folded into fresh views, which also rebuilds the submission dedup index. The host
    /// log is always opened, so a second host fails here even on an empty store.
    pub fn open(dir: &Path) -> Result<Self, LedgerError> {
        let dir = LogDir::new(dir);
        let mut writers = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last = None;
        let host = ActorHeader::Author(Author::Host);
        let mut actors = dir.actors()?;
        if !actors.contains(&actor_key(&host)) {
            actors.push(actor_key(&host));
        }
        for actor in actors {
            let header = match LogReader::open(&dir, &actor) {
                Ok(reader) => parse_header(&actor, reader.header())?,
                Err(LogError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => host.clone(),
                Err(e) => return Err(e.into()),
            };
            let (writer, recovery) = LogWriter::open(&dir, &actor, &codec::encode_header(&header))?;
            last = last.max(recovery.last.map(|(_, stamp)| stamp));
            if let ActorHeader::Source(meta) = header {
                sources.insert(meta.id, meta);
            }
            writers.insert(actor, writer);
        }

        let mut views = V::default();
        let mut seen = BTreeMap::new();
        let applied = fold(&dir, Applied::default(), V::wants, &mut |fact| {
            if let FactBody::Authored { event, .. } = fact.body
                && let Some(key) = event.dedup_key()
            {
                seen.entry(key).or_insert_with(|| fact.id.event.clone());
            }
            views.apply(fact);
        })?;
        Ok(Ledger {
            dir,
            clock: Clock::starting_after(last),
            writers,
            sources,
            applied,
            seen,
            views,
        })
    }

    /// Validates, dedups, appends, applies. The one write path for authored events.
    ///
    /// The dedup check comes before admission: a retried decision whose head has since
    /// moved still reports its original success. Admission runs on the sequencer right
    /// before the append, so check and append are atomic. Dirty source logs are synced
    /// first, so anything this event refers to is durable before the event is.
    pub fn submit(&mut self, author: Author, event: Event) -> Result<Submitted, LedgerError> {
        event
            .check(&author, V::KIND)
            .map_err(LedgerError::Rejected)?;
        let key = event.dedup_key();
        if let Some(existing) = key.as_ref().and_then(|k| self.seen.get(k)) {
            return Ok(Submitted::Duplicate(existing.clone()));
        }
        self.views
            .admit(&author, &event)
            .map_err(LedgerError::Rejected)?;
        self.flush()?;

        let header = ActorHeader::Author(author);
        let actor = actor_key(&header);
        if !self.writers.contains_key(&actor) {
            let (writer, _) = LogWriter::open(&self.dir, &actor, &codec::encode_header(&header))?;
            self.writers.insert(actor.clone(), writer);
        }
        let writer = self.writers.get_mut(&actor).unwrap();
        let stamp = self.clock.tick();
        let payload = event::encode(&event);
        let seq = writer.append(stamp, &payload)?;
        writer.sync()?;
        let id = EventId { actor, seq };
        self.apply_live(&header, &id, stamp, &payload)?;
        if let Some(key) = key {
            self.seen.insert(key, id.clone());
        }
        Ok(Submitted::Appended(id))
    }

    /// Creates or reopens a source log. Reopen requires an identical header.
    pub fn open_source(&mut self, meta: SourceMeta) -> Result<SourceCursor, LedgerError> {
        let header = ActorHeader::Source(meta.clone());
        let actor = actor_key(&header);
        match self.sources.get(&meta.id) {
            Some(known) if *known != meta => {
                let path = self.dir.root().join(format!("{}.alog", actor.as_str()));
                return Err(LedgerError::Log(LogError::HeaderMismatch(path)));
            }
            Some(_) => {}
            None => {
                let (writer, _) =
                    LogWriter::open(&self.dir, &actor, &codec::encode_header(&header))?;
                self.writers.insert(actor.clone(), writer);
                self.sources.insert(meta.id, meta.clone());
            }
        }
        // The resume point is in the log itself: the newest frame says where it ended.
        self.flush()?;
        let reader = LogReader::open(&self.dir, &actor)?;
        let mut cursor = SourceCursor {
            meta,
            resume_at: 0,
            last: None,
        };
        if let Some(seq) = reader.seqs().next_back() {
            let frame = reader.get(seq)?;
            if let Some(RawFrame::Bytes { offset, bytes }) = RawFrame::decode(&frame.payload) {
                cursor.resume_at = offset + bytes.len() as u64;
                cursor.last = Some((offset, bytes));
            }
        }
        Ok(cursor)
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
        let meta = self
            .sources
            .get(&source)
            .cloned()
            .ok_or(LedgerError::UnknownSource(source))?;
        let wanted = V::wants(&meta);
        let header = ActorHeader::Source(meta);
        let actor = actor_key(&header);
        let mut ids = Vec::with_capacity(frames.len());
        for frame in frames {
            let payload = frame.encode();
            let stamp = self.clock.tick();
            let writer = self.writers.get_mut(&actor).unwrap();
            let id = EventId {
                actor: actor.clone(),
                seq: writer.append(stamp, &payload)?,
            };
            // Replay does not read logs the views do not want; live skips them the same way.
            if wanted {
                self.apply_live(&header, &id, stamp, &payload)?;
            }
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn flush(&mut self) -> Result<(), LedgerError> {
        for writer in self.writers.values_mut() {
            writer.sync()?;
        }
        Ok(())
    }

    /// Retention: submits `SourcePruned` (Host), then drops the writer and deletes the
    /// whole log file. Never a partial truncation. Only for families no projection reads
    /// (terminal bytes), so replay and checkpoints are unaffected.
    pub fn prune_source(&mut self, source: SourceId) -> Result<(), LedgerError> {
        let meta = self
            .sources
            .get(&source)
            .cloned()
            .ok_or(LedgerError::UnknownSource(source))?;
        self.submit(Author::Host, Event::SourcePruned { source })?;
        let actor = actor_key(&ActorHeader::Source(meta));
        self.writers.remove(&actor);
        self.sources.remove(&source);
        let path = self.dir.root().join(format!("{}.alog", actor.as_str()));
        std::fs::remove_file(path).map_err(LogError::from)?;
        Ok(())
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
        let reader = self.reader(&id.actor)?;
        let header = parse_header(&id.actor, reader.header())?;
        let frame = reader.get(id.seq)?;
        self.record(&header, frame.id, frame.stamp, frame.payload)
    }

    /// A page of one source in source order (the per-session raw log view).
    pub fn raw_page(
        &self,
        source: SourceId,
        from: Seq,
        limit: usize,
    ) -> Result<Vec<RawRecord>, LedgerError> {
        let meta = self
            .sources
            .get(&source)
            .ok_or(LedgerError::UnknownSource(source))?;
        let header = ActorHeader::Source(meta.clone());
        let reader = self.reader(&actor_key(&header))?;
        reader
            .iter_from(from)?
            .take(limit)
            .map(|frame| {
                let frame = frame?;
                self.record(&header, frame.id, frame.stamp, frame.payload)
            })
            .collect()
    }

    pub fn blobs(&self) -> BlobStore {
        self.dir.blobs()
    }

    fn apply_live(
        &mut self,
        header: &ActorHeader,
        id: &EventId,
        stamp: Stamp,
        payload: &[u8],
    ) -> Result<(), LedgerError> {
        if self.applied.frames.admit(id) != Admit::Fresh {
            return Err(LedgerError::Corrupt(format!("{id} was not the next frame")));
        }
        let views = &mut self.views;
        apply_frame(&mut self.applied, header, id, stamp, payload, &mut |fact| {
            views.apply(fact)
        })
    }

    /// A log reader, or `Pruned` for a source whose bytes retention removed.
    fn reader(&self, actor: &ActorKey) -> Result<LogReader, LedgerError> {
        match LogReader::open(&self.dir, actor) {
            Err(LogError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                let pruned = actor
                    .as_str()
                    .strip_prefix("src-")
                    .and_then(Id128::parse)
                    .map(SourceId);
                Err(pruned.map_or(LedgerError::Log(LogError::Io(e)), LedgerError::Pruned))
            }
            other => Ok(other?),
        }
    }

    fn record(
        &self,
        header: &ActorHeader,
        id: EventId,
        stamp: Stamp,
        payload: Vec<u8>,
    ) -> Result<RawRecord, LedgerError> {
        let (bytes, reading) = match header {
            ActorHeader::Author(author) => {
                let event = event::decode(&payload).map_err(|e| corrupt(&id, &e.what))?;
                let reading = Reading::Authored {
                    author: author.clone(),
                    event,
                };
                (payload, reading)
            }
            ActorHeader::Source(meta) => {
                let (bytes, decoded) = match RawFrame::decode(&payload) {
                    Some(RawFrame::Bytes { bytes, .. }) => {
                        let decoded = meta.kind.decode(&bytes);
                        (bytes, decoded)
                    }
                    Some(RawFrame::Discontinuity {
                        stored_end,
                        file_len,
                    }) => (
                        Vec::new(),
                        Decoded::one(Observation::Discontinuity {
                            stored_end,
                            file_len,
                        }),
                    ),
                    None => return Err(corrupt(&id, "not a raw frame")),
                };
                let repeat = decoded.iter().any(|obs| self.applied.is_repeat(&id, obs));
                let reading = Reading::Observed {
                    source: meta.clone(),
                    decoded,
                    repeat,
                };
                (bytes, reading)
            }
        };
        Ok(RawRecord {
            id,
            stamp,
            bytes,
            reading,
        })
    }
}

/// Folds one projection over a ledger directory without locking it (read-only; safe while
/// a host is writing). Starts from a checkpoint when given one. Used for store-wide
/// analytics and offline tools; the live host uses `Ledger`.
pub fn replay<P: Projection>(
    dir: &Path,
    start: Option<(P, Applied)>,
) -> Result<(P, Applied), LedgerError> {
    let (mut projection, applied) = start.unwrap_or_default();
    let applied = fold(&LogDir::new(dir), applied, reads::<P>, &mut |fact| {
        projection.apply(fact);
    })?;
    Ok((projection, applied))
}

/// Shared by `Ledger::open` and `replay`: every authored log and every wanted source log,
/// merged by stamp, each frame once (`Applied::frames`), each record once.
fn fold(
    dir: &LogDir,
    mut applied: Applied,
    wants: fn(&SourceMeta) -> bool,
    apply: &mut dyn FnMut(&Fact<'_>),
) -> Result<Applied, LedgerError> {
    let mut headers = BTreeMap::new();
    let mut iters = Vec::new();
    for actor in dir.actors()? {
        let reader = LogReader::open(dir, &actor)?;
        let header = parse_header(&actor, reader.header())?;
        if let ActorHeader::Source(meta) = &header
            && !wants(meta)
        {
            continue;
        }
        iters.push(reader.iter_from(applied.frames.next_for(&actor))?);
        headers.insert(actor, header);
    }
    for frame in merge_by_stamp(iters) {
        let frame = frame?;
        match applied.frames.admit(&frame.id) {
            Admit::Seen => continue,
            Admit::Gap { expected } => {
                return Err(corrupt(
                    &frame.id,
                    &format!("expected seq {}", expected.get()),
                ));
            }
            Admit::Fresh => {}
        }
        let header = &headers[&frame.id.actor];
        apply_frame(
            &mut applied,
            header,
            &frame.id,
            frame.stamp,
            &frame.payload,
            apply,
        )?;
    }
    Ok(applied)
}

/// One stored frame to facts: the one path both live and replay take, so they agree.
fn apply_frame(
    applied: &mut Applied,
    header: &ActorHeader,
    id: &EventId,
    stamp: Stamp,
    payload: &[u8],
    apply: &mut dyn FnMut(&Fact<'_>),
) -> Result<(), LedgerError> {
    let wall = UnixMillis((stamp.wall_ns / 1_000_000) as i64);
    let fact_id = |part: usize| FactId {
        event: id.clone(),
        part: part as u16,
    };
    match header {
        ActorHeader::Author(author) => {
            let event = event::decode(payload).map_err(|e| corrupt(id, &e.what))?;
            apply(&Fact {
                id: fact_id(0),
                stamp,
                at: wall,
                body: FactBody::Authored {
                    author,
                    event: &event,
                },
            });
        }
        ActorHeader::Source(meta) => match RawFrame::decode(payload) {
            Some(RawFrame::Bytes { bytes, .. }) => {
                let decoded = meta.kind.decode(&bytes);
                for (part, obs) in decoded.iter().enumerate() {
                    if !applied.admit_record(id, obs) {
                        continue;
                    }
                    let at = match obs {
                        Observation::Agent(AgentEvent { at: Some(at), .. }) => *at,
                        _ => wall,
                    };
                    apply(&Fact {
                        id: fact_id(part),
                        stamp,
                        at,
                        body: FactBody::Observed {
                            source: meta,
                            obs,
                            raw: &bytes,
                        },
                    });
                }
            }
            Some(RawFrame::Discontinuity {
                stored_end,
                file_len,
            }) => apply(&Fact {
                id: fact_id(0),
                stamp,
                at: wall,
                body: FactBody::Observed {
                    source: meta,
                    obs: &Observation::Discontinuity {
                        stored_end,
                        file_len,
                    },
                    raw: &[],
                },
            }),
            None => return Err(corrupt(id, "not a raw frame")),
        },
    }
    Ok(())
}

fn corrupt(id: &EventId, what: &str) -> LedgerError {
    LedgerError::Corrupt(format!("{id}: {what}"))
}

/// Header of every log; decides how its frames are read.
#[derive(Clone, Debug)]
pub(crate) enum ActorHeader {
    Author(Author),
    Source(SourceMeta),
}

/// A log's header, checked against the file it came from.
fn parse_header(actor: &ActorKey, bytes: &[u8]) -> Result<ActorHeader, LedgerError> {
    let header = codec::decode_header(bytes)
        .map_err(|e| LedgerError::Corrupt(format!("{actor:?}: header: {}", e.what)))?;
    if actor_key(&header) != *actor {
        return Err(LedgerError::Corrupt(format!(
            "{actor:?}: header names another log"
        )));
    }
    Ok(header)
}

/// `human`, `host`, `policy`, `agent-<run>`, `src-<source>`. The only place that maps
/// domain actors to files.
fn actor_key(header: &ActorHeader) -> ActorKey {
    let key = match header {
        ActorHeader::Author(Author::Human) => "human".to_owned(),
        ActorHeader::Author(Author::Host) => "host".to_owned(),
        ActorHeader::Author(Author::Policy) => "policy".to_owned(),
        ActorHeader::Author(Author::Agent(run)) => format!("agent-{}", run.0),
        ActorHeader::Source(meta) => format!("src-{}", meta.id.0),
    };
    ActorKey::new(&key).expect("lowercase UUIDs and fixed names are valid actor keys")
}
