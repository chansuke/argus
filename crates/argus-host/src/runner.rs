//! Executes an `Invocation` and reports back as facts. Knows how to run a process; knows
//! nothing about any particular tool (that is `argus_core::adapters`, reached via `plan`).

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use argus_core::event::{Author, Event, RunOutcome};
use argus_core::ids::{CommitOid, JobId, RunId, TreeOid};
use argus_core::ledger::SourceCursor;
use argus_core::plan::{CommandSpec, Effect, Invocation, PlanEnv, Workdir};
use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};
use argus_log::BlobStore;

use crate::ledger_thread::{self, Cmd, observe};
use crate::tokens::RunTokens;
use crate::{git, pty, tail};

pub(crate) struct Running {
    pub run: RunId,
    cancel: Sender<()>,
    input: Sender<Vec<u8>>,
}

impl Running {
    pub(crate) fn new(run: RunId, cancel: Sender<()>, input: Sender<Vec<u8>>) -> Self {
        Running { run, cancel, input }
    }

    /// Sends what the human typed to the program's terminal.
    pub(crate) fn send_input(&self, bytes: Vec<u8>) {
        // A run that already ended has no terminal to type into.
        let _ = self.input.send(bytes);
    }

    /// Asks the run to stop (`RunCancelRequested` reached the host). Idempotent.
    /// Dropping the handle stops the run as well.
    pub(crate) fn cancel(&self) {
        let _ = self.cancel.send(());
    }
}

/// Where a job's runs execute and what they record into. `PlanEnv` says what to run;
/// this says where, from the job's `JobInfo` and the views at start.
#[derive(Clone)]
pub(crate) struct Workspace {
    pub home: PathBuf,
    pub blobs: BlobStore,
    /// The job the run belongs to; `None` for a run on the meta ledger.
    pub job: Option<JobPlace>,
    /// Where a process that talks to argus gets its `ARGUS_TOKEN`.
    pub tokens: RunTokens,
}

/// Where a job's runs work: its repository, worktree, and branches.
#[derive(Clone)]
pub(crate) struct JobPlace {
    pub repo: PathBuf,
    pub worktree: PathBuf,
    pub job: JobId,
    pub base: CommitOid,
    /// The head snapshot's tree when the run starts (a new snapshot's `previous`).
    pub previous: Option<TreeOid>,
    /// The branch an approved head is merged into.
    pub target: String,
}

/// How long a cancelled run has to exit before it is killed.
pub(crate) const GRACE: Duration = Duration::from_secs(10);

/// Executes `inv` on its own thread and reports back as facts: sources while it runs,
/// then `RunFinished` (by the host), then `Released`.
///
/// - `Process`: in the job worktree, home, or a throwaway checkout of the snapshot; with
///   its `watch` files followed and its artifact directory collected afterwards.
/// - `Snapshot`: `git::snapshot` of the worktree.
/// - `Import`: the file streamed through its kind's framer, as one source.
/// - `Merge`: `git::merge` of the approved snapshot into the target branch.
/// - `WriteSkill`: the skill's `SKILL.md` under its target directory.
/// - Leased (device checks) are not written yet: they finish as `Failed`, so nothing
///   waits on them forever.
pub(crate) fn start(inv: Invocation, env: PlanEnv, ws: Workspace, ledger: Sender<Cmd>) -> Running {
    let (cancel, cancelled) = mpsc::channel();
    let (input, typed) = mpsc::channel();
    let run = env.run;
    std::thread::spawn(move || {
        let mut control = Control {
            cancel: cancelled,
            grace: GRACE,
            input: Some(typed),
        };
        let outcome = execute(inv, &env, &ws, &ledger, &mut control);
        let finished = Event::RunFinished {
            run: env.run,
            attempt: env.attempt,
            outcome,
        };
        // Without the ledger the outcome is lost; the restart abandons the attempt instead.
        let _ = ledger_thread::submit(&ledger, Author::Host, finished);
        let _ = ledger.send(Cmd::Released { run: env.run });
    });
    Running { run, cancel, input }
}

fn execute(
    inv: Invocation,
    env: &PlanEnv,
    ws: &Workspace,
    ledger: &Sender<Cmd>,
    control: &mut Control,
) -> RunOutcome {
    let failed = |reason: String| RunOutcome::Failed { reason };
    let no_job = || failed("this run needs a job".into());
    match inv {
        Invocation::Process(spec) => run_process(spec, env, ws, ledger, control),
        Invocation::InProcess(Effect::Snapshot) => {
            let Some(j) = &ws.job else { return no_job() };
            let previous = j.previous.as_ref();
            match git::snapshot(&j.worktree, &j.base, previous, j.job, &env.snapshot_deny) {
                Ok(snapshot) => RunOutcome::Snapshot(snapshot),
                Err(e) => failed(e.to_string()),
            }
        }
        Invocation::InProcess(Effect::Import { transcript, kind }) => {
            match import(&transcript, kind, env, ledger) {
                Ok(()) => RunOutcome::Exited { code: 0 },
                Err(e) => failed(format!("{}: {e}", transcript.display())),
            }
        }
        Invocation::Leased { .. } => failed("device checks are not supported yet".into()),
        Invocation::InProcess(Effect::Merge(approved)) => {
            let Some(j) = &ws.job else { return no_job() };
            let (tree, base) = (approved.tree(), approved.base());
            match git::merge(&j.repo, j.job, tree, base, &j.target) {
                Ok(commit) => RunOutcome::Merged { commit },
                Err(reason) => failed(reason),
            }
        }
        Invocation::InProcess(Effect::WriteSkill(draft)) => match write_skill(&draft) {
            Ok(path) => RunOutcome::SkillWritten { path },
            Err(reason) => failed(reason),
        },
    }
}

/// `<target>/<name>/SKILL.md`: frontmatter (the description as a JSON string, which is
/// also a YAML double-quoted scalar) and the body. Written beside and renamed over, so a
/// reader never sees half a skill; the same draft again writes the same bytes.
fn write_skill(draft: &argus_core::event::SkillDraft) -> Result<PathBuf, String> {
    let dir = draft.target.join(draft.name.as_str());
    let path = dir.join("SKILL.md");
    let text = format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}",
        draft.name.as_str(),
        argus_json::write(&argus_json::Json::str(&draft.description)),
        draft.body
    );
    let fail = |e: std::io::Error| format!("{}: {e}", dir.display());
    std::fs::create_dir_all(&dir).map_err(fail)?;
    let temp = dir.join(".SKILL.md.argus-tmp");
    std::fs::write(&temp, text).map_err(fail)?;
    std::fs::rename(&temp, &path).map_err(fail)?;
    Ok(path)
}

fn run_process(
    mut spec: CommandSpec,
    env: &PlanEnv,
    ws: &Workspace,
    ledger: &Sender<Cmd>,
    control: &mut Control,
) -> RunOutcome {
    let spawn_failed = |error: String| RunOutcome::SpawnFailed { error };
    let job = |what: &str| ws.job.as_ref().ok_or(format!("{what} needs a job"));
    // Held until the process ended: dropping it removes the checkout.
    let mut checkout = None;
    let dir = match &spec.workdir {
        Workdir::Home => ws.home.clone(),
        Workdir::JobWorktree => match job("the job worktree") {
            Ok(j) => j.worktree.clone(),
            Err(e) => return spawn_failed(e),
        },
        Workdir::Snapshot { tree, base } => {
            let made = job("a snapshot checkout")
                .and_then(|j| git::materialize(&j.repo, tree, base).map_err(|e| e.to_string()));
            match made {
                Ok(made) => checkout.insert(made).path.clone(),
                Err(e) => return spawn_failed(e),
            }
        }
    };
    let artifacts = match spec.artifacts {
        true => {
            let dir = std::env::temp_dir()
                .join(format!("argus-artifacts-{}-{}", env.run.0, env.attempt.0));
            if let Err(e) = std::fs::create_dir_all(&dir) {
                return RunOutcome::SpawnFailed {
                    error: e.to_string(),
                };
            }
            let value = dir.to_string_lossy().into_owned();
            spec.env.push(("ARGUS_ARTIFACT_DIR".into(), value));
            Some(dir)
        }
        false => None,
    };
    // Held until the process ends; dropping it revokes the token.
    let mut token = None;
    if spec.env.iter().any(|(k, _)| k == "ARGUS_RUN") {
        match ws.tokens.issue(env.run) {
            Ok(issued) => {
                spec.env.push(("ARGUS_TOKEN".into(), issued.token.clone()));
                token = Some(issued);
            }
            Err(e) => {
                return RunOutcome::SpawnFailed {
                    error: e.to_string(),
                };
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = (!spec.watch.is_empty()).then(|| {
        let specs = spec.watch.clone();
        tail::watch(
            specs,
            ws.home.clone(),
            env.run,
            env.attempt,
            ledger.clone(),
            stop.clone(),
            tail::POLL,
        )
    });

    let ended = spawn_capture(&spec, &dir, env, ledger, control);
    stop.store(true, Ordering::SeqCst);
    if let Some(watcher) = watcher {
        let _ = watcher.join();
    }
    if let Some(artifacts) = &artifacts {
        let _ = collect_artifacts(artifacts, env, &ws.blobs, ledger);
        let _ = std::fs::remove_dir_all(artifacts);
    }
    drop(checkout);
    drop(token);
    match ended {
        Err(e) => RunOutcome::SpawnFailed {
            error: e.to_string(),
        },
        Ok(ended) => outcome_of(&ended),
    }
}

fn outcome_of(ended: &Ended) -> RunOutcome {
    use std::os::unix::process::ExitStatusExt;
    match (ended.cancelled, ended.status.code(), ended.status.signal()) {
        (true, _, signal) => RunOutcome::Killed {
            signal: signal.unwrap_or(15),
            cancelled: true,
        },
        (false, Some(code), _) => RunOutcome::Exited { code },
        (false, None, signal) => RunOutcome::Killed {
            signal: signal.unwrap_or(0),
            cancelled: false,
        },
    }
}

/// Streams `path` into one source of `kind`, as the tailer would, and finishes it.
fn import(
    path: &Path,
    kind: SourceKind,
    env: &PlanEnv,
    ledger: &Sender<Cmd>,
) -> std::io::Result<()> {
    let origin = SourceOrigin::File(path.to_path_buf());
    let meta = SourceMeta {
        id: SourceMeta::derive_id(env.run, env.attempt, &origin),
        run: env.run,
        attempt: env.attempt,
        kind,
        origin,
    };
    // The tailer treats a missing file as "not written yet"; an import must not.
    std::fs::metadata(path)?;
    let cursor = ledger_thread::open_source(ledger, meta)?;
    let mut tailer = tail::Tailer::new(path.to_path_buf(), &cursor);
    observe(ledger, cursor.meta.id, tailer.finish()?);
    Ok(())
}

/// Files left in `dir` (`$ARGUS_ARTIFACT_DIR`): each is put in the blob store (fsynced)
/// before the `ArtifactCaptured` that names it is submitted, in name order.
pub(crate) fn collect_artifacts(
    dir: &Path,
    env: &PlanEnv,
    blobs: &BlobStore,
    ledger: &Sender<Cmd>,
) -> std::io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for path in files {
        let (blob, len) = blobs
            .put_file(&path)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let captured = Event::ArtifactCaptured {
            run: env.run,
            attempt: env.attempt,
            blob,
            media_type: media_type(&name).to_owned(),
            name,
            len,
        };
        ledger_thread::submit(ledger, Author::Host, captured)?;
    }
    Ok(())
}

/// By extension; the viewer only needs to know images, videos, and text apart.
fn media_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("txt" | "log") => "text/plain",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
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
    /// Bytes the human typed (`RunInput`), for a program on a terminal. Taken by
    /// `spawn_capture`.
    pub input: Option<Receiver<Vec<u8>>>,
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
    control: &mut Control,
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
    // `then`, not `zip`: the pipe is taken only when there is something to write, so a
    // terminal's input is not closed by accident.
    let writer = fed
        .and_then(|bytes| stdin.take().map(|pipe| (bytes, pipe)))
        .map(|(bytes, mut pipe)| {
            // A thread, so a process that writes before it reads cannot deadlock the host.
            std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            })
        });
    // What the human types goes to the terminal as it arrives. The thread holds the
    // pipe (so the terminal's input stays open) and ends with the run's `Running`.
    if spec.terminal
        && let Some(typed) = control.input.take()
        && let Some(mut pipe) = stdin.take()
    {
        std::thread::spawn(move || {
            for bytes in typed {
                if pipe.write_all(&bytes).and_then(|()| pipe.flush()).is_err() {
                    return;
                }
            }
        });
    }
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
                // A dropped `Running` (the host is going away, or its caller gave up) is
                // a cancel too: no process outlives the handle that could stop it.
                let cancel = !matches!(control.cancel.try_recv(), Err(TryRecvError::Empty));
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

/// Whether `pgid` may be signalled as a group. `kill -- -0` means the caller's own group
/// and `kill -- -1` every process the user owns (it logged a desktop session out once,
/// under a mutant that made `children` return `[1]`). Neither, nor this host's own group,
/// is ever a run's.
fn may_signal(pgid: u32, own_group: Option<u32>) -> bool {
    pgid > 1 && Some(pgid) != own_group
}

/// This process's group (`/proc/self/stat` field 5; the name in field 2 may hold spaces
/// and parentheses, so fields are counted after the last `)`).
fn own_group() -> Option<u32> {
    parse_pgrp(&std::fs::read_to_string("/proc/self/stat").ok()?)
}

fn parse_pgrp(stat: &str) -> Option<u32> {
    let (_, after_name) = stat.rsplit_once(')')?;
    // After the name: state (3), ppid (4), pgrp (5).
    after_name.split_whitespace().nth(2)?.parse().ok()
}

/// `kill -<signal> -<pgid>`: std can only SIGKILL one process, not ask a group to exit.
fn signal_group(pgid: u32, signal: &str) {
    if !may_signal(pgid, own_group()) {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger_thread::testing::{Received, bytes, fake_ledger};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use argus_core::ids::{Attempt, Id128};
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
        let (_cancel, mut control) = control(Duration::from_secs(5));
        let (ended, got) = capture_with(spec, dir, &mut control);
        assert!(!ended.cancelled);
        (ended.status, got)
    }

    fn control(grace: Duration) -> (Sender<()>, Control) {
        let (tx, cancel) = std::sync::mpsc::channel();
        let control = Control {
            cancel,
            grace,
            input: None,
        };
        (tx, control)
    }

    fn capture_with(spec: &CommandSpec, dir: &Path, control: &mut Control) -> (Ended, Received) {
        let (ledger, thread) = fake_ledger();
        let ended = spawn_capture(spec, dir, &env(), &ledger, control).unwrap();
        drop(ledger);
        (ended, thread.join().unwrap())
    }

    // ------------------------------------------------------------ start

    use argus_core::event::{Author, Event, RunOutcome};
    use argus_core::plan::{Effect, Invocation};

    fn workspace(worktree: &Path) -> Workspace {
        let data = worktree.join(".argus-data");
        Workspace {
            home: worktree.to_path_buf(),
            blobs: argus_log::LogDir::new(data).blobs(),
            job: Some(JobPlace {
                repo: worktree.to_path_buf(),
                worktree: worktree.to_path_buf(),
                job: argus_core::ids::JobId(Id128(1)),
                base: CommitOid::parse(&"b".repeat(40)).unwrap(),
                previous: None,
                target: "release".into(),
            }),
            tokens: RunTokens::default(),
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("argus-start-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs `inv` to its end and returns what the ledger received.
    fn run_to_end(inv: Invocation, ws: Workspace) -> Received {
        let (ledger, thread) = fake_ledger();
        let running = start(inv, env(), ws, ledger.clone());
        assert_eq!(running.run, RunId(Id128(1)));
        drop(ledger);
        // Held until the run reported back: dropping it would stop the run.
        let got = thread.join().unwrap();
        drop(running);
        got
    }

    #[test]
    fn a_run_whose_handle_is_dropped_is_stopped() {
        let dir = scratch_dir("dropped");
        let (ledger, thread) = fake_ledger();
        let started = Instant::now();
        let running = start(
            Invocation::Process(sh(BUSY)),
            env(),
            workspace(&dir),
            ledger.clone(),
        );
        drop(ledger);
        std::thread::sleep(Duration::from_millis(200));
        drop(running);
        let got = thread.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                finished(&got),
                RunOutcome::Killed {
                    cancelled: true,
                    ..
                }
            ),
            "{:?}",
            finished(&got)
        );
    }

    fn finished(got: &Received) -> RunOutcome {
        match got.events.last() {
            Some((
                Author::Host,
                Event::RunFinished {
                    run,
                    attempt,
                    outcome,
                },
            )) => {
                assert_eq!((*run, *attempt), (RunId(Id128(1)), Attempt(0)));
                outcome.clone()
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_process_run_finishes_with_its_exit_code() {
        let dir = scratch_dir("exit");
        let got = run_to_end(Invocation::Process(sh("echo hi; exit 3")), workspace(&dir));
        assert_eq!(finished(&got), RunOutcome::Exited { code: 3 });
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            b"hi\n"
        );
    }

    #[test]
    fn a_program_that_cannot_start_is_a_spawn_failure() {
        let dir = scratch_dir("spawn");
        let mut spec = sh("");
        spec.program = PathBuf::from("/nonexistent/argus-test-program");
        let got = run_to_end(Invocation::Process(spec), workspace(&dir));
        assert!(
            matches!(finished(&got), RunOutcome::SpawnFailed { .. }),
            "{:?}",
            finished(&got)
        );
    }

    #[test]
    fn a_process_that_talks_to_argus_holds_a_token_only_while_it_runs() {
        let dir = scratch_dir("token");
        let mut spec = sh(
            "printf %s \"$ARGUS_TOKEN\" > tok.tmp && mv tok.tmp tok; while [ ! -e go ]; do sleep 0.02; done",
        );
        spec.env.push(("ARGUS_RUN".into(), "1".into()));
        let ws = workspace(&dir);
        let tokens = ws.tokens.clone();
        let (ledger, thread) = fake_ledger();
        let running = start(Invocation::Process(spec), env(), ws, ledger.clone());
        drop(ledger);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.join("tok").exists() {
            assert!(
                Instant::now() < deadline,
                "the process never wrote its token"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let token = std::fs::read_to_string(dir.join("tok")).unwrap();
        assert_eq!(token.len(), 64);
        assert!(tokens.check(RunId(Id128(1)), &token));
        std::fs::write(dir.join("go"), "").unwrap();
        let got = thread.join().unwrap();
        drop(running);
        assert_eq!(finished(&got), RunOutcome::Exited { code: 0 });
        assert!(!tokens.check(RunId(Id128(1)), &token));
    }

    #[test]
    fn a_process_that_does_not_talk_to_argus_gets_no_token() {
        let dir = scratch_dir("no-token");
        let got = run_to_end(
            Invocation::Process(sh("test -z \"$ARGUS_TOKEN\"")),
            workspace(&dir),
        );
        assert_eq!(finished(&got), RunOutcome::Exited { code: 0 });
    }

    #[test]
    fn files_left_in_the_artifact_directory_are_stored_before_the_run_finishes() {
        let dir = scratch_dir("artifacts");
        let mut spec = sh(
            "printf png > \"$ARGUS_ARTIFACT_DIR/shot.png\"; printf log > \"$ARGUS_ARTIFACT_DIR/a.txt\"",
        );
        spec.artifacts = true;
        let ws = workspace(&dir);
        let got = run_to_end(Invocation::Process(spec), ws.clone());
        assert_eq!(finished(&got), RunOutcome::Exited { code: 0 });
        let captured: Vec<_> = got
            .events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::ArtifactCaptured {
                    name,
                    media_type,
                    len,
                    blob,
                    ..
                } => Some((
                    name.clone(),
                    media_type.clone(),
                    *len,
                    std::fs::read(ws.blobs.path(blob)).unwrap(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            captured,
            vec![
                (
                    "a.txt".to_owned(),
                    "text/plain".to_owned(),
                    3,
                    b"log".to_vec()
                ),
                (
                    "shot.png".to_owned(),
                    "image/png".to_owned(),
                    3,
                    b"png".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn a_cancelled_run_finishes_as_killed_by_request() {
        let dir = scratch_dir("cancel");
        let (ledger, thread) = fake_ledger();
        let running = start(
            Invocation::Process(sh(BUSY)),
            env(),
            workspace(&dir),
            ledger.clone(),
        );
        std::thread::sleep(Duration::from_millis(200));
        running.cancel();
        drop(ledger);
        let got = thread.join().unwrap();
        assert!(
            matches!(
                finished(&got),
                RunOutcome::Killed {
                    cancelled: true,
                    ..
                }
            ),
            "{:?}",
            finished(&got)
        );
    }

    #[test]
    fn an_import_streams_the_file_as_one_source() {
        let dir = scratch_dir("import");
        let file = dir.join("old.jsonl");
        std::fs::write(&file, "{\"a\":1}\n{\"b\":2}").unwrap();
        let inv = Invocation::InProcess(Effect::Import {
            transcript: file.clone(),
            kind: SourceKind::ClaudeTranscript,
        });
        let got = run_to_end(inv, workspace(&dir));
        assert_eq!(finished(&got), RunOutcome::Exited { code: 0 });
        let (kind, frames) = by_origin(&got, SourceOrigin::File(file)).unwrap();
        assert_eq!(kind, SourceKind::ClaudeTranscript);
        assert_eq!(frames, vec![b"{\"a\":1}\n".to_vec(), b"{\"b\":2}".to_vec()]);
    }

    #[test]
    fn artifacts_are_typed_by_extension() {
        let cases = [
            ("shot.PNG", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("run.mp4", "video/mp4"),
            ("run.webm", "video/webm"),
            ("out.log", "text/plain"),
            ("a.txt", "text/plain"),
            ("r.json", "application/json"),
            ("trace", "application/octet-stream"),
            ("x.bin", "application/octet-stream"),
        ];
        for (name, want) in cases {
            assert_eq!(media_type(name), want, "{name}");
        }
    }

    #[test]
    fn a_home_run_runs_in_home_and_its_watched_files_are_followed() {
        let dir = scratch_dir("home");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("notes")).unwrap();
        let mut ws = workspace(&dir);
        ws.home = home.clone();
        let mut spec = sh("basename \"$PWD\"; printf 'seen\\n' > notes/run.jsonl");
        spec.workdir = Workdir::Home;
        spec.watch = vec![argus_core::plan::WatchSpec {
            pattern: "notes/*.jsonl".into(),
            kind: SourceKind::ClaudeTranscript,
        }];
        let got = run_to_end(Invocation::Process(spec), ws);
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            b"home\n"
        );
        let watched = by_origin(&got, SourceOrigin::File(home.join("notes/run.jsonl"))).unwrap();
        assert_eq!(
            watched,
            (SourceKind::ClaudeTranscript, vec![b"seen\n".to_vec()])
        );
    }

    #[test]
    fn importing_a_missing_file_fails() {
        let dir = scratch_dir("import-missing");
        let inv = Invocation::InProcess(Effect::Import {
            transcript: dir.join("nowhere.jsonl"),
            kind: SourceKind::ClaudeTranscript,
        });
        let got = run_to_end(inv, workspace(&dir));
        assert!(
            matches!(finished(&got), RunOutcome::Failed { .. }),
            "{:?}",
            finished(&got)
        );
    }

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
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A repository whose worktree changed `a.txt` after the base commit.
    fn job_repo(name: &str) -> Workspace {
        let dir = scratch_dir(name);
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "base"]);
        let mut ws = workspace(&dir);
        ws.job.as_mut().unwrap().base =
            CommitOid::parse(&git(&dir, &["rev-parse", "HEAD"])).unwrap();
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        ws
    }

    /// An approval token, as only the review board mints them: a snapshot of `tree`,
    /// then the human's approval of it.
    fn approved(
        tree: &argus_core::ids::TreeOid,
        base: &CommitOid,
    ) -> argus_core::event::ApprovedHead {
        use argus_core::event::{Decision, RunPurpose, Snapshot};
        use argus_core::fact::{Fact, FactBody};
        use argus_core::ids::{DecisionId, EventId, FactId, Stamp, UnixMillis};
        use argus_core::views::Projection;
        use argus_core::views::review::ReviewBoard;
        let mut board = ReviewBoard::default();
        let run = RunId(Id128(9));
        let snapshot = Snapshot {
            tree: tree.clone(),
            base: base.clone(),
            previous: None,
            delta: Vec::new(),
            files: Vec::new(),
            withheld: Vec::new(),
        };
        let events = [
            (
                Author::Policy,
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Snapshot { after: None },
                },
            ),
            (
                Author::Host,
                Event::RunStarted {
                    run,
                    attempt: Attempt(0),
                    pid: None,
                },
            ),
            (
                Author::Host,
                Event::RunFinished {
                    run,
                    attempt: Attempt(0),
                    outcome: RunOutcome::Snapshot(snapshot),
                },
            ),
            (
                Author::Human,
                Event::Decided {
                    decision: DecisionId(Id128(1)),
                    tree: Some(tree.clone()),
                    verdict: Decision::Approve,
                    note: String::new(),
                },
            ),
        ];
        for (n, (author, event)) in events.iter().enumerate() {
            let fact = Fact {
                id: FactId {
                    event: EventId::parse(&format!("host:{n}")).unwrap(),
                    part: 0,
                },
                stamp: Stamp {
                    wall_ns: n as u64 + 1,
                    logical: 0,
                },
                at: UnixMillis(n as i64),
                body: FactBody::Authored { author, event },
            };
            board.apply(&fact);
        }
        board.approved_head().unwrap()
    }

    #[test]
    fn a_merge_run_brings_the_approved_snapshot_into_the_target() {
        let ws = job_repo("merge");
        let j = ws.job.clone().unwrap();
        git(&j.repo, &["branch", &j.target, j.base.as_str()]);
        let got = run_to_end(Invocation::InProcess(Effect::Snapshot), ws.clone());
        let RunOutcome::Snapshot(s) = finished(&got) else {
            panic!()
        };
        let token = approved(&s.tree, &s.base);
        let got = run_to_end(Invocation::InProcess(Effect::Merge(token)), ws.clone());
        let RunOutcome::Merged { commit } = finished(&got) else {
            panic!("{:?}", finished(&got))
        };
        assert_eq!(git(&j.repo, &["rev-parse", &j.target]), commit.as_str());
        assert_eq!(
            git(&j.repo, &["rev-parse", &format!("{}^{{tree}}", j.target)]),
            s.tree.as_str()
        );
    }

    #[test]
    fn a_merge_that_cannot_happen_fails_with_its_reason() {
        let ws = job_repo("merge-missing");
        let got = run_to_end(Invocation::InProcess(Effect::Snapshot), ws.clone());
        let RunOutcome::Snapshot(s) = finished(&got) else {
            panic!()
        };
        // No `release` branch exists in this repository.
        let got = run_to_end(
            Invocation::InProcess(Effect::Merge(approved(&s.tree, &s.base))),
            ws,
        );
        assert_eq!(
            finished(&got),
            RunOutcome::Failed {
                reason: "no branch release".into()
            }
        );
    }

    #[test]
    fn a_skill_write_puts_the_skill_file_in_its_target_and_can_be_repeated() {
        use argus_core::event::SkillDraft;
        use argus_core::ids::SkillName;
        let dir = scratch_dir("skill");
        let target = dir.join("skills");
        let draft = SkillDraft {
            from: None,
            name: SkillName::parse("run-fmt-first").unwrap(),
            description: "Use before tests: fmt fails \"often\".".into(),
            body: "Run `cargo fmt` before `cargo test`.\n".into(),
            target: target.clone(),
        };
        let path = target.join("run-fmt-first").join("SKILL.md");
        let want = "---\nname: run-fmt-first\ndescription: \"Use before tests: fmt fails \\\"often\\\".\"\n---\n\nRun `cargo fmt` before `cargo test`.\n";
        for _ in 0..2 {
            let got = run_to_end(
                Invocation::InProcess(Effect::WriteSkill(draft.clone())),
                workspace(&dir),
            );
            assert_eq!(
                finished(&got),
                RunOutcome::SkillWritten { path: path.clone() }
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
        }
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["SKILL.md"]);
    }

    #[test]
    fn a_skill_write_that_cannot_happen_fails_with_its_reason() {
        use argus_core::event::SkillDraft;
        use argus_core::ids::SkillName;
        let dir = scratch_dir("skill-blocked");
        // A file where the target directory should be.
        std::fs::write(dir.join("skills"), "").unwrap();
        let draft = SkillDraft {
            from: None,
            name: SkillName::parse("x").unwrap(),
            description: String::new(),
            body: "b".into(),
            target: dir.join("skills"),
        };
        let got = run_to_end(
            Invocation::InProcess(Effect::WriteSkill(draft)),
            workspace(&dir),
        );
        let RunOutcome::Failed { reason } = finished(&got) else {
            panic!("{:?}", finished(&got))
        };
        let target = dir.join("skills").display().to_string();
        assert!(reason.contains(&target), "{reason}");
    }

    #[test]
    fn a_snapshot_run_finishes_with_the_snapshot() {
        let ws = job_repo("snapshot");
        let j = ws.job.clone().unwrap();
        let got = run_to_end(Invocation::InProcess(Effect::Snapshot), ws.clone());
        let RunOutcome::Snapshot(s) = finished(&got) else {
            panic!("{:?}", finished(&got))
        };
        assert_eq!(s.base, j.base);
        let blob = git(&j.repo, &["show", &format!("{}:a.txt", s.tree.as_str())]);
        assert_eq!(blob, "changed");
    }

    #[test]
    fn a_checker_runs_in_a_checkout_of_its_snapshot() {
        let ws = job_repo("checkout");
        let j = ws.job.clone().unwrap();
        let got = run_to_end(Invocation::InProcess(Effect::Snapshot), ws.clone());
        let RunOutcome::Snapshot(s) = finished(&got) else {
            panic!()
        };
        // The agent edits again; the checker must judge the snapshot, not this.
        std::fs::write(j.worktree.join("a.txt"), "later\n").unwrap();
        let mut spec = sh("cat a.txt; basename \"$PWD\"");
        spec.workdir = Workdir::Snapshot {
            tree: s.tree,
            base: s.base,
        };
        let got = run_to_end(Invocation::Process(spec), ws.clone());
        let out =
            String::from_utf8(by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat()).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("changed"));
        assert!(
            lines.next().unwrap().starts_with("argus-checkout-"),
            "{out}"
        );
    }

    /// Loops for about five seconds unless stopped.
    const BUSY: &str = "i=0; while [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done";

    #[test]
    fn what_the_human_types_reaches_the_program_on_the_terminal() {
        let mut spec = sh("read line; echo \"got:$line\"");
        spec.terminal = true;
        let (_cancel, mut control) = control(Duration::from_secs(5));
        let (typed, input) = std::sync::mpsc::channel();
        control.input = Some(input);
        // Enter on a terminal is a carriage return.
        typed.send(b"hello\r".to_vec()).unwrap();
        let started = Instant::now();
        let (ended, got) = capture_with(&spec, Path::new("/"), &mut control);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(ended.status.success());
        let screen = by_origin(&got, SourceOrigin::Terminal).unwrap().1.concat();
        assert!(
            String::from_utf8_lossy(&screen).contains("got:hello"),
            "{screen:?}"
        );
    }

    #[test]
    fn a_cancel_asks_the_process_to_exit_first() {
        let spec = sh(&format!("trap 'echo bye; exit 0' TERM; {BUSY}"));
        let (cancel, mut control) = control(Duration::from_secs(5));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.send(()).unwrap();
        });
        let started = Instant::now();
        let (ended, got) = capture_with(&spec, Path::new("/"), &mut control);
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
        let (cancel, mut control) = control(Duration::from_secs(5));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.send(()).unwrap();
        });
        let started = Instant::now();
        let (ended, got) = capture_with(&spec, Path::new("/"), &mut control);
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

    /// The grace is for cleaning up: a process that takes a while to exit gets it.
    #[test]
    fn a_process_that_takes_a_moment_to_exit_is_given_the_grace() {
        let spec = sh(&format!("trap 'sleep 0.3; echo bye; exit 0' TERM; {BUSY}"));
        let (cancel, mut control) = control(Duration::from_secs(5));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.send(()).unwrap();
        });
        let (ended, got) = capture_with(&spec, Path::new("/"), &mut control);
        assert!(ended.cancelled);
        assert_eq!(ended.status.code(), Some(0));
        assert_eq!(
            by_origin(&got, SourceOrigin::Stdout).unwrap().1.concat(),
            b"bye\n"
        );
    }

    #[test]
    fn a_process_that_ends_within_its_timeout_is_left_alone() {
        let mut spec = sh("sleep 0.1; exit 0");
        spec.timeout = Some(Duration::from_secs(10));
        let (status, _) = capture(&spec, Path::new("/"));
        assert!(status.success());
    }

    #[test]
    fn a_process_that_ignores_the_request_is_killed_after_the_grace() {
        let spec = sh(&format!("trap '' TERM; {BUSY}"));
        let (cancel, mut control) = control(Duration::from_millis(300));
        // After the shell installed its trap: a signal before that would end it at once.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.send(()).unwrap();
        });
        let started = Instant::now();
        let (ended, _) = capture_with(&spec, Path::new("/"), &mut control);
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
        got.sources
            .values()
            .find(|(meta, _)| meta.origin == origin)
            .map(|(meta, frames)| (meta.kind, bytes(frames)))
    }

    // These never call `kill`: a broken check must not be able to signal anything.
    #[test]
    fn only_a_run_s_own_group_may_be_signalled() {
        let own = Some(4242);
        assert!(!may_signal(0, own), "0 is the caller's own group");
        assert!(!may_signal(1, own), "-1 is every process the user owns");
        assert!(!may_signal(4242, own), "the host's own group");
        assert!(may_signal(2, own));
        assert!(may_signal(98765, own));
        // Without knowing its own group, the host still refuses 0 and 1.
        assert!(!may_signal(1, None));
        assert!(may_signal(98765, None));
    }

    #[test]
    fn the_own_group_is_read_after_the_process_name() {
        assert_eq!(parse_pgrp("123 (cat) S 1 77 77 0 -1"), Some(77));
        // A name may contain spaces and parentheses.
        assert_eq!(parse_pgrp("123 (a b) c) S 1 88 88 0 -1"), Some(88));
        assert_eq!(parse_pgrp("garbage"), None);
        assert!(own_group().is_some());
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
        for (id, (meta, _)) in &got.sources {
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

    // ------------------------------------------------------------ real Claude Code

    /// What a live run has sent so far, readable while it runs.
    #[derive(Default)]
    struct Live {
        sources: std::collections::BTreeMap<argus_core::ids::SourceId, (SourceMeta, Vec<u8>)>,
        finished: Option<RunOutcome>,
    }

    fn live_ledger() -> (Sender<Cmd>, std::sync::Arc<std::sync::Mutex<Live>>) {
        use argus_core::ledger::{SourceCursor, Submitted};
        use argus_core::source::RawFrame;
        let (tx, rx) = std::sync::mpsc::channel();
        let live = std::sync::Arc::new(std::sync::Mutex::new(Live::default()));
        let shared = live.clone();
        std::thread::spawn(move || {
            for cmd in rx {
                let mut live = shared.lock().unwrap();
                match cmd {
                    Cmd::OpenSource { meta, reply } => {
                        live.sources
                            .entry(meta.id)
                            .or_insert((meta.clone(), Vec::new()));
                        let _ = reply.send(Ok(SourceCursor {
                            meta,
                            resume_at: 0,
                            last: None,
                        }));
                    }
                    Cmd::Observe { source, frames } => {
                        for frame in frames {
                            if let RawFrame::Bytes { bytes, .. } = frame {
                                live.sources.get_mut(&source).unwrap().1.extend(bytes);
                            }
                        }
                    }
                    Cmd::Submit { event, reply, .. } => {
                        if let Event::RunFinished { outcome, .. } = event {
                            live.finished = Some(outcome);
                        }
                        let id = argus_core::ids::EventId::parse("host:0").unwrap();
                        let _ = reply.send(Ok(Submitted::Appended(id)));
                    }
                    _ => {}
                }
            }
        });
        (tx, live)
    }

    /// Terminal output without escape sequences or whitespace.
    fn visible(screen: &str) -> String {
        let mut out = String::new();
        let mut chars = screen.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // CSI: up to a final byte in @..~; OSC: up to BEL or ST; others: one char.
                match chars.next() {
                    Some('[') => {
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        for c in chars.by_ref() {
                            if c == '\x07' || c == '\x1b' {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }

    /// Runs real `claude` (haiku) under a PTY in a fresh repository until its transcript
    /// shows `done`, answering the folder trust question if it is asked; then cancels it.
    fn real_claude(
        ws: &Workspace,
        run: RunId,
        prompt: &str,
        resume: Option<RunId>,
        done: &str,
    ) -> (RunOutcome, String, Vec<String>) {
        use argus_core::event::{AgentLaunch, RunPurpose};
        use argus_core::models::AgentCli;
        let mut env = env();
        env.run = run;
        let purpose = RunPurpose::Agent(AgentLaunch {
            cli: AgentCli::Claude,
            model: Some(argus_core::ids::ModelId("haiku".into())),
            prompt: prompt.into(),
            resume,
        });
        let inv = argus_core::plan::plan(&purpose, &env);
        let (ledger, live) = live_ledger();
        let running = start(inv, env, ws.clone(), ledger);
        let deadline = Instant::now() + Duration::from_secs(180);
        let (mut moved, mut trusted) = (false, false);
        let transcript = loop {
            std::thread::sleep(Duration::from_millis(500));
            let live = live.lock().unwrap();
            if Instant::now() >= deadline {
                for (meta, bytes) in live.sources.values() {
                    let text = String::from_utf8_lossy(bytes);
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(1500)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    eprintln!("--- {:?} ({} bytes)\n{tail}", meta.origin, bytes.len());
                }
                panic!("no answer in time");
            }
            if live.finished.is_some() {
                for (meta, bytes) in live.sources.values() {
                    eprintln!(
                        "--- {:?}\n{}",
                        meta.origin,
                        visible(&String::from_utf8_lossy(bytes))
                    );
                }
                panic!("ended early: {:?}", live.finished);
            }
            let screen: String = live
                .sources
                .values()
                .filter(|(m, _)| m.origin == SourceOrigin::Terminal)
                .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
                .collect();
            // The screen draws spaces as cursor moves: compare without escapes or spaces.
            // Keys sent before the prompt is fully drawn are misread, so wait for it, then
            // move to "Yes" and confirm only once the marker is on it.
            let shown = visible(&screen);
            let marker = shown.rfind('❯').map(|i| &shown[i..]);
            if !trusted && shown.contains("Entertoconfirm") {
                if marker.is_some_and(|m| m.starts_with("❯Yes")) {
                    running.send_input(b"\r".to_vec());
                    trusted = true;
                } else if !moved {
                    std::thread::sleep(Duration::from_secs(1));
                    running.send_input(b"\x1b[B".to_vec());
                    moved = true;
                }
            }
            let answered = live.sources.values().find(|(m, b)| {
                matches!(m.origin, SourceOrigin::File(_))
                    && String::from_utf8_lossy(b).contains(done)
                    && String::from_utf8_lossy(b).contains("end_turn")
            });
            if let Some((meta, bytes)) = answered {
                let SourceOrigin::File(path) = &meta.origin else {
                    unreachable!()
                };
                break (
                    path.display().to_string(),
                    String::from_utf8_lossy(bytes).into_owned(),
                );
            }
        };
        running.cancel();
        let deadline = Instant::now() + Duration::from_secs(30);
        let outcome = loop {
            if let Some(outcome) = live.lock().unwrap().finished.clone() {
                break outcome;
            }
            assert!(Instant::now() < deadline, "did not exit after the cancel");
            std::thread::sleep(Duration::from_millis(200));
        };
        let files: Vec<String> = live
            .lock()
            .unwrap()
            .sources
            .values()
            .filter_map(|(m, _)| match &m.origin {
                SourceOrigin::File(p) => Some(p.display().to_string()),
                _ => None,
            })
            .collect();
        (
            outcome,
            transcript.0,
            vec![transcript.1].into_iter().chain(files).collect(),
        )
    }

    /// `cargo test -p argus-host --lib real_claude -- --ignored --nocapture`. Costs two
    /// small haiku turns and leaves two sessions in ~/.claude/projects.
    #[test]
    #[ignore = "runs the real claude CLI, which costs a little and writes ~/.claude"]
    fn real_claude_answers_under_a_pty_and_feedback_resumes_a_copy_of_its_session() {
        let mut ws = job_repo("real-claude");
        ws.home = PathBuf::from(std::env::var_os("HOME").unwrap());
        // Derived run ids are UUID v8; Claude must accept them as session ids.
        let first = RunId(Id128::derive("e2e", &std::process::id().to_le_bytes()));
        let (outcome, path, texts) = real_claude(
            &ws,
            first,
            "Reply with exactly the word pong and nothing else.",
            None,
            "pong",
        );
        eprintln!("first: {outcome:?} {path}");
        assert!(path.ends_with(&format!("{}.jsonl", first.0)), "{path}");
        assert!(
            matches!(
                outcome,
                RunOutcome::Killed {
                    cancelled: true,
                    ..
                }
            ),
            "{outcome:?}"
        );

        let second = RunId(Id128::derive(
            "e2e-resume",
            &std::process::id().to_le_bytes(),
        ));
        let (outcome, path, texts2) = real_claude(
            &ws,
            second,
            "Now reply with exactly the word again and nothing else.",
            Some(first),
            "again",
        );
        eprintln!("second: {outcome:?} {path} files={:?}", &texts2[1..]);
        // A copy of the earlier session continues under the new run's id.
        assert!(path.ends_with(&format!("{}.jsonl", second.0)), "{path}");
        assert!(
            texts2[0].contains("pong"),
            "the earlier turn is in the new transcript"
        );
        assert!(
            matches!(
                outcome,
                RunOutcome::Killed {
                    cancelled: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let _ = texts;
    }
}
