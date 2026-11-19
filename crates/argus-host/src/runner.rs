//! Executes an `Invocation` and reports back as facts. Knows how to run a process; knows
//! nothing about any particular tool (that is `argus_core::adapters`, reached via `plan`).

use std::path::Path;
use std::sync::mpsc::Sender;

use argus_core::ids::{Attempt, RunId};
use argus_core::plan::{CommandSpec, Effect, Invocation, PlanEnv};

use crate::ledger_thread::Cmd;

pub(crate) struct Running {
    pub run: RunId,
    pub pid: Option<u32>,
    kill: Sender<()>,
}

/// TODO:
/// - Process(spec): materialize workdir (git::materialize for Snapshot); open sources
///   (stdin -> write bytes as frames first; stdout/stderr or one Terminal source);
///   spawn (PTY via `script -qfec` or openpty FFI, see open question); one reader thread
///   per stream: Framer::push -> Cmd::Observe; tailers for `watch` globs;
///   wait (timeout -> kill); collect artifacts; submit RunFinished; send Released.
/// - Leased{lease, inner}: lease::run_leased(lease, inner, ...) wraps the same steps.
/// - InProcess(Snapshot): git::snapshot(.., &env.snapshot_deny) -> RunFinished{ Snapshot(..) }.
/// - InProcess(Merge(token)): git::merge(repo, &token, &job.target) ->
///   RunFinished{ Merged{commit} | Failed{reason} }.
/// - InProcess(WriteSkill): write `<target>/<name>/SKILL.md` (frontmatter name +
///   description, then body) via temp + rename -> RunFinished{ SkillWritten }.
/// - InProcess(Import): stream the file through the kind's framer -> Observe; finish.
pub(crate) fn start(inv: Invocation, env: PlanEnv, ledger: Sender<Cmd>) -> Running {
    todo!("not implemented")
}

/// Files left in `$ARGUS_ARTIFACT_DIR`: BlobStore::put_file each (fsync), then submit
/// `ArtifactCaptured` (blob written before the event that names it).
pub(crate) fn collect_artifacts(
    dir: &std::path::Path,
    run: RunId,
    attempt: Attempt,
    ledger: &Sender<Cmd>,
) {
    todo!("not implemented")
}

/// Runs `spec` in `dir` (the resolved workdir: the job worktree, a snapshot checkout, or
/// home) and records everything it reads and writes as sources of `env.run`:
///
/// - `stdin` bytes, if any, as a `Stdin` source before they are written to the process.
/// - Under a PTY (`terminal`): the terminal as one `Terminal` source, and `script`'s own
///   stderr. Otherwise stdout and stderr, each with its kind.
///
/// Output is framed and observed as it arrives, so the raw log is live. A `timeout`
/// kills the process. Returns once the process exited and every stream was read to its
/// end.
pub(crate) fn spawn_capture(
    spec: &CommandSpec,
    dir: &Path,
    env: &PlanEnv,
    ledger: &Sender<Cmd>,
) -> std::io::Result<std::process::ExitStatus> {
    todo!("not implemented")
}

pub(crate) fn run_effect(effect: Effect, env: &PlanEnv, ledger: &Sender<Cmd>) {
    todo!("not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use argus_core::ids::{Id128, SourceId};
    use argus_core::ledger::SourceCursor;
    use argus_core::plan::{SNAPSHOT_DENY, ToolPaths, Workdir};
    use argus_core::source::{RawFrame, SourceKind, SourceMeta, SourceOrigin};

    /// What a fake ledger thread received: per source, its header and stored frames.
    type Received = BTreeMap<SourceId, (SourceMeta, Vec<Vec<u8>>)>;

    fn fake_ledger() -> (Sender<Cmd>, JoinHandle<Received>) {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut got = Received::new();
            for cmd in rx {
                match cmd {
                    Cmd::OpenSource { meta, reply } => {
                        got.insert(meta.id, (meta.clone(), Vec::new()));
                        let cursor = SourceCursor {
                            meta,
                            resume_at: 0,
                            last: None,
                        };
                        reply.send(Ok(cursor)).unwrap();
                    }
                    Cmd::Observe { source, frames } => {
                        let stored = &mut got.get_mut(&source).expect("observed after open").1;
                        for frame in frames {
                            match frame {
                                RawFrame::Bytes { bytes, .. } => stored.push(bytes),
                                other => panic!("{other:?}"),
                            }
                        }
                    }
                    _ => {}
                }
            }
            got
        });
        (tx, thread)
    }

    fn env() -> PlanEnv {
        PlanEnv {
            job: None,
            run: RunId(Id128(1)),
            attempt: Attempt(0),
            socket: PathBuf::from("/run/argus.sock"),
            tools: ToolPaths {
                claude: "claude".into(),
                codex: "codex".into(),
                java: "java".into(),
                tla2tools_jar: "tla2tools.jar".into(),
                tlars: "tlars".into(),
                lake: "lake".into(),
                eas: "eas".into(),
                script: "script".into(),
            },
            snapshot_deny: SNAPSHOT_DENY.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sh(script: &str) -> CommandSpec {
        CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Vec::new(),
            workdir: Workdir::Home,
            cwd: None,
            stdin: None,
            stdout: SourceKind::PlainText,
            stderr: SourceKind::PlainText,
            terminal: false,
            watch: Vec::new(),
            artifacts: false,
            timeout: None,
        }
    }

    fn capture(spec: &CommandSpec, dir: &Path) -> (std::process::ExitStatus, Received) {
        let (ledger, thread) = fake_ledger();
        let status = spawn_capture(spec, dir, &env(), &ledger).unwrap();
        drop(ledger);
        (status, thread.join().unwrap())
    }

    fn by_origin(got: &Received, origin: SourceOrigin) -> Option<(SourceKind, Vec<Vec<u8>>)> {
        got.values()
            .find(|(meta, _)| meta.origin == origin)
            .map(|(meta, frames)| (meta.kind, frames.clone()))
    }

    #[test]
    fn stdout_and_stderr_are_separate_sources_framed_as_they_arrive() {
        let (status, got) = capture(
            &sh("printf 'a\\nb'; printf err >&2; exit 4"),
            Path::new("/"),
        );
        assert_eq!(status.code(), Some(4));
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout),
            Some((SourceKind::PlainText, vec![b"a\n".to_vec(), b"b".to_vec()]))
        );
        assert_eq!(
            by_origin(&got, SourceOrigin::Stderr).unwrap().1,
            vec![b"err".to_vec()]
        );
        // Every source belongs to the run's attempt and has its derived id.
        for (id, (meta, _)) in &got {
            assert_eq!((meta.run, meta.attempt), (RunId(Id128(1)), Attempt(0)));
            assert_eq!(
                *id,
                SourceMeta::derive_id(meta.run, meta.attempt, &meta.origin)
            );
        }
    }

    #[test]
    fn what_the_process_was_fed_is_recorded_before_it_is_written() {
        let mut spec = sh("cat");
        spec.stdin = Some(b"review this\nplease\n".to_vec());
        let (status, got) = capture(&spec, Path::new("/"));
        assert!(status.success());
        let fed = by_origin(&got, SourceOrigin::Stdin).unwrap().1.concat();
        assert_eq!(fed, b"review this\nplease\n");
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            fed
        );
    }

    #[test]
    fn a_terminal_process_is_one_terminal_source() {
        let mut spec = sh("test -t 0 && test -t 1 && printf tty");
        spec.terminal = true;
        let (status, got) = capture(&spec, Path::new("/"));
        assert!(status.success());
        let (kind, frames) = by_origin(&got, SourceOrigin::Terminal).unwrap();
        assert_eq!(kind, SourceKind::Terminal);
        assert_eq!(frames.concat(), b"tty");
        assert!(by_origin(&got, SourceOrigin::Stdout).is_none());
    }

    #[test]
    fn the_process_runs_in_its_directory_with_its_environment() {
        let dir = std::env::temp_dir().join(format!("argus-runner-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("proofs")).unwrap();
        let mut spec = sh("printf '%s %s' \"$(basename \"$PWD\")\" \"$LAKE\"");
        spec.cwd = argus_core::ids::RepoPath::parse("proofs");
        spec.env = vec![("LAKE".into(), "/bin/lake".into())];
        let (_, got) = capture(&spec, &dir);
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            b"proofs /bin/lake"
        );
    }

    #[test]
    fn a_process_past_its_timeout_is_killed() {
        let mut spec = sh("sleep 30");
        spec.timeout = Some(Duration::from_millis(200));
        let started = Instant::now();
        let (status, _) = capture(&spec, Path::new("/"));
        assert!(!status.success());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }
}
