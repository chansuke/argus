//! Executes an `Invocation` and reports back as facts. Knows how to run a process; knows
//! nothing about any particular tool (that is `argus_core::adapters`, reached via `plan`).

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::ExitStatus;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use argus_core::ids::{Attempt, RunId};
use argus_core::ledger::SourceCursor;
use argus_core::plan::{CommandSpec, Effect, Invocation, PlanEnv};
use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};

use crate::ledger_thread::{self, Cmd, observe};
use crate::pty;

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
/// Output is framed and observed as it arrives, so the raw log is live. A message on
/// `control.cancel` or a passed `timeout` stops the process (see [`Control`]). Returns
/// once the process exited and every stream was read to its end.
/// How a running process is stopped: SIGTERM first, so an agent can exit cleanly, then
/// SIGKILL once `grace` has passed.
pub(crate) struct Control {
    pub cancel: Receiver<()>,
    pub grace: Duration,
}

/// How a captured process ended.
#[derive(Debug)]
pub(crate) struct Ended {
    pub status: ExitStatus,
    /// It was stopped because of a cancel (not a timeout, not on its own).
    pub cancelled: bool,
}

pub(crate) fn spawn_capture(
    spec: &CommandSpec,
    dir: &Path,
    env: &PlanEnv,
    ledger: &Sender<Cmd>,
    control: &Control,
) -> std::io::Result<Ended> {
    let (program, args) = match spec.terminal {
        true => pty::wrap(&env.tools.script, &spec.program, &spec.args),
        false => (spec.program.clone(), spec.args.clone()),
    };
    let workdir = match &spec.cwd {
        Some(cwd) => dir.join(cwd.as_str()),
        None => dir.to_path_buf(),
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(workdir)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A PTY's input stays open for what the human types (`RunInput`).
        .stdin(match spec.stdin.is_some() || spec.terminal {
            true => Stdio::piped(),
            false => Stdio::null(),
        });

    // What the process is fed is on record before it is written.
    let fed = match &spec.stdin {
        Some(bytes) => {
            let cursor = open_source(ledger, env, SourceOrigin::Stdin, SourceKind::PlainText)?;
            let mut framer = SourceKind::PlainText.framer(cursor.resume_at);
            let mut frames = framer.push(bytes);
            frames.extend(framer.finish());
            observe(ledger, cursor.meta.id, frames);
            Some(bytes.clone())
        }
        None => None,
    };
    let (out_origin, out_kind) = match spec.terminal {
        true => (SourceOrigin::Terminal, SourceKind::Terminal),
        false => (SourceOrigin::Stdout, spec.stdout),
    };
    let out = open_source(ledger, env, out_origin, out_kind)?;
    let err = open_source(ledger, env, SourceOrigin::Stderr, spec.stderr)?;

    // Its own process group, so a stop reaches what it started too.
    command.process_group(0);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take();
    let writer = fed.zip(stdin.take()).map(|(bytes, mut pipe)| {
        // A thread, so a process that writes before it reads cannot deadlock the host.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        })
    });
    let readers = [
        pump(child.stdout.take(), out, ledger.clone()),
        pump(child.stderr.take(), err, ledger.clone()),
    ];

    // Stopping: SIGTERM to the process group, then SIGKILL once the grace has passed.
    let started = Instant::now();
    let mut stopping: Option<Instant> = None;
    let mut cancelled = false;
    let pgid = child.id();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        match stopping {
            None => {
                let cancel = control.cancel.try_recv().is_ok();
                let timed_out = spec.timeout.is_some_and(|t| started.elapsed() >= t);
                if cancel || timed_out {
                    cancelled = cancel;
                    // On a PTY the program leads its own session under `script`, which
                    // would only hang it up; ask the program itself.
                    match spec.terminal {
                        true => children(pgid)
                            .into_iter()
                            .for_each(|c| signal_group(c, "TERM")),
                        false => signal_group(pgid, "TERM"),
                    }
                    stopping = Some(Instant::now());
                }
            }
            Some(since) if since.elapsed() >= control.grace => {
                children(pgid)
                    .into_iter()
                    .for_each(|c| signal_group(c, "KILL"));
                signal_group(pgid, "KILL");
                break child.wait()?;
            }
            Some(_) => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = Ended { status, cancelled };
    drop(stdin);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }
    Ok(status)
}

/// Direct children of `pid` (Linux `/proc`); empty once it has none or is gone.
fn children(pid: u32) -> Vec<u32> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|c| c.parse().ok())
        .collect()
}

/// `kill -<signal> -<pgid>`: std can only SIGKILL one process, not ask a group to exit.
fn signal_group(pgid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn open_source(
    ledger: &Sender<Cmd>,
    env: &PlanEnv,
    origin: SourceOrigin,
    kind: SourceKind,
) -> std::io::Result<SourceCursor> {
    let meta = SourceMeta {
        id: SourceMeta::derive_id(env.run, env.attempt, &origin),
        run: env.run,
        attempt: env.attempt,
        kind,
        origin,
    };
    ledger_thread::open_source(ledger, meta)
}

/// Reads `stream` to its end, observing complete frames as they arrive.
fn pump(
    stream: Option<impl Read + Send + 'static>,
    cursor: SourceCursor,
    ledger: Sender<Cmd>,
) -> Option<JoinHandle<()>> {
    let mut stream = stream?;
    Some(std::thread::spawn(move || {
        let mut framer = cursor.meta.kind.framer(cursor.resume_at);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => observe(&ledger, cursor.meta.id, framer.push(&buf[..n])),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        observe(
            &ledger,
            cursor.meta.id,
            framer.finish().into_iter().collect(),
        );
    }))
}

pub(crate) fn run_effect(effect: Effect, env: &PlanEnv, ledger: &Sender<Cmd>) {
    todo!("not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger_thread::testing::{Received, bytes, fake_ledger};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use argus_core::ids::Id128;
    use argus_core::plan::{SNAPSHOT_DENY, ToolPaths, Workdir};
    use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};

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
        let (_cancel, control) = control(Duration::from_secs(5));
        let (ended, got) = capture_with(spec, dir, &control);
        assert!(!ended.cancelled);
        (ended.status, got)
    }

    fn control(grace: Duration) -> (Sender<()>, Control) {
        let (tx, cancel) = std::sync::mpsc::channel();
        (tx, Control { cancel, grace })
    }

    fn capture_with(spec: &CommandSpec, dir: &Path, control: &Control) -> (Ended, Received) {
        let (ledger, thread) = fake_ledger();
        let ended = spawn_capture(spec, dir, &env(), &ledger, control).unwrap();
        drop(ledger);
        (ended, thread.join().unwrap())
    }

    /// Loops for about five seconds unless stopped.
    const BUSY: &str = "i=0; while [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done";

    #[test]
    fn a_cancel_asks_the_process_to_exit_first() {
        let spec = sh(&format!("trap 'echo bye; exit 0' TERM; {BUSY}"));
        let (cancel, control) = control(Duration::from_secs(5));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.send(()).unwrap();
        });
        let started = Instant::now();
        let (ended, got) = capture_with(&spec, Path::new("/"), &control);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(ended.cancelled);
        // It handled SIGTERM itself and exited cleanly.
        assert_eq!(ended.status.code(), Some(0));
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            b"bye\n"
        );
    }

    #[test]
    fn a_cancel_reaches_a_program_on_a_terminal() {
        let mut spec = sh(&format!("trap 'echo bye; exit 0' TERM; {BUSY}"));
        spec.terminal = true;
        let (cancel, control) = control(Duration::from_secs(5));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.send(()).unwrap();
        });
        let started = Instant::now();
        let (ended, got) = capture_with(&spec, Path::new("/"), &control);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(ended.cancelled);
        let screen = by_origin(&got, SourceOrigin::Terminal).unwrap().1.concat();
        assert!(
            String::from_utf8_lossy(&screen).contains("bye"),
            "{screen:?}"
        );
    }

    #[test]
    fn a_process_that_ignores_the_request_is_killed_after_the_grace() {
        let spec = sh(&format!("trap '' TERM; {BUSY}"));
        let (cancel, control) = control(Duration::from_millis(300));
        cancel.send(()).unwrap();
        let started = Instant::now();
        let (ended, _) = capture_with(&spec, Path::new("/"), &control);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(ended.cancelled);
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(ended.status.signal(), Some(9));
    }

    fn by_origin(got: &Received, origin: SourceOrigin) -> Option<(SourceKind, Vec<Vec<u8>>)> {
        got.values()
            .find(|(meta, _)| meta.origin == origin)
            .map(|(meta, frames)| (meta.kind, bytes(frames)))
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
