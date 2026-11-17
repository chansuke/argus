use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use crate::frame::{self, FRAME_HEAD, FileHead, FrameHead};
use crate::{ActorKey, EventId, LogDir, LogError, Seq, Stamp};

/// Lock-free reader over one log. Builds a seq -> byte-offset index on open by walking
/// the frames (checking each CRC), so random access by [`EventId`] for the raw view is
/// one positioned read.
#[derive(Debug)]
pub struct LogReader {
    path: PathBuf,
    file: File,
    actor: ActorKey,
    header: Vec<u8>,
    /// `offsets[seq]` = byte offset of that frame's head.
    offsets: Vec<u64>,
    /// End of the last indexed frame; [`LogReader::refresh`] continues from here.
    end: u64,
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
        let path = dir.log_path(actor);
        let mut file = File::open(&path)?;
        // The writer creates the head in a single write, so a torn head is a log whose
        // creation crashed, not one being written right now.
        let FileHead::Whole { header, end } = frame::read_file_head(&mut file, &path)? else {
            return Err(LogError::NotALog(path));
        };
        let scan = frame::scan(&mut file, end, Seq::FIRST, None)?;
        Ok(LogReader {
            path,
            file,
            actor: actor.clone(),
            header,
            offsets: scan.offsets,
            end: scan.end,
            last_stamp: scan.last_stamp,
        })
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
        let Some(&offset) = self.offsets.get(seq.get() as usize) else {
            return Err(LogError::NotFound(EventId {
                actor: self.actor.clone(),
                seq,
            }));
        };
        let corrupt = || LogError::Corrupt {
            path: self.path.clone(),
            offset,
        };
        let mut head_bytes = [0u8; FRAME_HEAD];
        self.file.read_exact_at(&mut head_bytes, offset)?;
        let head = frame::decode_head(&head_bytes).ok_or_else(corrupt)?;
        let mut payload = vec![0u8; head.payload_len as usize];
        self.file
            .read_exact_at(&mut payload, offset + FRAME_HEAD as u64)?;
        if head.seq != seq || !frame::verify(&head_bytes, &payload, &head) {
            return Err(corrupt());
        }
        Ok(Frame {
            id: EventId {
                actor: self.actor.clone(),
                seq,
            },
            stamp: head.stamp,
            payload,
        })
    }
    /// Sequential iteration starting at `from` (inclusive), up to the frames indexed now.
    pub fn iter_from(&self, from: Seq) -> Result<FrameIter, LogError> {
        let offset = self
            .offsets
            .get(from.get() as usize)
            .copied()
            .unwrap_or(self.end);
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        Ok(FrameIter {
            file: BufReader::new(file),
            path: self.path.clone(),
            actor: self.actor.clone(),
            next: from.min(Seq::from_persisted(self.len())),
            end: Seq::from_persisted(self.len()),
            offset,
            peeked: None,
        })
    }
    /// Extends the index with frames appended since open. Returns how many were added.
    pub fn refresh(&mut self) -> Result<u64, LogError> {
        let first = Seq::from_persisted(self.len());
        let scan = frame::scan(&mut self.file, self.end, first, self.last_stamp)?;
        let added = scan.offsets.len() as u64;
        self.offsets.extend(scan.offsets);
        self.end = scan.end;
        self.last_stamp = scan.last_stamp;
        Ok(added)
    }
}

/// Buffered sequential reader over frames a [`LogReader`] already indexed.
#[derive(Debug)]
pub struct FrameIter {
    file: BufReader<File>,
    path: PathBuf,
    actor: ActorKey,
    next: Seq,
    end: Seq,
    /// Byte offset of the frame `next`.
    offset: u64,
    peeked: Option<([u8; FRAME_HEAD], FrameHead)>,
}

impl FrameIter {
    pub fn actor(&self) -> &ActorKey {
        &self.actor
    }
    /// The stamp of the next frame without reading its payload; drives the k-way merge.
    pub fn peek_stamp(&mut self) -> Result<Option<Stamp>, LogError> {
        if self.next >= self.end {
            return Ok(None);
        }
        if self.peeked.is_none() {
            let mut head_bytes = [0u8; FRAME_HEAD];
            if !frame::read_full(&mut self.file, &mut head_bytes)? {
                return Err(self.corrupt());
            }
            let head = frame::decode_head(&head_bytes).unwrap();
            self.peeked = Some((head_bytes, head));
        }
        Ok(self.peeked.as_ref().map(|(_, head)| head.stamp))
    }

    fn read_next(&mut self) -> Result<Option<Frame>, LogError> {
        if self.peek_stamp()?.is_none() {
            return Ok(None);
        }
        let (head_bytes, head) = self.peeked.take().unwrap();
        let mut payload = vec![0u8; head.payload_len as usize];
        if !frame::read_full(&mut self.file, &mut payload)?
            || head.seq != self.next
            || !frame::verify(&head_bytes, &payload, &head)
        {
            return Err(self.corrupt());
        }
        let seq = self.next;
        self.next = seq.next();
        self.offset += (FRAME_HEAD + payload.len()) as u64;
        Ok(Some(Frame {
            id: EventId {
                actor: self.actor.clone(),
                seq,
            },
            stamp: head.stamp,
            payload,
        }))
    }

    /// An indexed frame no longer reads back: the file was changed behind the log.
    fn corrupt(&self) -> LogError {
        LogError::Corrupt {
            path: self.path.clone(),
            offset: self.offset,
        }
    }
}

impl Iterator for FrameIter {
    type Item = Result<Frame, LogError>;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.read_next().transpose();
        if matches!(item, Some(Err(_))) {
            // One error ends the iteration rather than repeating on every call.
            self.end = self.next;
        }
        item
    }
}
