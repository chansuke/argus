use std::path::{Path, PathBuf};

use crate::LogError;

/// SHA-256 of a blob's bytes. Whole immutable files (simulator screenshots and
/// recordings, TLC trace dumps) live here rather than in a log because they are fetched
/// whole by id and never decoded or replayed. An event names the blob; the blob is
/// written and synced before that event is appended, so a referenced blob always exists.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlobId(pub [u8; 32]);

#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub(crate) fn at(root: PathBuf) -> Self {
        Self { root }
    }
    /// Streams `src` into the store (hashing while copying), fsyncs, renames into
    /// `ab/cdef...`. Idempotent: an existing blob with the same id is left alone.
    pub fn put_file(&self, src: &Path) -> Result<(BlobId, u64), LogError> {
        todo!("not implemented")
    }
    pub fn path(&self, id: &BlobId) -> PathBuf {
        todo!("not implemented")
    }
}
