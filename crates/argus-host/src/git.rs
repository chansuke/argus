//! git via the `git` CLI. Content lives in git's object store; the ledger records only
//! which tree existed when (`Snapshot`), plus readings of those immutable trees.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argus_core::diff::{DiffRequest, HeadDiff};
use argus_core::event::{ApprovedHead, Snapshot};
use argus_core::ids::{CommitOid, JobId, TreeOid};

/// `git worktree add <path> -b argus/<job> <base>`.
pub(crate) fn create_worktree(
    repo: &Path,
    base: &CommitOid,
    job: JobId,
) -> std::io::Result<PathBuf> {
    todo!("not implemented")
}

/// Records the worktree without touching its index or HEAD.
///
/// 1. untracked = `git ls-files --others --exclude-standard -z` (git's ignore rules);
///    `withheld` = those matching `deny` (`plan::SNAPSHOT_DENY` + config). Withheld
///    files are never staged, so a secret never enters a tree, a ref, or a reviewer's
///    checkout. Tracked files are always included.
/// 2. With `GIT_INDEX_FILE=<tmp>`: `git read-tree HEAD`, `git add -u`,
///    `git add -- <untracked minus withheld>`, `git write-tree`.
/// 3. `delta` from `git diff --unified=0 <previous> <tree>` (hunk headers only); `files`
///    from `git diff <base> <tree>` through `diff::parse_unified` (anchors) and
///    `git ls-tree <tree> -- <changed paths>` (blob ids).
/// 4. `git update-ref refs/argus/<job>/<n> <commit-tree tree -p base>` so gc keeps it.
pub(crate) fn snapshot(
    worktree: &Path,
    base: &CommitOid,
    previous: Option<&TreeOid>,
    job: JobId,
    deny: &[String],
) -> std::io::Result<Snapshot> {
    todo!("not implemented")
}

/// Head diffs for review packets, memoized by `(base, tree)`: both are immutable, so an
/// entry never goes stale and needs no invalidation. Runs on the calling API thread,
/// never on a ledger thread, and the lock is not held while git runs.
#[derive(Default)]
pub(crate) struct DiffCache {
    /// Small and bounded (the latest few heads per job).
    entries: Mutex<BTreeMap<(CommitOid, TreeOid), Arc<HeadDiff>>>,
}

impl DiffCache {
    /// `git diff --no-color --no-renames --unified=3 <base> <tree>` -> `diff::parse_unified`.
    pub(crate) fn get(&self, repo: &Path, req: &DiffRequest) -> std::io::Result<Arc<HeadDiff>> {
        todo!("not implemented")
    }
}

/// `Effect::Merge`: merge the approved snapshot commit into `target` without a checkout:
/// `git merge-tree --write-tree <target> <snapshot commit>`, `git commit-tree`, then
/// `git update-ref refs/heads/<target> <new> <old>` (compare-and-swap, so a target that
/// moved meanwhile fails cleanly and a retried merge of an applied one is a no-op).
/// Conflicts -> `Err` with the paths, recorded as `RunOutcome::Failed`.
pub(crate) fn merge(
    repo: &Path,
    approved: &ApprovedHead,
    target: &str,
) -> Result<CommitOid, String> {
    todo!("not implemented")
}

/// Throwaway checkout of `tree` on top of `base` (detached worktree under the host's
/// temp dir), removed when the guard drops.
pub(crate) fn materialize(
    repo: &Path,
    tree: &TreeOid,
    base: &CommitOid,
) -> std::io::Result<TempCheckout> {
    todo!("not implemented")
}

pub(crate) struct TempCheckout {
    pub path: PathBuf,
}
