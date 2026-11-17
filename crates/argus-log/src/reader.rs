use std::fs::File;
use std::path::PathBuf;

use crate::{ActorKey, EventId, LogDir, LogError, Seq, Stamp};

/// Lock-free reader over one log. Builds a seq -> byte-offset index on open by walking
/// frame heads (payloads are skipped with a seek), so random access by [`EventId`] for the
/// raw view is one seek + one read.
#[derive(Debug)]
pub struct LogReader {
    path: PathBuf,
    actor: ActorKey,
    header: Vec<u8>,
    /// `offsets[seq]` = byte offset of that frame's head.
    offsets: Vec<u64>,
    last_stamp: Option<Stamp>,
}

/// One stored frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub id: EventId,
    pub stamp: Stamp,
    pub payload: Vec<u8>,
}

impl LogReader {
    pub fn open(dir: &LogDir, actor: &ActorKey) -> Result<Self, LogError> {
        todo!("not implemented")
    }
    /// The bytes given to [`crate::LogWriter::open`] when the log was created.
    pub fn header(&self) -> &[u8] {
        &self.header
    }
    pub fn actor(&self) -> &ActorKey {
        &self.actor
    }
    pub fn len(&self) -> u64 {
        self.offsets.len() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
    pub fn last_stamp(&self) -> Option<Stamp> {
        self.last_stamp
    }
    pub fn get(&self, seq: Seq) -> Result<Frame, LogError> {
        todo!("not implemented")
    }
    /// Sequential iteration starting at `from` (inclusive).
    pub fn iter_from(&self, from: Seq) -> Result<FrameIter, LogError> {
        todo!("not implemented")
    }
    /// Extends the index with frames appended since open. Returns how many were added.
    pub fn refresh(&mut self) -> Result<u64, LogError> {
        todo!("not implemented")
    }
}

/// Buffered sequential reader. Ends silently at a torn tail.
#[derive(Debug)]
pub struct FrameIter {
    file: File,
    actor: ActorKey,
    next: Seq,
    end: Seq,
}

impl FrameIter {
    /// The stamp of the next frame without reading its payload; drives the k-way merge.
    pub fn peek_stamp(&mut self) -> Result<Option<Stamp>, LogError> {
        todo!("not implemented")
    }
}

impl Iterator for FrameIter {
    type Item = Result<Frame, LogError>;
    fn next(&mut self) -> Option<Self::Item> {
        todo!("not implemented")
    }
}
