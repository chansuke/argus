use std::fs::File;

use crate::{ActorKey, LogDir, LogError, Seq, Stamp};

/// The only handle that can append to one actor's log. Not `Clone`; holds the OS lock
/// for its lifetime, so "two writers" cannot happen inside or across processes.
#[derive(Debug)]
pub struct LogWriter {
    file: File,
    actor: ActorKey,
    next: Seq,
    last_stamp: Option<Stamp>,
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
        // TODO:
        //   create_dir_all(dir.root); open read+append (create)
        //   file.try_lock() -> Err(Locked) on WouldBlock
        //   if len == 0: write MAGIC + header; sync
        //   else: verify MAGIC, compare header; scan frame heads to find the last good
        //         frame (stop at short read / bad crc / seq != index); set_len(good_end)
        todo!("not implemented")
    }

    /// Appends one frame and returns its seq. Does not fsync; see [`LogWriter::sync`].
    /// Rejects a stamp that does not strictly exceed the previous one (invariant 3).
    pub fn append(&mut self, stamp: Stamp, payload: &[u8]) -> Result<Seq, LogError> {
        if let Some(last) = self.last_stamp
            && stamp <= last
        {
            return Err(LogError::NonMonotonicStamp { last, given: stamp });
        }
        // TODO: frame::encode into a reused buffer; single write_all; bump next, set dirty.
        todo!("not implemented")
    }

    /// `fdatasync` if anything was appended since the last sync.
    pub fn sync(&mut self) -> Result<(), LogError> {
        todo!("not implemented")
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
