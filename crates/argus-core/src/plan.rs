//! Turning a run's purpose into a concrete invocation. Pure: no process is spawned here.
//! The host executes whatever comes back with one generic runner, so all knowledge of
//! external tools (flags, output formats, lease commands) stays in `adapters/`.

use std::path::PathBuf;
use std::time::Duration;

use crate::adapters;
use crate::event::*;
use crate::ids::*;
use crate::models::AgentCli;
use crate::source::SourceKind;

#[derive(Clone, Debug)]
pub enum Invocation {
    Process(CommandSpec),
    /// A process that needs a billed remote resource for its whole lifetime. The host runs
    /// the lease protocol (see `LeaseSpec`) around `inner`.
    Leased {
        lease: Box<LeaseSpec>,
        inner: CommandSpec,
    },
    InProcess(Effect),
}

#[derive(Clone, Debug)]
pub enum Effect {
    /// `git::snapshot` honoring git's ignore rules plus `PlanEnv::snapshot_deny`.
    Snapshot,
    /// `git merge-tree --write-tree` of the approved snapshot commit and the target
    /// branch, `commit-tree`, then `update-ref <target> <new> <old>` (compare-and-swap).
    /// The user's checkouts are never touched; a conflict or a moved target -> `Failed`.
    Merge(ApprovedHead),
    WriteSkill(SkillDraft),
    /// Stream an existing file into a source of this kind, then finish.
    Import {
        transcript: PathBuf,
        kind: SourceKind,
    },
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: Workdir,
    /// Written to the process and also captured verbatim as a `Stdin` source.
    pub stdin: Option<Vec<u8>>,
    pub stdout: SourceKind,
    pub stderr: SourceKind,
    /// Run under a PTY; output becomes one `Terminal` source instead of stdout/stderr.
    pub terminal: bool,
    /// Files the process writes that are sources in their own right (agent transcripts).
    pub watch: Vec<WatchSpec>,
    /// Export `ARGUS_ARTIFACT_DIR`; files left there become `ArtifactCaptured` events.
    pub artifacts: bool,
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub enum Workdir {
    JobWorktree,
    /// A throwaway checkout of exactly this tree on top of `base`, so reviewers and
    /// checkers judge the recorded snapshot, not whatever the agent is editing now.
    Snapshot {
        tree: TreeOid,
        base: CommitOid,
    },
    Home,
}

/// `$HOME`-relative glob, e.g. `.claude/projects/*/<uuid>.jsonl`.
#[derive(Clone, Debug)]
pub struct WatchSpec {
    pub pattern: String,
    pub kind: SourceKind,
}

/// The lease protocol, executed by the host (`argus-host/src/lease.rs`):
///
/// 1. append `LeaseOpened` (intent) and fsync;
/// 2. run `acquire`; its stdout carries secrets (`remoteConfig`), so it is parsed for the
///    remote id and dropped, never captured as a source;
/// 3. append `LeaseBound { remote_id }`;
/// 4. run `inner` (and `follow` alongside, captured as `EasEvents`);
/// 5. run `collect` into the artifact dir, then `release`; append `LeaseClosed`.
///
/// Recovery: `Leases::unclosed()` lists leases to release on restart. An `Opened` lease
/// without `Bound` is resolved with `find` (sessions are tagged with the lease name).
#[derive(Clone, Debug)]
pub struct LeaseSpec {
    pub lease: LeaseId,
    pub provider: LeaseProvider,
    pub name: String,
    pub acquire: CommandSpec,
}

impl LeaseProvider {
    pub fn parse_acquired(&self, stdout: &[u8]) -> Option<(String, Option<String>)> {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::parse_started(stdout),
        }
    }
    pub fn follow(&self, remote_id: &str, env: &PlanEnv) -> Option<CommandSpec> {
        match self {
            LeaseProvider::EasSimulator { .. } => {
                Some(adapters::eas::follow_events(remote_id, env))
            }
        }
    }
    pub fn collect(&self, remote_id: &str, env: &PlanEnv) -> Option<CommandSpec> {
        match self {
            LeaseProvider::EasSimulator { platform } => {
                adapters::eas::collect_logs(*platform, remote_id, env)
            }
        }
    }
    pub fn release(&self, remote_id: &str, env: &PlanEnv) -> CommandSpec {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::stop(remote_id, env),
        }
    }
    pub fn find(&self, name: &str, env: &PlanEnv) -> CommandSpec {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::find_by_tag(name, env),
        }
    }
    pub fn parse_found(&self, stdout: &[u8]) -> Vec<String> {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::parse_list(stdout),
        }
    }
}

/// Paths of external tools, resolved once at host start (Nix profile).
#[derive(Clone, Debug)]
pub struct ToolPaths {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub java: PathBuf,
    pub tla2tools_jar: PathBuf,
    pub tlars: PathBuf,
    pub lake: PathBuf,
    /// `eas` (eas-cli); authenticated via `eas login` or `EXPO_TOKEN`.
    pub eas: PathBuf,
}

/// Untracked files a snapshot never stages, on top of git's own ignore rules (secrets an
/// agent or a tool dropped into the worktree). Config may add patterns, not remove these.
/// Tracked files are always included: the repo owner chose to track them.
pub const SNAPSHOT_DENY: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.keystore",
    "id_*",
    ".netrc",
    ".npmrc",
];

#[derive(Clone, Debug)]
pub struct PlanEnv {
    pub job: Option<JobId>,
    pub run: RunId,
    pub attempt: Attempt,
    /// Socket the `argus` CLI / MCP server inside the agent talks to.
    pub socket: PathBuf,
    pub tools: ToolPaths,
    /// `SNAPSHOT_DENY` plus the user's additions.
    pub snapshot_deny: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermissionAnswer {
    Allow,
    /// Allow, and do not ask again for this kind of call in this session.
    AllowAlways,
    Deny,
}

/// The keystrokes that answer an agent's permission prompt. They are sent as an ordinary
/// `RunInput`, so the raw log shows exactly what was typed. The adapter owns the keys.
pub fn permission_input(cli: AgentCli, answer: PermissionAnswer) -> Vec<u8> {
    match cli {
        AgentCli::Claude => adapters::claude::permission_keys(answer),
        AgentCli::Codex => adapters::codex::permission_keys(answer),
    }
}

/// Exhaustive over `RunPurpose`; the only place a purpose becomes a command.
pub fn plan(purpose: &RunPurpose, env: &PlanEnv) -> Invocation {
    match purpose {
        RunPurpose::Agent(launch) => Invocation::Process(match launch.cli {
            AgentCli::Claude => adapters::claude::launch(launch, env),
            AgentCli::Codex => adapters::codex::launch(launch, env),
        }),
        RunPurpose::Snapshot { .. } => Invocation::InProcess(Effect::Snapshot),
        RunPurpose::Merge(approved) => Invocation::InProcess(Effect::Merge(approved.clone())),
        RunPurpose::Review(spec) => Invocation::Process(adapters::llm::review(spec, env)),
        RunPurpose::Check(req) => match &req.spec {
            CheckSpec::Tla { .. } => Invocation::Process(adapters::tlc::command(req, env)),
            CheckSpec::Lean { .. } => Invocation::Process(adapters::lean::command(req, env)),
            CheckSpec::Device { .. } => adapters::eas::device_check(req, env),
        },
        RunPurpose::Summarize { llm, .. } | RunPurpose::Insight { llm, .. } => Invocation::Process(
            adapters::llm::headless(llm, adapters::llm::ToolAccess::None, Workdir::Home, env),
        ),
        RunPurpose::WriteSkill(draft) => Invocation::InProcess(Effect::WriteSkill(draft.clone())),
        RunPurpose::Import { transcript, cli } => Invocation::InProcess(Effect::Import {
            transcript: transcript.clone(),
            kind: cli.session_kind(),
        }),
    }
}
