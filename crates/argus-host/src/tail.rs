//! Tails files that other programs write (agent transcripts). Polling (250 ms) keeps it
//! std-only; inotify would need FFI.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use argus_core::ledger::SourceCursor;
use argus_core::plan::WatchSpec;

use crate::ledger_thread::Cmd;

/// Follows one file from `cursor.resume_at` until its run finishes (then a final read and
/// `framer.finish()`). Never stops on a rewrite.
///
/// TODO, per poll:
/// - rewritten = file len < resume_at, or the bytes at `cursor.last`'s offset no longer
///   equal the last stored frame (the agent truncated, compacted, or rewrote its log).
/// - rewritten -> Observe [RawFrame::Discontinuity { stored_end: resume_at, file_len }],
///   reset the framer to offset 0, and ingest the whole new content. Stored frames are
///   never edited, so every reference stays valid; records the new content repeats are
///   dropped by `ledger::Applied`, so nothing is counted twice.
/// - else read the new bytes -> framer.push -> Observe (complete units only).
pub(crate) fn tail(
    path: PathBuf,
    cursor: SourceCursor,
    ledger: Sender<Cmd>,
) -> std::thread::JoinHandle<()> {
    todo!("not implemented")
}

/// Expands `watch` globs under $HOME on each poll; every new match becomes a source
/// (`SourceOrigin::File(path)`, deterministic id) and gets a tailer. This is how subagent
/// transcripts that appear mid-run are picked up.
pub(crate) fn watch(specs: Vec<WatchSpec>, ledger: Sender<Cmd>) -> std::thread::JoinHandle<()> {
    todo!("not implemented")
}
