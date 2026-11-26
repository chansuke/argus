//! What the host executes for a run: a process (`CommandSpec`), a leased process, or an
//! in-process effect. Plain data: `plan::plan` and the adapters build these, the host
//! runs them.

use std::path::PathBuf;
use std::time::Duration;

use crate::event::*;
use crate::ids::*;
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
    /// A directory inside the workdir to run in (a spec's directory, a Lean package).
    pub cwd: Option<RepoPath>,
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

/// Paths of external tools, resolved once at host start (Nix profile).
#[derive(Clone, Debug)]
pub struct ToolPaths {
    /// The `argus` CLI; agents get `argus mcp` as an MCP server.
    pub argus: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub java: PathBuf,
    pub tla2tools_jar: PathBuf,
    pub tlars: PathBuf,
    pub lake: PathBuf,
    /// `eas` (eas-cli); authenticated via `eas login` or `EXPO_TOKEN`.
    pub eas: PathBuf,
    /// util-linux `script`: `script -qfec <command> /dev/null` gives a process a PTY
    /// without FFI (`CommandSpec::terminal`).
    pub script: PathBuf,
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
