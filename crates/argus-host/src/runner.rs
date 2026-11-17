//! Executes an `Invocation` and reports back as facts. Knows how to run a process; knows
//! nothing about any particular tool (that is `argus_core::adapters`, reached via `plan`).

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

pub(crate) fn spawn_capture(
    spec: &CommandSpec,
    env: &PlanEnv,
    ledger: &Sender<Cmd>,
) -> std::io::Result<std::process::ExitStatus> {
    todo!("not implemented")
}

pub(crate) fn run_effect(effect: Effect, env: &PlanEnv, ledger: &Sender<Cmd>) {
    todo!("not implemented")
}
