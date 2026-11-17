use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;

use crate::frame::{self, FileHead};
use crate::{ActorKey, LogDir, LogError, Seq, Stamp};

/// The only handle that can append to one actor's log. Not `Clone`; holds the OS lock
/// for its lifetime, so "two writers" cannot happen inside or across processes.
#[derive(Debug)]
pub struct LogWriter {
    file: File,
    actor: ActorKey,
    next: Seq,
    last_stamp: Option<Stamp>,
    /// Byte length of the trusted file; a failed append is cut back to it.
    end: u64,
    buf: Vec<u8>,
    dirty: bool,
}

/// What [`LogWriter::open`] found and repaired.
#[derive(Debug, Default)]
pub struct Recovery {
    /// Bytes of an incomplete or corrupt final frame that were cut off.
    pub truncated_bytes: u64,
    pub last: Option<(Seq, Stamp)>,
}

impl LogWriter {
    /// Opens or creates `<actor>.alog`.
    ///
    /// - A new file gets `header` written once; an existing file must carry the same
    ///   header bytes, or this fails with `HeaderMismatch` (one key, one meaning).
    /// - Takes the exclusive lock before reading anything.
    /// - Truncates a torn tail (invariant 4) and reports it.
    pub fn open(
        dir: &LogDir,
        actor: &ActorKey,
        header: &[u8],
    ) -> Result<(Self, Recovery), LogError> {
        fs::create_dir_all(dir.root())?;
        let path = dir.log_path(actor);
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LogError::Locked(path)),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }

        let mut recovery = Recovery::default();
        let len = file.metadata()?.len();
        let fresh = frame::encode_file_head(header);
        let start = if len == 0 {
            create(&mut file, &fresh, dir.root())?
        } else {
            match frame::read_file_head(&mut file, &path)? {
                // A crash while this log was created: nothing was ever appended to it.
                FileHead::Torn { bytes } if fresh.starts_with(&bytes) => {
                    file.set_len(0)?;
                    recovery.truncated_bytes = len;
                    create(&mut file, &fresh, dir.root())?
                }
                FileHead::Torn { .. } => return Err(LogError::HeaderMismatch(path)),
                FileHead::Whole { header: found, .. } if found != header => {
                    return Err(LogError::HeaderMismatch(path));
                }
                FileHead::Whole { end, .. } => end,
            }
        };

        let scan = frame::scan(&mut file, start, Seq::FIRST, None)?;
        let len = file.metadata()?.len();
        if len > scan.end {
            file.set_len(scan.end)?;
            file.sync_all()?;
            recovery.truncated_bytes += len - scan.end;
        }
        let next = Seq::from_persisted(scan.offsets.len() as u64);
        recovery.last = scan
            .last_stamp
            .map(|stamp| (Seq::from_persisted(next.get() - 1), stamp));
        let writer = LogWriter {
            file,
            actor: actor.clone(),
            next,
            last_stamp: scan.last_stamp,
            end: scan.end,
            buf: Vec::new(),
            dirty: false,
        };
        Ok((writer, recovery))
    }

    /// Appends one frame and returns its seq. Does not fsync; see [`LogWriter::sync`].
    /// Rejects a stamp that does not strictly exceed the previous one (invariant 3).
    pub fn append(&mut self, stamp: Stamp, payload: &[u8]) -> Result<Seq, LogError> {
        if let Some(last) = self.last_stamp
            && stamp <= last
        {
            return Err(LogError::NonMonotonicStamp { last, given: stamp });
        }
        self.buf.clear();
        frame::encode(self.next, stamp, payload, &mut self.buf);
        if let Err(e) = self.file.write_all(&self.buf) {
            // Leave no partial frame behind for the next append to follow.
            self.file.set_len(self.end)?;
            return Err(e.into());
        }
        let seq = self.next;
        self.end += self.buf.len() as u64;
        self.next = seq.next();
        self.last_stamp = Some(stamp);
        self.dirty = true;
        Ok(seq)
    }

    /// `fdatasync` if anything was appended since the last sync.
    pub fn sync(&mut self) -> Result<(), LogError> {
        if self.dirty {
            self.file.sync_data()?;
            self.dirty = false;
        }
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn next_seq(&self) -> Seq {
        self.next
    }
    pub fn actor(&self) -> &ActorKey {
        &self.actor
    }
}

/// Writes the file head in one write and makes the new file's name durable.
fn create(file: &mut File, head: &[u8], root: &Path) -> Result<u64, LogError> {
    file.write_all(head)?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(head.len() as u64)
}
