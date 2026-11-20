//! git via the `git` CLI. Content lives in git's object store; the ledger records only
//! which tree existed when (`Snapshot`), plus readings of those immutable trees.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use argus_core::diff::{DiffRequest, HeadDiff, parse_unified};
use argus_core::event::{ApprovedHead, FileDelta, FileState, Snapshot};
use argus_core::ids::{CommitOid, GitOid, JobId, RepoPath, TreeOid};

use crate::glob;

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
/// 4. `git update-ref refs/argus/<job>/<tree> <commit-tree tree -p base>` so gc keeps
///    it. Named by the tree, so recording the same tree twice is one ref.
pub(crate) fn snapshot(
    worktree: &Path,
    base: &CommitOid,
    previous: Option<&TreeOid>,
    job: JobId,
    deny: &[String],
) -> std::io::Result<Snapshot> {
    let untracked = git(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        &[],
        None,
    )?;
    let (withheld, staged): (Vec<String>, Vec<String>) = nul_separated(&untracked)
        .into_iter()
        .partition(|path| denied(path, deny));

    static N: AtomicU32 = AtomicU32::new(0);
    let index = std::env::temp_dir().join(format!(
        "argus-index-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let index_env = [("GIT_INDEX_FILE", index.as_os_str())];
    let tree = (|| {
        git(worktree, &["read-tree", "HEAD"], &index_env, None)?;
        git(worktree, &["add", "-u"], &index_env, None)?;
        if !staged.is_empty() {
            let list = staged.join("\0");
            let args = ["add", "--pathspec-from-file=-", "--pathspec-file-nul"];
            git(worktree, &args, &index_env, Some(list.as_bytes()))?;
        }
        oid(&git(worktree, &["write-tree"], &index_env, None)?)
    })();
    let _ = std::fs::remove_file(&index);
    let tree = tree?;

    let delta = match previous {
        None => Vec::new(),
        Some(previous) => {
            let diff = diff_text(worktree, previous.as_str(), tree.as_str(), "--unified=0")?;
            let mut delta: Vec<FileDelta> = Vec::new();
            for hunk in parse_unified(&diff) {
                match delta.last_mut() {
                    Some(d) if d.path == hunk.anchor.path => d.hunks.push(hunk.header),
                    _ => delta.push(FileDelta {
                        path: hunk.anchor.path,
                        renamed_from: None,
                        hunks: vec![hunk.header],
                    }),
                }
            }
            delta
        }
    };

    let changed = git(
        worktree,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            base.as_str(),
            tree.as_str(),
        ],
        &[],
        None,
    )?;
    let diff = diff_text(worktree, base.as_str(), tree.as_str(), "--unified=3")?;
    let hunks = parse_unified(&diff);
    let mut files = Vec::new();
    for path in nul_separated(&changed) {
        let Some(path) = RepoPath::parse(&path) else {
            continue;
        };
        let listed = git(
            worktree,
            &["ls-tree", "-z", tree.as_str(), "--", path.as_str()],
            &[],
            None,
        )?;
        // `<mode> blob <oid>\t<path>`; nothing when the tree no longer has the file.
        let blob = String::from_utf8_lossy(&listed)
            .split(['\t', ' '])
            .nth(2)
            .and_then(GitOid::parse);
        let anchors = hunks
            .iter()
            .filter(|h| h.anchor.path == path)
            .map(|h| h.anchor.clone())
            .collect();
        files.push(FileState {
            path,
            blob,
            hunks: anchors,
        });
    }

    let message = format!("argus snapshot of job {}", job.0);
    let identity = [
        ("GIT_AUTHOR_NAME", OsStr::new("argus")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("argus@localhost")),
        ("GIT_COMMITTER_NAME", OsStr::new("argus")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("argus@localhost")),
    ];
    let commit = git(
        worktree,
        &[
            "commit-tree",
            tree.as_str(),
            "-p",
            base.as_str(),
            "-m",
            &message,
        ],
        &identity,
        None,
    )?;
    let commit = String::from_utf8_lossy(&commit).trim().to_owned();
    let reference = format!("refs/argus/{}/{}", job.0, tree.as_str());
    git(worktree, &["update-ref", &reference, &commit], &[], None)?;

    Ok(Snapshot {
        tree,
        base: base.clone(),
        previous: previous.cloned(),
        delta,
        files,
        withheld: withheld.iter().filter_map(|p| RepoPath::parse(p)).collect(),
    })
}

/// Runs `git` in `dir` and returns its stdout; a failure carries git's stderr.
fn git(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &OsStr)],
    stdin: Option<&[u8]>,
) -> std::io::Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(out.stdout)
}

fn diff_text(dir: &Path, from: &str, to: &str, context: &str) -> std::io::Result<String> {
    let args = ["diff", "--no-color", "--no-renames", context, from, to];
    Ok(String::from_utf8_lossy(&git(dir, &args, &[], None)?).into_owned())
}

fn oid(stdout: &[u8]) -> std::io::Result<GitOid> {
    let text = String::from_utf8_lossy(stdout);
    GitOid::parse(text.trim())
        .ok_or_else(|| std::io::Error::other(format!("not an object id: {text}")))
}

fn nul_separated(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// A pattern without `/` matches the file name anywhere; one with `/` the whole path.
fn denied(path: &str, deny: &[String]) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    deny.iter().any(|pattern| match pattern.contains('/') {
        true => glob::wildcard(pattern, path),
        false => glob::wildcard(pattern, name),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    use argus_core::diff::parse_unified;
    use argus_core::ids::Id128;
    use argus_core::plan::SNAPSHOT_DENY;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
    }

    /// A repository with a committed base, and a worktree the agent then changed.
    fn repo() -> (PathBuf, CommitOid) {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "argus-git-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("b.txt"), "gone\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "base"]);
        let base = CommitOid::parse(&git(&dir, &["rev-parse", "HEAD"])).unwrap();

        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/c.txt"), "new\n").unwrap();
        std::fs::write(dir.join(".env"), "TOKEN=secret\n").unwrap();
        std::fs::write(dir.join("src/key.pem"), "-----BEGIN-----\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/out.bin"), "build output\n").unwrap();
        (dir, base)
    }

    fn deny() -> Vec<String> {
        SNAPSHOT_DENY.iter().map(|s| s.to_string()).collect()
    }

    fn tree_files(dir: &Path, tree: &TreeOid) -> Vec<String> {
        git(dir, &["ls-tree", "-r", "--name-only", tree.as_str()])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_worktree_is_recorded_without_secrets_ignored_files_or_touching_the_index() {
        let (dir, base) = repo();
        let job = JobId(Id128(1));
        let status = git(&dir, &["status", "--porcelain"]);
        let s = snapshot(&dir, &base, None, job, &deny()).unwrap();

        assert_eq!(
            tree_files(&dir, &s.tree),
            vec![".gitignore", "a.txt", "src/c.txt"]
        );
        let withheld: Vec<&str> = s.withheld.iter().map(|p| p.as_str()).collect();
        assert_eq!(withheld, vec![".env", "src/key.pem"]);
        // The agent's index and HEAD are as they were.
        assert_eq!(git(&dir, &["status", "--porcelain"]), status);
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), base.as_str());
        assert_eq!((s.base.clone(), s.previous.clone()), (base.clone(), None));
        assert!(s.delta.is_empty());

        // Files that differ from the base: blob ids from the tree, anchors from the diff.
        let files: Vec<(&str, Option<String>)> = s
            .files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.blob.as_ref().map(|b| b.as_str().to_owned()),
                )
            })
            .collect();
        let blob = |path: &str| {
            Some(git(
                &dir,
                &["rev-parse", &format!("{}:{path}", s.tree.as_str())],
            ))
        };
        assert_eq!(
            files,
            vec![
                ("a.txt", blob("a.txt")),
                ("b.txt", None),
                ("src/c.txt", blob("src/c.txt"))
            ]
        );
        let diff = git(
            &dir,
            &[
                "diff",
                "--no-color",
                "--no-renames",
                "--unified=3",
                base.as_str(),
                s.tree.as_str(),
            ],
        );
        let anchors: Vec<_> = parse_unified(&format!("{diff}\n"))
            .into_iter()
            .map(|h| h.anchor)
            .collect();
        let recorded: Vec<_> = s.files.iter().flat_map(|f| f.hunks.clone()).collect();
        assert_eq!(recorded, anchors);

        // A ref keeps the tree alive: a commit of it on top of the base.
        let commit = git(
            &dir,
            &[
                "rev-parse",
                &format!("refs/argus/{}/{}", job.0, s.tree.as_str()),
            ],
        );
        assert_eq!(
            git(&dir, &["rev-parse", &format!("{commit}^{{tree}}")]),
            s.tree.as_str()
        );
        assert_eq!(
            git(&dir, &["rev-parse", &format!("{commit}^")]),
            base.as_str()
        );
    }

    #[test]
    fn a_later_snapshot_records_its_delta_from_the_previous_tree() {
        let (dir, base) = repo();
        let job = JobId(Id128(1));
        let first = snapshot(&dir, &base, None, job, &deny()).unwrap();
        // Nothing changed: the same tree.
        let again = snapshot(&dir, &base, Some(&first.tree), job, &deny()).unwrap();
        assert_eq!(again.tree, first.tree);
        assert!(again.delta.is_empty());

        std::fs::write(dir.join("a.txt"), "zero\none\nTWO\nthree\n").unwrap();
        let second = snapshot(&dir, &base, Some(&first.tree), job, &deny()).unwrap();
        assert_ne!(second.tree, first.tree);
        assert_eq!(second.previous, Some(first.tree.clone()));
        let delta: Vec<_> = second
            .delta
            .iter()
            .map(|d| (d.path.as_str(), d.hunks.clone()))
            .collect();
        let inserted_on_top = argus_core::event::Hunk {
            old_start: 0,
            old_len: 0,
            new_start: 1,
            new_len: 1,
        };
        assert_eq!(delta, vec![("a.txt", vec![inserted_on_top])]);
    }
}
