//! Append-only, single-writer, per-actor logs, the read-time merge across them, and a
//! content-addressed blob store for whole immutable files.
//!
//! This crate does not know what the payload bytes mean. It owns these invariants:
//!
//! 1. **One writer per log file, ever.** [`LogWriter::open`] takes an exclusive OS lock
//!    (`File::try_lock`) and fails if another process holds it. A second host on the
//!    same store cannot start.
//! 2. **`Seq` is dense and equals the frame's index in its file.** A frame's identity is
//!    `(ActorKey, Seq)`. No id is ever minted, so no id can be duplicated.
//! 3. **Stamps strictly increase within a file.** The ledger's sequencer guarantees it and
//!    the writer rejects violations.
//! 4. **A torn tail (crash mid-append) is never surfaced.** Length + CRC detect it;
//!    readers stop before it and the writer truncates it on open.
//! 5. **Blobs are immutable and named by content.** Writing the same bytes twice is a no-op.
//!
//! Readers need no lock: they only ever see whole frames, so a viewer process can read a
//! store while the host writes it.

#![allow(dead_code, unused_variables)]

mod blob;
mod frame;
mod merge;
mod reader;
mod stamp;
mod writer;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use blob::{BlobId, BlobStore};
pub use merge::{Admit, Cursor, MergedFrames, merge_by_stamp};
pub use reader::{Frame, FrameIter, LogReader};
pub use stamp::{Clock, Stamp};
pub use writer::{LogWriter, Recovery};

/// Names one log inside a [`LogDir`]. Also its file stem, so the alphabet is restricted
/// to `[a-z0-9-]`, which keeps the key safe as a filename and stable when encoded.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorKey(Arc<str>);

impl ActorKey {
    pub fn new(key: &str) -> Result<Self, LogError> {
        let ok = !key.is_empty()
            && key.len() <= 96
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(Self(Arc::from(key)))
        } else {
            Err(LogError::InvalidActorKey(key.to_owned()))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ActorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Position of a frame in its log. Assigned by the writer; dense from 0.
///
/// Outside this crate a `Seq` comes only from the writer (`LogWriter::append`), a reader,
/// or [`EventId::parse`] of a stored reference. A parsed reference may name a frame that
/// does not exist; lookups then fail with `NotFound`. Nothing outside mints a position.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Seq(u64);

impl Seq {
    pub const FIRST: Seq = Seq(0);
    pub(crate) fn from_persisted(n: u64) -> Self {
        Seq(n)
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn next(self) -> Seq {
        Seq(self.0 + 1)
    }
}

/// Identity of one stored frame. Unique by construction (invariant 2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EventId {
    pub actor: ActorKey,
    pub seq: Seq,
}

impl EventId {
    /// The one text form of a frame reference, `<actor>:<seq>` (stored payloads, the API).
    pub fn parse(s: &str) -> Option<Self> {
        let (actor, seq) = s.rsplit_once(':')?;
        Some(EventId {
            actor: ActorKey::new(actor).ok()?,
            seq: Seq::from_persisted(seq.parse().ok()?),
        })
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.actor.as_str(), self.seq.0)
    }
}

/// A directory holding one `<actor>.alog` per actor, plus `blobs/`.
#[derive(Clone, Debug)]
pub struct LogDir {
    root: PathBuf,
}

impl LogDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Every actor that has a log here, in key order.
    pub fn actors(&self) -> Result<Vec<ActorKey>, LogError> {
        // TODO: read_dir, keep `*.alog`, parse stems with ActorKey::new, sort.
        todo!("not implemented")
    }
    pub fn blobs(&self) -> BlobStore {
        BlobStore::at(self.root.join("blobs"))
    }
    fn log_path(&self, actor: &ActorKey) -> PathBuf {
        self.root.join(format!("{}.alog", actor.as_str()))
    }
}

#[derive(Debug)]
pub enum LogError {
    Io(std::io::Error),
    InvalidActorKey(String),
    /// Another process holds the writer lock (invariant 1).
    Locked(PathBuf),
    /// The file exists but was created for a different header (a different actor or source).
    HeaderMismatch(PathBuf),
    NotALog(PathBuf),
    NonMonotonicStamp {
        last: Stamp,
        given: Stamp,
    },
    /// A frame's embedded seq disagrees with its position: files were concatenated or edited.
    Corrupt {
        path: PathBuf,
        offset: u64,
    },
    /// A parsed reference names a frame past the end of its log.
    NotFound(EventId),
}

impl From<std::io::Error> for LogError {
    fn from(e: std::io::Error) -> Self {
        LogError::Io(e)
    }
}
