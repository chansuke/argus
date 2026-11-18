//! Authored events: facts that argus itself originates. Foreign bytes (agent transcripts,
//! tool output) are never authored events; they enter as raw frames (`source.rs`).
//!
//! Rule that decides what is an event: anything non-deterministic (a human decision,
//! a process outcome, wall-clock timing, an LLM answer, a remote id) is recorded.
//! Anything computable from recorded facts is a projection and is never stored.

use std::path::PathBuf;

use crate::diff::HunkAnchor;
use crate::ids::*;
use crate::models::{AgentCli, ModelChoice, ModelSet};
use crate::period::Period;
use crate::views::timeline::Window;

/// Who wrote an event. Maps 1:1 to an actor log (`ledger::actor_key`), so each author
/// has its own single-writer file.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Author {
    /// The person at the desktop app, a paired phone, or the `argus` CLI.
    Human,
    /// An agent speaking argus's own protocol (`argus task ...`, MCP tools), bound to its run.
    Agent(RunId),
    /// The host: job/run lifecycle, leases, artifacts, retention.
    Host,
    /// Automatic events derived from views (`policy::decide`): snapshots, auto review,
    /// feedback delivery. Ids are derived from the trigger, so deciding twice is a no-op.
    Policy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LedgerKind {
    /// One unit of work: a worktree, its runs, snapshots, reviews, checks, decisions, plan.
    Job,
    /// Store-wide: insight runs, skill candidates, skill writes.
    Meta,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    JobOpened(JobInfo),

    // Every external effect is a run: requested, started, (fed input), finished.
    RunRequested {
        run: RunId,
        purpose: RunPurpose,
    },
    RunStarted {
        run: RunId,
        attempt: Attempt,
        pid: Option<u32>,
    },
    /// Bytes the human typed or sent into a running agent's terminal.
    RunInput {
        input: InputId,
        run: RunId,
        bytes: Vec<u8>,
    },
    RunCancelRequested {
        run: RunId,
    },
    RunFinished {
        run: RunId,
        attempt: Attempt,
        outcome: RunOutcome,
    },

    // Billed external resources. Opened (intent) is appended BEFORE the remote resource
    // is created, so a crash at any point leaves a record the reconciler can act on.
    LeaseOpened {
        lease: LeaseId,
        run: RunId,
        attempt: Attempt,
        provider: LeaseProvider,
        name: String,
    },
    LeaseBound {
        lease: LeaseId,
        remote_id: String,
        preview_url: Option<String>,
    },
    LeaseClosed {
        lease: LeaseId,
        how: LeaseEnd,
    },

    /// A whole file a run produced (screenshot, recording, trace dump), stored in the blob
    /// store before this event is appended.
    ArtifactCaptured {
        run: RunId,
        attempt: Attempt,
        blob: BlobId,
        name: String,
        media_type: String,
        len: u64,
    },
    /// Retention deleted a source log (terminal bytes after N days). Appended before the
    /// file is removed, so the raw view can say why the bytes are gone.
    SourcePruned {
        source: SourceId,
    },

    // Review. Threads and comments are drafts until a `RequestChanges` decision sends them.
    // AI findings reach the agent only by being promoted into a draft thread.
    ThreadOpened {
        thread: ThreadId,
        comment: CommentId,
        anchor: Anchor,
        body: String,
    },
    Replied {
        comment: CommentId,
        thread: ThreadId,
        body: String,
    },
    ThreadResolved {
        thread: ThreadId,
        resolved: bool,
    },
    FindingPromoted {
        finding: FindingId,
        thread: ThreadId,
        comment: CommentId,
    },
    FindingDismissed {
        finding: FindingId,
        dismissed: bool,
    },
    /// The human's verdict on the head snapshot. `tree` is the head the human saw: the
    /// ledger accepts it only while it is still the head (`ViewSet::admit`), so a verdict
    /// always means "this exact code". `None` only to abandon a job with no snapshot yet.
    Decided {
        decision: DecisionId,
        tree: Option<TreeOid>,
        verdict: Decision,
        note: String,
    },

    /// Plan edit by the human or by an agent's self-report. Patches are assignments,
    /// never increments, so a retried `argus task done X` is harmless.
    TaskSet {
        task: TaskKey,
        patch: TaskPatch,
    },

    /// "This moment is worth learning from": seeds insight runs.
    Bookmarked {
        bookmark: BookmarkId,
        at: FactId,
        kind: BookmarkKind,
        note: String,
    },
    CandidateDismissed {
        candidate: CandidateId,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub struct JobInfo {
    pub job: JobId,
    pub title: String,
    pub repo: PathBuf,
    pub base: CommitOid,
    /// Branch an approved head is merged into (the branch `base` was taken from).
    pub target: String,
    pub worktree: PathBuf,
    /// Jobs fanned out from one prompt share a group, for side-by-side comparison.
    pub group: Option<Id128>,
    pub origin: JobOrigin,
}

#[derive(Clone, PartialEq, Debug)]
pub enum JobOrigin {
    Launched,
    /// Backfilled from a transcript that existed before argus watched it.
    Imported {
        transcript: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Allows `merge` while this tree stays the head.
    Approve,
    /// Sends every draft thread (human comments and promoted findings) to the agent as
    /// one feedback prompt. Delivery is a policy-requested resumed agent run.
    RequestChanges,
    /// Gives up on the job; policy cancels its live runs.
    Abandon,
}

/// What a run is for. Adding a variant fails the build until `plan::plan`,
/// `on_interrupt`, `ledger_kind`, and `Event::check` handle it.
#[derive(Clone, PartialEq, Debug)]
pub enum RunPurpose {
    /// An interactive coding agent in the job's worktree.
    Agent(AgentLaunch),
    /// Record the worktree as a git tree (in-process).
    Snapshot {
        after: Option<FactId>,
    },
    Review(ReviewSpec),
    Check(CheckRequest),
    /// Merge the approved head snapshot into the job's target branch (in-process).
    Merge(ApprovedHead),
    /// LLM summary of one timeline window (15m / 1h / 4h).
    Summarize {
        window: Window,
        llm: LlmCall,
    },
    /// Meta ledger: propose skill candidates from conversations in a period.
    Insight {
        period: Period,
        llm: LlmCall,
    },
    /// Meta ledger: materialize an accepted skill (in-process, idempotent write).
    WriteSkill(SkillDraft),
    /// Backfill a pre-existing transcript file into this job.
    Import {
        transcript: PathBuf,
        cli: AgentCli,
    },
}

/// What the host does with a run that was in flight when it crashed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnInterrupt {
    Retry,
    /// Close the attempt as `Abandoned`; a human decides whether to rerun.
    Abandon,
}

impl RunPurpose {
    pub fn on_interrupt(&self) -> OnInterrupt {
        match self {
            // An agent session has conversational state; restarting it blindly is wrong.
            // This also makes feedback delivery at-most-once: an interrupted delivery is
            // shown as failed (`Attention::DeliveryFailed`), never sent twice.
            RunPurpose::Agent(_) => OnInterrupt::Abandon,
            // Device sessions cost money per minute; never retry without a human.
            RunPurpose::Check(CheckRequest {
                spec: CheckSpec::Device { .. },
                ..
            }) => OnInterrupt::Abandon,
            // Merge is a compare-and-swap ref update; re-running an applied merge is a no-op.
            RunPurpose::Snapshot { .. }
            | RunPurpose::Review(_)
            | RunPurpose::Check(_)
            | RunPurpose::Merge(_)
            | RunPurpose::Summarize { .. }
            | RunPurpose::Insight { .. }
            | RunPurpose::WriteSkill(_)
            | RunPurpose::Import { .. } => OnInterrupt::Retry,
        }
    }

    pub fn ledger_kind(&self) -> LedgerKind {
        match self {
            RunPurpose::Insight { .. } | RunPurpose::WriteSkill(_) => LedgerKind::Meta,
            RunPurpose::Agent(_)
            | RunPurpose::Snapshot { .. }
            | RunPurpose::Review(_)
            | RunPurpose::Check(_)
            | RunPurpose::Merge(_)
            | RunPurpose::Summarize { .. }
            | RunPurpose::Import { .. } => LedgerKind::Job,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct AgentLaunch {
    pub cli: AgentCli,
    pub model: Option<ModelId>,
    pub prompt: String,
    /// Continue the session of this earlier agent run (`claude --resume <its session>`)
    /// instead of starting fresh. Feedback delivery always resumes.
    pub resume: Option<RunId>,
}

/// The exact prompt sent, stored in the request: the log shows what the model was asked,
/// not a template that may have changed since.
#[derive(Clone, PartialEq, Debug)]
pub struct LlmCall {
    pub model: ModelChoice,
    pub prompt: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ReviewSpec {
    pub tree: TreeOid,
    pub base: CommitOid,
    pub lens: ReviewLens,
    pub llm: LlmCall,
}

impl ReviewSpec {
    /// The one way to ask for AI review (human, agent, or policy): one run per model, all
    /// of `tree`. Run ids derive from `request` and the model, so a retried request or a
    /// policy decision made twice yields the same runs. The prompt is built here
    /// (`adapters::llm::review_prompt`) and stored verbatim; `focus` is free text the
    /// human adds to the lens.
    pub fn panel(
        request: Id128,
        tree: &TreeOid,
        base: &CommitOid,
        lens: ReviewLens,
        focus: Option<&str>,
        models: &ModelSet,
    ) -> Vec<(RunId, RunPurpose)> {
        // TODO: for m in models: run = RunId(Id128::derive("review", request ++ m));
        //       prompt = adapters::llm::review_prompt(base, lens, focus);
        //       (run, Review(ReviewSpec { tree, base, lens, llm: LlmCall { model: m, prompt } }))
        todo!("not implemented")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewLens {
    General,
    Correctness,
    Security,
    Tests,
}

/// A check of one tree. The spec enum makes engine/target mismatches unrepresentable.
#[derive(Clone, PartialEq, Debug)]
pub struct CheckRequest {
    pub tree: TreeOid,
    pub base: CommitOid,
    pub spec: CheckSpec,
}

#[derive(Clone, PartialEq, Debug)]
pub enum CheckSpec {
    /// Both engines print the same `-tool` message stream (tlars targets byte-identical
    /// output), so one decoder serves both.
    Tla {
        spec: RepoPath,
        config: Option<RepoPath>,
        engine: TlaEngine,
        workers: Option<u16>,
    },
    Lean {
        root: RepoPath,
        mode: LeanMode,
    },
    /// Exercise an Expo app on an EAS Simulators session and collect evidence.
    Device {
        project: RepoPath,
        platform: DevicePlatform,
        app: AppSource,
        driver: DeviceDriver,
        budget: DeviceBudget,
    },
}

/// Engine-independent identity of what is checked: running TLC and tlars on one spec are
/// two runs of the same target, so their verdicts can be compared.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CheckTarget {
    Tla {
        spec: RepoPath,
        config: Option<RepoPath>,
    },
    Lean {
        root: RepoPath,
    },
    Device {
        project: RepoPath,
        platform: DevicePlatform,
    },
}

impl CheckSpec {
    pub fn target(&self) -> CheckTarget {
        match self {
            CheckSpec::Tla { spec, config, .. } => CheckTarget::Tla {
                spec: spec.clone(),
                config: config.clone(),
            },
            CheckSpec::Lean { root, .. } => CheckTarget::Lean { root: root.clone() },
            CheckSpec::Device {
                project, platform, ..
            } => CheckTarget::Device {
                project: project.clone(),
                platform: *platform,
            },
        }
    }
}

impl CheckTarget {
    /// Whether a verdict on this target can depend on `path`. The one rule for both
    /// "run this check on a snapshot that touched it" (policy) and "an older verdict still
    /// holds at head because none of its files changed" (`Applicability::CarriedForward`).
    pub fn depends_on(&self, path: &RepoPath) -> bool {
        // TODO: Tla: path == spec || Some(path) == config; Lean: under root (any file of
        //       the package); Device: under project.
        todo!("not implemented")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TlaEngine {
    /// `java -cp tla2tools.jar tlc2.TLC -tool ...`
    JavaTlc,
    /// `tlars -tool ...` (~/code/myprojects/tlars)
    Tlars,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LeanMode {
    /// `lake build`: text diagnostics for the whole package.
    LakeBuild,
    /// `lake env lean --json <file>` per file: structured diagnostics.
    Files(Vec<RepoPath>),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum DevicePlatform {
    Ios,
    Android,
}

/// Maps to the mutually exclusive `eas simulator:start` app sources.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AppSource {
    BuildId(String),
    Fingerprint(String),
    ArchiveUrl(String),
    ExpoGo { sdk: Option<String> },
}

#[derive(Clone, PartialEq, Debug)]
pub enum DeviceDriver {
    /// A repo script run under `eas simulator:exec` (agent-device / Appium env loaded).
    Script(RepoPath),
    /// A headless LLM agent told to verify the change with agent-device and save evidence.
    Agent(LlmCall),
}

/// Provider-side caps (`--max-duration-minutes`, `--max-idle-time-minutes`). They bound
/// the cost of a leak even if argus dies and never comes back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceBudget {
    pub max_minutes: u16,
    pub max_idle_minutes: u16,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LeaseProvider {
    EasSimulator { platform: DevicePlatform },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeaseEnd {
    Stopped,
    /// The provider no longer knows it (expired by its own cap, or never created).
    Gone,
}

#[derive(Clone, PartialEq, Debug)]
pub enum RunOutcome {
    Exited {
        code: i32,
    },
    Killed {
        signal: i32,
        cancelled: bool,
    },
    SpawnFailed {
        error: String,
    },
    /// The host died while this attempt ran.
    Abandoned,
    Snapshot(Snapshot),
    Merged {
        commit: CommitOid,
    },
    SkillWritten {
        path: PathBuf,
    },
    /// An in-process effect failed (merge conflict, target branch moved, write error).
    Failed {
        reason: String,
    },
}

/// Proof that `tree` is the job's head snapshot and the human's latest decision on it is
/// `Approve`. Minted only by `ReviewBoard::approved_head` (`mint` is crate-private for that
/// view and the codec). `RunPurpose::Merge` takes nothing else, so "merge without approval"
/// does not compile. The ledger re-checks it on the sequencer at append (`ViewSet::admit`),
/// so an approval that a new snapshot made stale between read and submit is rejected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ApprovedHead {
    tree: TreeOid,
    base: CommitOid,
}

impl ApprovedHead {
    pub(crate) fn mint(tree: TreeOid, base: CommitOid) -> Self {
        ApprovedHead { tree, base }
    }
    pub fn tree(&self) -> &TreeOid {
        &self.tree
    }
    pub fn base(&self) -> &CommitOid {
        &self.base
    }
}

/// A worktree state, stored as a git tree kept alive by `refs/argus/<job>/<n>`.
///
/// `delta` (hunk headers vs `previous`) and `files` (vs `base`) are readings of immutable
/// trees, recorded so that projections never call git; they cannot disagree with git.
#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub tree: TreeOid,
    pub base: CommitOid,
    pub previous: Option<TreeOid>,
    /// Enough to re-anchor human threads (line ranges) from `previous` to `tree`.
    pub delta: Vec<FileDelta>,
    /// Every file that differs from `base`: blob ids decide `CarriedForward`, hunk anchors
    /// decide "already seen in a decided snapshot" (`Tier::Collapsed`).
    pub files: Vec<FileState>,
    /// Untracked files left out by the snapshot deny list (`.env*`, `*.pem`, `id_*`, ...).
    /// Shown on the review screen, so a withheld file is visible, never silently missing.
    pub withheld: Vec<RepoPath>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FileDelta {
    pub path: RepoPath,
    pub renamed_from: Option<RepoPath>,
    pub hunks: Vec<Hunk>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FileState {
    pub path: RepoPath,
    /// `None`: deleted in this tree.
    pub blob: Option<GitOid>,
    pub hunks: Vec<HunkAnchor>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

/// What a review thread is about.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Anchor {
    Code {
        tree: TreeOid,
        path: RepoPath,
        lines: LineRange,
    },
    Task(TaskKey),
    Fact(FactId),
    Window(Window),
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct TaskPatch {
    pub parent: Option<Option<TaskKey>>,
    pub title: Option<String>,
    pub weight: Option<Weight>,
    pub state: Option<TaskState>,
}

/// Share of a task among its siblings (dsh-timeline semantics: splitting a task into
/// subtasks redistributes the task's share, never the parent's total).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Weight(pub std::num::NonZeroU32);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaskState {
    Pending,
    InProgress,
    Blocked {
        reason: String,
    },
    Done {
        result: Option<String>,
    },
    /// Excluded from progress denominators.
    Dropped,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BookmarkKind {
    Idea,
    Improvement,
    Pitfall,
    Exemplar,
}

#[derive(Clone, PartialEq, Debug)]
pub struct SkillDraft {
    pub from: Option<CandidateId>,
    pub name: SkillName,
    pub description: String,
    pub body: String,
    /// Directory that holds skill directories, e.g. `<repo>/.claude/skills` or the
    /// dotfiles-managed one. The run writes `<target>/<name>/SKILL.md`.
    pub target: PathBuf,
}

/// Why a submission was refused at the ledger boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum Rejected {
    WrongAuthor,
    WrongLedger,
    /// A decision named a tree that is no longer (or not yet) the head.
    NotHead {
        head: Option<TreeOid>,
    },
    /// A merge whose approval is no longer current.
    NotApproved,
}

/// Identity used to drop a re-submitted event (a retried CLI call, a policy decision made
/// twice around a crash). Kept in an ordered index: O(log N) per submission.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum DedupKey {
    Job(JobId),
    Run(RunId),
    RunStart(RunId, Attempt),
    RunEnd(RunId, Attempt),
    Input(InputId),
    Lease(LeaseId, u8),
    Artifact(RunId, BlobId),
    Pruned(SourceId),
    Comment(CommentId),
    Decision(DecisionId),
    Bookmark(BookmarkId),
}

impl Event {
    /// Who may author this event in which ledger. Exhaustive: a new variant fails to
    /// compile until its authorship rule is written. View-dependent rules (head-only
    /// decisions, current approval) are `ViewSet::admit`, checked right after this.
    pub fn check(&self, author: &Author, ledger: LedgerKind) -> Result<(), Rejected> {
        use Author::*;
        let author_ok = match self {
            Event::JobOpened(_)
            | Event::RunStarted { .. }
            | Event::RunFinished { .. }
            | Event::LeaseOpened { .. }
            | Event::LeaseBound { .. }
            | Event::LeaseClosed { .. }
            | Event::ArtifactCaptured { .. }
            | Event::SourcePruned { .. } => matches!(author, Host),
            Event::RunRequested { purpose, .. } => match (author, purpose) {
                // Merging is the human's call alone.
                (Human, _) => true,
                (Policy, RunPurpose::Merge(_)) => false,
                (Policy, _) => true,
                // Agents may ask for review or checks of their own work; nothing else.
                (Agent(_), p) => matches!(p, RunPurpose::Review(_) | RunPurpose::Check(_)),
                (Host, _) => false,
            },
            // Policy cancels idle agents before a feedback delivery and live runs on Abandon.
            Event::RunCancelRequested { .. } => matches!(author, Human | Policy),
            Event::TaskSet { .. } => matches!(author, Human | Agent(_)),
            Event::RunInput { .. }
            | Event::ThreadOpened { .. }
            | Event::Replied { .. }
            | Event::ThreadResolved { .. }
            | Event::FindingPromoted { .. }
            | Event::FindingDismissed { .. }
            | Event::Decided { .. }
            | Event::Bookmarked { .. }
            | Event::CandidateDismissed { .. } => matches!(author, Human),
        };
        let ledger_ok = match self {
            Event::RunRequested { purpose, .. } => purpose.ledger_kind() == ledger,
            Event::CandidateDismissed { .. } => ledger == LedgerKind::Meta,
            _ => ledger == LedgerKind::Job,
        };
        match (author_ok, ledger_ok) {
            (false, _) => Err(Rejected::WrongAuthor),
            (_, false) => Err(Rejected::WrongLedger),
            _ => Ok(()),
        }
    }

    /// `None` for events that are idempotent by meaning (assignments: `TaskSet`,
    /// `ThreadResolved`, `FindingDismissed`, ...).
    pub(crate) fn dedup_key(&self) -> Option<DedupKey> {
        Some(match self {
            Event::JobOpened(info) => DedupKey::Job(info.job),
            Event::RunRequested { run, .. } => DedupKey::Run(*run),
            Event::RunStarted { run, attempt, .. } => DedupKey::RunStart(*run, *attempt),
            Event::RunFinished { run, attempt, .. } => DedupKey::RunEnd(*run, *attempt),
            Event::RunInput { input, .. } => DedupKey::Input(*input),
            Event::LeaseOpened { lease, .. } => DedupKey::Lease(*lease, 0),
            Event::LeaseBound { lease, .. } => DedupKey::Lease(*lease, 1),
            Event::LeaseClosed { lease, .. } => DedupKey::Lease(*lease, 2),
            Event::ArtifactCaptured { run, blob, .. } => DedupKey::Artifact(*run, *blob),
            Event::SourcePruned { source } => DedupKey::Pruned(*source),
            Event::ThreadOpened { comment, .. }
            | Event::Replied { comment, .. }
            | Event::FindingPromoted { comment, .. } => DedupKey::Comment(*comment),
            Event::Decided { decision, .. } => DedupKey::Decision(*decision),
            Event::Bookmarked { bookmark, .. } => DedupKey::Bookmark(*bookmark),
            Event::RunCancelRequested { .. }
            | Event::ThreadResolved { .. }
            | Event::FindingDismissed { .. }
            | Event::TaskSet { .. }
            | Event::CandidateDismissed { .. } => return None,
        })
    }
}

/// Stored payload codec (JSON via argus-json; human-inspectable with `argus raw`).
/// The author is not in the payload: it is the identity of the log the event lives in.
/// References to frames use `FactId`'s one text form.
pub(crate) use crate::codec::{decode, encode};
