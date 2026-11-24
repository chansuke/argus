//! git via the `git` CLI. Content lives in git's object store; the ledger records only
//! which tree existed when (`Snapshot`), plus readings of those immutable trees.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use argus_core::diff::{DiffRequest, HeadDiff, parse_unified};
use argus_core::event::{FileDelta, FileState, Snapshot};
use argus_core::ids::{CommitOid, GitOid, JobId, RepoPath, TreeOid};

use crate::{glob, proc};

/// `git worktree add <at> -b argus/<job> <base>`: the job's own checkout, on its own branch.
/// An existing checkout at `at` (a retried start) is kept as it is.
pub(crate) fn create_worktree(
    repo: &Path,
    base: &CommitOid,
    job: JobId,
    at: &Path,
) -> std::io::Result<PathBuf> {
    if at.exists() {
        return Ok(at.to_path_buf());
    }
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let branch = format!("argus/{}", job.0);
    let target = at.to_string_lossy().into_owned();
    git(
        repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            &target,
            base.as_str(),
        ],
        &[],
        None,
    )?;
    Ok(at.to_path_buf())
}

/// The commit `rev` names in `repo` (`HEAD`, a branch, a commit id).
pub(crate) fn resolve(repo: &Path, rev: &str) -> std::io::Result<CommitOid> {
    let rev = format!("{rev}^{{commit}}");
    oid(&git(
        repo,
        &["rev-parse", "--verify", "--quiet", &rev],
        &[],
        None,
    )?)
}

/// The branch checked out in `repo`, if it is on one.
pub(crate) fn current_branch(repo: &Path) -> Option<String> {
    let out = git(
        repo,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        &[],
        None,
    )
    .ok()?;
    Some(String::from_utf8_lossy(&out).trim().to_owned())
}

/// Records the worktree without touching its index or HEAD.
///
/// 1. untracked = `git ls-files --others --exclude-standard -z` (git's ignore rules);
///    `withheld` = those matching `deny` (`command::SNAPSHOT_DENY` + config). Withheld
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
    let commit = commit_tree(worktree, &tree, base, &message)?;
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
    let mut command = proc::command("git");
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
        let key = (req.base.clone(), req.tree.clone());
        if let Some(diff) = self.entries.lock().unwrap().get(&key) {
            return Ok(diff.clone());
        }
        // Both ids name immutable objects, so a diff computed twice by racing callers is
        // the same diff; the lock is not held while git runs.
        let out = git(
            repo,
            &[
                "diff",
                "--no-color",
                "--no-renames",
                "--unified=3",
                req.base.as_str(),
                req.tree.as_str(),
            ],
            &[],
            None,
        )?;
        let diff = Arc::new(HeadDiff {
            request: req.clone(),
            hunks: argus_core::diff::parse_unified(&String::from_utf8_lossy(&out)),
        });
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= DIFF_CACHE_MAX {
            // Any entry may go: a dropped one is recomputed on the next read.
            let first = entries
                .keys()
                .next()
                .cloned()
                .expect("a full cache is not empty");
            entries.remove(&first);
        }
        Ok(entries.entry(key).or_insert(diff).clone())
    }
}

const DIFF_CACHE_MAX: usize = 32;

/// `Effect::Merge`: brings the approved snapshot (`tree` on `base`) into `target`.
///
/// - The snapshot's commit is `refs/argus/<job>/<tree>` (made if missing).
/// - Already in `target` -> nothing to do; a retried merge is a no-op.
/// - `target` not moved since `base` -> fast-forward; else a merge commit of
///   `git merge-tree --write-tree`, authored by the repository's configured user (the
///   human approved it). Conflicts -> `Err` naming the paths.
/// - `target` checked out in a worktree: a clean one is fast-forwarded there
///   (`git merge --ff-only`), as the user would; one with local changes is refused, and
///   nothing moves. Otherwise `git update-ref <new> <old>` (compare-and-swap), so a
///   target that moved meanwhile fails cleanly.
///
/// `Err` is recorded as `RunOutcome::Failed { reason }`.
pub(crate) fn merge(
    repo: &Path,
    job: JobId,
    tree: &TreeOid,
    base: &CommitOid,
    target: &str,
) -> Result<CommitOid, String> {
    let text = |out: Vec<u8>| String::from_utf8_lossy(&out).trim().to_owned();
    let run = |dir: &Path, args: &[&str]| {
        git(dir, args, &[], None)
            .map(text)
            .map_err(|e| e.to_string())
    };
    let branch = format!("refs/heads/{target}");
    let snapshot_ref = format!("refs/argus/{}/{}", job.0, tree.as_str());
    let commit = match run(repo, &["rev-parse", "--verify", "--quiet", &snapshot_ref]) {
        Ok(commit) => commit,
        Err(_) => {
            let commit = commit_tree(
                repo,
                tree,
                base,
                &format!("argus snapshot of job {}", job.0),
            )
            .map_err(|e| e.to_string())?;
            run(repo, &["update-ref", &snapshot_ref, &commit])?;
            commit
        }
    };
    let old = run(repo, &["rev-parse", "--verify", &branch])
        .map_err(|_| format!("no branch {target}"))?;
    let is_ancestor = |a: &str, b: &str| run(repo, &["merge-base", "--is-ancestor", a, b]).is_ok();
    if is_ancestor(&commit, &old) {
        return oid_of(&old);
    }
    let new = if is_ancestor(&old, &commit) {
        commit.clone()
    } else {
        let tree = merge_tree(repo, &old, &commit)?;
        let message = format!("Merge argus job {} into {target}", job.0);
        run(
            repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &old,
                "-p",
                &commit,
                "-m",
                &message,
            ],
        )?
    };
    match checkout_of(repo, &branch)? {
        Some(worktree) => {
            if !run(&worktree, &["status", "--porcelain"])?.is_empty() {
                return Err(format!(
                    "{target} is checked out in {} with local changes",
                    worktree.display()
                ));
            }
            run(&worktree, &["merge", "--ff-only", "--quiet", &new])?;
        }
        None => {
            run(repo, &["update-ref", &branch, &new, &old])?;
        }
    }
    oid_of(&new)
}

/// `git merge-tree --write-tree`: the merged tree, or the conflicted paths (exit 1 prints
/// the tree, then one conflicted path per line).
fn merge_tree(repo: &Path, ours: &str, theirs: &str) -> Result<String, String> {
    let out = proc::command("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            ours,
            theirs,
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = stdout.lines();
    match out.status.code() {
        Some(0) => Ok(lines.next().unwrap_or_default().to_owned()),
        Some(1) => {
            let paths: Vec<&str> = lines.skip(1).take_while(|l| !l.is_empty()).collect();
            Err(format!("merge conflicts in {}", paths.join(", ")))
        }
        _ => Err(format!(
            "git merge-tree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn oid_of(hex: &str) -> Result<CommitOid, String> {
    CommitOid::parse(hex).ok_or_else(|| format!("not an object id: {hex}"))
}

/// The worktree that has `branch` checked out, if any.
fn checkout_of(repo: &Path, branch: &str) -> Result<Option<PathBuf>, String> {
    let list =
        git(repo, &["worktree", "list", "--porcelain"], &[], None).map_err(|e| e.to_string())?;
    let list = String::from_utf8_lossy(&list).into_owned();
    let mut path = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line.strip_prefix("branch ") == Some(branch) {
            return Ok(path);
        }
    }
    Ok(None)
}

/// Throwaway checkout of `tree` on top of `base` (detached worktree under the host's
/// temp dir), removed when the guard drops.
///
/// The checkout is a commit of `tree` on `base`, so `git diff <base>` inside it shows the
/// snapshot's change and nothing the agent did since.
pub(crate) fn materialize(
    repo: &Path,
    tree: &TreeOid,
    base: &CommitOid,
) -> std::io::Result<TempCheckout> {
    let commit = commit_tree(repo, tree, base, "argus checkout")?;
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "argus-checkout-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let target = path.to_string_lossy().into_owned();
    git(
        repo,
        &["worktree", "add", "--quiet", "--detach", &target, &commit],
        &[],
        None,
    )?;
    Ok(TempCheckout {
        path,
        repo: repo.to_path_buf(),
    })
}

pub(crate) struct TempCheckout {
    pub path: PathBuf,
    repo: PathBuf,
}

impl Drop for TempCheckout {
    fn drop(&mut self) {
        let target = self.path.to_string_lossy().into_owned();
        let removed = git(
            &self.repo,
            &["worktree", "remove", "--force", &target],
            &[],
            None,
        );
        if removed.is_err() {
            // The worktree is gone or broken: remove the files and forget it.
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = git(&self.repo, &["worktree", "prune"], &[], None);
        }
    }
}

/// A commit of `tree` on `base` by argus (no user identity needed), returned as its id.
fn commit_tree(
    dir: &Path,
    tree: &TreeOid,
    base: &CommitOid,
    message: &str,
) -> std::io::Result<String> {
    let identity = [
        ("GIT_AUTHOR_NAME", OsStr::new("argus")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("argus@localhost")),
        ("GIT_COMMITTER_NAME", OsStr::new("argus")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("argus@localhost")),
    ];
    let args = [
        "commit-tree",
        tree.as_str(),
        "-p",
        base.as_str(),
        "-m",
        message,
    ];
    let commit = git(dir, &args, &identity, None)?;
    Ok(String::from_utf8_lossy(&commit).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use argus_core::command::SNAPSHOT_DENY;
    use argus_core::diff::parse_unified;
    use argus_core::ids::Id128;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = crate::proc::command("git")
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
        let inserted_on_top = argus_core::diff::Hunk {
            old_start: 0,
            old_len: 0,
            new_start: 1,
            new_len: 1,
        };
        assert_eq!(delta, vec![("a.txt", vec![inserted_on_top])]);
    }

    #[test]
    fn a_reviewer_gets_a_checkout_of_exactly_the_snapshot_that_goes_away_after() {
        let (dir, base) = repo();
        let s = snapshot(&dir, &base, None, JobId(Id128(1)), &deny()).unwrap();
        // The agent keeps editing after the snapshot.
        std::fs::write(dir.join("a.txt"), "edited later\n").unwrap();

        let checkout = materialize(&dir, &s.tree, &base).unwrap();
        let path = checkout.path.clone();
        assert_ne!(path, dir);
        assert_eq!(
            std::fs::read_to_string(path.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("src/c.txt")).unwrap(),
            "new\n"
        );
        assert!(!path.join("b.txt").exists());
        assert!(!path.join(".env").exists());
        // `git diff <base>` inside it shows the snapshot's change, which is what a reviewer runs.
        let diff = git(&path, &["diff", "--name-only", base.as_str()]);
        assert_eq!(
            diff.lines().collect::<Vec<_>>(),
            vec!["a.txt", "b.txt", "src/c.txt"]
        );
        assert_eq!(git(&path, &["rev-parse", "HEAD^{tree}"]), s.tree.as_str());
        // The agent's worktree is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "edited later\n"
        );

        drop(checkout);
        assert!(!path.exists());
        assert!(!git(&dir, &["worktree", "list"]).contains(path.to_str().unwrap()));
    }

    #[test]
    fn the_delta_groups_each_file_s_hunks_under_that_file() {
        let (dir, base) = repo();
        let job = JobId(Id128(1));
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        let first = snapshot(&dir, &base, None, job, &deny()).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n2\n3\n4\n5\n6\n7\n8\n9\nten\n").unwrap();
        std::fs::write(dir.join("src/c.txt"), "changed\n").unwrap();
        let second = snapshot(&dir, &base, Some(&first.tree), job, &deny()).unwrap();
        let delta: Vec<(&str, usize)> = second
            .delta
            .iter()
            .map(|d| (d.path.as_str(), d.hunks.len()))
            .collect();
        assert_eq!(delta, vec![("a.txt", 2), ("src/c.txt", 1)]);
    }

    // ------------------------------------------------------------ merge

    /// A job snapshot of `repo()`'s worktree, and a `release` branch at the base that
    /// nothing has checked out.
    #[test]
    fn a_head_diff_is_read_once_and_then_shared() {
        let (dir, base) = repo();
        let s = snapshot(&dir, &base, None, JobId(Id128(1)), &deny()).unwrap();
        let cache = DiffCache::default();
        let req = DiffRequest {
            base: base.clone(),
            tree: s.tree.clone(),
        };
        let diff = cache.get(&dir, &req).unwrap();
        assert_eq!(diff.request, req);
        let paths: Vec<&str> = diff.hunks.iter().map(|h| h.anchor.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt", "src/c.txt"]);
        let a = &diff.hunks[0];
        assert!(a.text.contains("-two\n+TWO\n"), "{}", a.text);
        assert!(std::sync::Arc::ptr_eq(
            &diff,
            &cache.get(&dir, &req).unwrap()
        ));
        // The diff is of the stored tree, not of whatever the worktree holds now.
        std::fs::write(dir.join("a.txt"), "changed again\n").unwrap();
        let fresh = DiffCache::default().get(&dir, &req).unwrap();
        assert_eq!(fresh.hunks.len(), diff.hunks.len());
        assert_eq!(fresh.hunks[0].text, diff.hunks[0].text);

        let missing = DiffRequest {
            base,
            tree: TreeOid::parse(&"1".repeat(40)).unwrap(),
        };
        assert!(cache.get(&dir, &missing).is_err());
    }

    fn merge_setup() -> (PathBuf, CommitOid, Snapshot) {
        let (dir, base) = repo();
        git(&dir, &["config", "user.name", "Reviewer"]);
        git(&dir, &["config", "user.email", "reviewer@example.invalid"]);
        git(&dir, &["branch", "release", base.as_str()]);
        let s = snapshot(&dir, &base, None, JobId(Id128(1)), &deny()).unwrap();
        (dir, base, s)
    }

    fn rev(dir: &Path, name: &str) -> String {
        git(dir, &["rev-parse", name])
    }

    #[test]
    fn an_unmoved_target_is_fast_forwarded_to_the_snapshot() {
        let (dir, base, s) = merge_setup();
        let job = JobId(Id128(1));
        let merged = merge(&dir, job, &s.tree, &base, "release").unwrap();
        assert_eq!(rev(&dir, "release"), merged.as_str());
        assert_eq!(rev(&dir, "release^{tree}"), s.tree.as_str());
        assert_eq!(
            rev(&dir, &format!("refs/argus/{}/{}", job.0, s.tree.as_str())),
            merged.as_str()
        );
        // A retry changes nothing.
        assert_eq!(merge(&dir, job, &s.tree, &base, "release").unwrap(), merged);
        assert_eq!(rev(&dir, "release"), merged.as_str());
    }

    #[test]
    fn a_moved_target_gets_a_merge_commit_with_both_sides() {
        let (dir, base, s) = merge_setup();
        let other = std::env::temp_dir().join(format!("argus-git-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        git(
            &dir,
            &["worktree", "add", "-q", other.to_str().unwrap(), "release"],
        );
        std::fs::write(other.join("notes.md"), "elsewhere\n").unwrap();
        git(&other, &["add", "notes.md"]);
        git(&other, &["commit", "-qm", "meanwhile"]);
        let moved = rev(&dir, "release");
        git(
            &dir,
            &["worktree", "remove", "--force", other.to_str().unwrap()],
        );

        let merged = merge(&dir, JobId(Id128(1)), &s.tree, &base, "release").unwrap();
        let parents = git(&dir, &["rev-list", "--parents", "-n", "1", merged.as_str()]);
        assert_eq!(parents.split(' ').count(), 3, "{parents}");
        assert!(parents.contains(&moved));
        let files = git(&dir, &["ls-tree", "-r", "--name-only", merged.as_str()]);
        assert!(
            files.contains("notes.md") && files.contains("src/c.txt"),
            "{files}"
        );
        assert_eq!(
            git(&dir, &["log", "-1", "--format=%an", merged.as_str()]),
            "Reviewer"
        );
    }

    #[test]
    fn a_conflict_names_its_paths_and_moves_nothing() {
        let (dir, base, s) = merge_setup();
        let other = std::env::temp_dir().join(format!("argus-git-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        git(
            &dir,
            &["worktree", "add", "-q", other.to_str().unwrap(), "release"],
        );
        std::fs::write(other.join("a.txt"), "one\nTHREE-WAY\nthree\n").unwrap();
        git(&other, &["commit", "-qam", "conflicting"]);
        git(
            &dir,
            &["worktree", "remove", "--force", other.to_str().unwrap()],
        );
        let before = rev(&dir, "release");

        let err = merge(&dir, JobId(Id128(1)), &s.tree, &base, "release").unwrap_err();
        assert!(err.contains("a.txt"), "{err}");
        assert_eq!(rev(&dir, "release"), before);
    }

    #[test]
    fn a_checked_out_target_moves_with_its_checkout_only_when_clean() {
        let (dir, base, s) = merge_setup();
        let other = std::env::temp_dir().join(format!("argus-git-checkout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        git(
            &dir,
            &["worktree", "add", "-q", other.to_str().unwrap(), "release"],
        );

        std::fs::write(other.join("b.txt"), "local edit\n").unwrap();
        let before = rev(&dir, "release");
        let err = merge(&dir, JobId(Id128(1)), &s.tree, &base, "release").unwrap_err();
        assert!(err.contains("local changes"), "{err}");
        assert_eq!(rev(&dir, "release"), before);

        git(&other, &["checkout", "--", "b.txt"]);
        let merged = merge(&dir, JobId(Id128(1)), &s.tree, &base, "release").unwrap();
        assert_eq!(rev(&dir, "release"), merged.as_str());
        // The checkout followed: its files are the snapshot's, and nothing is left changed.
        assert_eq!(
            std::fs::read_to_string(other.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert_eq!(git(&other, &["status", "--porcelain"]), "");
    }
}
