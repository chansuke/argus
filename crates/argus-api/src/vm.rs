//! View models: what the desktop and phone render. Flat, paginated, serializable.
//! They are deliberately not the core's projection types: projections are fold state
//! (indices, maps); view models are screens. The host maps one to the other.
//! Visual design is done elsewhere; these define only what each screen can show.
//!
//! Every closed set is a `vm_tags!` enum (a string-literal union in TS). Strings remain
//! only for ids, paths, free text, model names, and period expressions (a small grammar,
//! parsed at the boundary).

use argus_json::Json;

use crate::wire::{vm_enum, vm_struct, vm_tags};

vm_struct! { /// No input.
Empty {} }

vm_struct! { JobRef { job: String } }
vm_struct! { RunRef { job: String, run: String } }
vm_struct! { RunRefs { runs: Vec<RunRef> } }
vm_struct! { Ack { duplicate: bool } }

// ---- Closed sets ------------------------------------------------------------------

vm_tags! { /// Mapped exhaustively both ways to `argus_core::models::AgentCli` by the host.
Agent { Claude, Codex } }
vm_tags! { Effort { Low, Medium, High, Max } }
vm_tags! { Lens { General, Correctness, Security, Tests } }
vm_tags! { Severity { High, Medium, Low, Nit } }
vm_tags! { Engine { JavaTlc, Tlars } }
vm_tags! { Platform { Ios, Android } }
vm_tags! { Verdict { Approve, RequestChanges, Abandon } }
vm_tags! { GroupBy { Agent, Model, Job, Day, Tool } }
vm_tags! { Bucket { M15, H1, H4 } }
vm_tags! { BookmarkKind { Idea, Improvement, Pitfall, Exemplar } }
vm_tags! { /// Declaration order is priority order.
Tier { Blocking, Suspicious, Normal, Collapsed } }
vm_tags! { /// How the raw view renders a frame's bytes.
RawFormat { Event, Json, Terminal, Text } }
vm_tags! { ItemKind {
    Prompt, Reply, Thinking, ToolCall, PlanChange, Snapshot, RunLifecycle, Finding, CheckVerdict,
    Artifact, Comment, TaskChange, HumanInput, Decision, Discontinuity, Unrecognized,
} }

vm_struct! { /// `claude:opus`, `codex:gpt-5:high`.
ModelPick { agent: Agent, model: String, effort: Option<Effort> } }

// ---- Overview / glance (progress) -------------------------------------------------

vm_enum! { Gate { NotConfigured {}, Pending {}, Green {}, Red { open: u32 }, Stale {} } }

vm_enum! { /// What an agent is doing right now. `permission`: the prompt text when it
    /// waits for a permission; `null` when its turn ended.
    Activity {
        Starting { since_ms: i64 }, Thinking { since_ms: i64 }, Writing { since_ms: i64 },
        Tool { name: String, since_ms: i64, fact: String },
        AwaitingHuman { since_ms: i64, permission: Option<String> },
        Exited { at_ms: i64, ok: bool },
} }

vm_enum! { JobState {
    Working {}, AwaitingDecision {}, ChangesRequested { delivered: bool }, Approved {}, Merging {}, Merged {}, Abandoned {},
} }

vm_enum! { /// Why the human is needed, in urgency order (the overview sorts by it).
    Attention {
        AwaitingPermission { since_ms: i64, run: String }, Counterexample { since_ms: i64 },
        ChecksFailing { since_ms: i64 }, DeliveryFailed { since_ms: i64, decision: String },
        AwaitingDecision { since_ms: i64 }, AgentWaiting { since_ms: i64, run: String },
        Stalled { since_ms: i64, run: String }, RunFailed { since_ms: i64, run: String },
} }

vm_struct! { AgentNow { run: String, agent: Agent, model: Option<String>, activity: Activity, calls_since_plan_change: u32 } }

vm_struct! { /// One row of the overview: everything needed to decide where to look next.
    /// `share` is in 0..=1, or absent when the job has no plan to measure against.
    JobCard {
        job: String, title: String, group: Option<String>, state: JobState, share: Option<f64>, agents: Vec<AgentNow>,
        review: Gate, checks: Gate, open_leases: u32, attention: Vec<Attention>, updated_ms: i64,
} }

vm_struct! { QueueEntry { job: String, title: String, attention: Attention } }

vm_struct! { /// `queue`: every job's attention items, most urgent first. The phone's home screen.
Overview { queue: Vec<QueueEntry>, jobs: Vec<JobCard> } }

vm_enum! { TaskStatus { Pending {}, InProgress {}, Blocked { reason: String }, Done { result: Option<String> }, Dropped {} } }

vm_struct! { PlanRow { depth: u16, key: Option<String>, title: String, status: TaskStatus, share: f64, source: String, counted: bool } }

vm_struct! { JobDetail { card: JobCard, plan: Vec<PlanRow>, worktree: String, base: String, head: Option<HeadRef> } }

vm_struct! { /// Pushed on every change to a job; `cursor` lets clients drop stale or repeated pushes.
JobPulse { cursor: String, detail: JobDetail } }

// ---- Timeline ---------------------------------------------------------------------

vm_enum! { Lane { Agent { run: String }, Run { run: String }, Human {}, Host {} } }
vm_struct! { TimelineQuery { job: String, from_ms: i64, to_ms: i64, bucket: Option<Bucket> } }
vm_struct! { TimelineItem { fact: String, lane: Lane, kind: ItemKind, title: String, at_ms: i64, end_ms: Option<i64>, ok: Option<bool> } }
vm_struct! { TimelineBucket { start_ms: i64, width: Bucket, tool_calls: u32, tool_errors: u32, summary: Option<String> } }
vm_struct! { TimelinePage { items: Vec<TimelineItem>, buckets: Vec<TimelineBucket> } }

// ---- Raw log ----------------------------------------------------------------------

vm_struct! { RawQuery { job: String, source: Option<String>, around_fact: Option<String>, limit: u16 } }
vm_struct! { /// A stored frame: exact bytes and argus's reading of them, side by side.
/// `reading` is display-only (rendered as a JSON tree, never matched on).
/// `repeat`: a record already applied from another frame (resume, rewrite).
RawRecord { fact: String, source: Option<String>, format: RawFormat, stamp: String, text: String, is_utf8: bool, repeat: bool, reading: Json } }
vm_struct! { RawPage { records: Vec<RawRecord>, next: Option<String> } }

// ---- Review -----------------------------------------------------------------------

vm_struct! { Lines { path: String, start: u32, end: u32 } }
vm_struct! { /// The head snapshot. Verdicts and merges send `tree` back: "this exact code".
HeadRef { tree: String, seq: u32 } }
vm_struct! { Evidence { run: String, name: String, media_type: String, blob: String, len: String } }
vm_enum! { Applicability { Current {}, CarriedForward { from_tree: String } } }
vm_enum! { /// One click from a hunk to the agent's raw words (`facts` open the raw view).
Provenance { Direct { facts: Vec<String>, coverage_permille: u16 }, ViaShell { facts: Vec<String> }, Unexplained {} } }
vm_enum! { Signal {
    CheckFailed { run: String },
    Finding { severity: Severity, agreeing_models: u16 },
    Unaddressed { thread: String },
    Unexplained {},
    SensitivePath {},
} }
vm_struct! { FindingView {
    id: String, run: String, model: ModelPick, severity: Severity, title: String, body: String,
    at: Option<Lines>, applicability: Applicability, agreeing_models: u16, dismissed: bool, promoted: bool,
} }
vm_struct! { /// `draft`: not sent yet; the next RequestChanges sends it.
Comment { id: String, at_ms: i64, body: String, draft: bool } }
vm_struct! { ThreadView {
    id: String, at: Option<Lines>, outdated: bool, resolved: bool, unaddressed: bool,
    from_finding: Option<String>, comments: Vec<Comment>,
} }
vm_struct! { HunkView { path: String, old_start: u32, old_len: u32, new_start: u32, new_len: u32, text: String } }
vm_struct! { AgendaItem { hunk: HunkView, tier: Tier, signals: Vec<Signal>, provenance: Provenance, findings: Vec<FindingView>, threads: Vec<ThreadView> } }
vm_struct! { AiTally { model: ModelPick, high: u32, medium: u32, low: u32, nit: u32, unstructured: bool } }
vm_struct! { /// Readable in five seconds. `withheld`: untracked files the snapshot deny list
    /// kept out (`.env*`, keys).
    Headline {
        round: u32, files: u32, added: u32, removed: u32, checks: Gate, ai: Vec<AiTally>,
        pending_reviews: u32, unaddressed: u32, unexplained: u32, withheld: Vec<String>,
} }
vm_enum! { ReviewRunState { Running {}, Findings { count: u32 }, Unstructured { text: String }, Failed { reason: String } } }
vm_struct! { ReviewRunCard { run: String, model: ModelPick, lens: Lens, tree: String, state: ReviewRunState, raw_source: Option<String> } }
vm_enum! { CheckVerdict {
    Proved {}, Holes { sorries: u32 }, Refuted { summary: String, trace: Vec<String> }, Passed {}, ToolError { summary: String }, Cancelled {},
} }
vm_enum! { CheckTarget { Tla { spec: String, config: Option<String> }, Lean { root: String }, Device { project: String, platform: Platform } } }
vm_struct! { CheckCard {
    run: String, target: CheckTarget, engine: Option<Engine>, tree: String, applicability: Applicability,
    verdict: Option<CheckVerdict>, evidence: Vec<Evidence>, preview_url: Option<String>,
} }
vm_struct! { /// The review packet as a screen. `agenda` is sorted by tier; `unattached` and
    /// `threads` are what no head hunk holds; `window` lists the edits (facts) that produced
    /// the head snapshot, the raw-log entry point for this round.
    ReviewScreen {
        job: String, state: JobState, base: String, head: Option<HeadRef>, headline: Option<Headline>,
        agenda: Vec<AgendaItem>, unattached: Vec<FindingView>, threads: Vec<ThreadView>,
        reviews: Vec<ReviewRunCard>, checks: Vec<CheckCard>, window: Vec<String>, drafts: u32, can_merge: bool,
} }
vm_struct! { Preset { name: String, models: Vec<ModelPick> } }
vm_struct! { Presets { default: String, presets: Vec<Preset> } }

// ---- Analytics / skills -----------------------------------------------------------

vm_struct! { /// `period`: `2026-11`, `2026-11-17`, `last-7d`, ... (parsed by the host).
StatsQuery { period: String, group_by: GroupBy } }
vm_struct! { StatsRow { key: String, runs: u32, prompts: String, tool_calls: String, tool_errors: String, input_tokens: String, output_tokens: String, human_inputs: String } }
vm_struct! { StatsTable { period_from_ms: i64, period_to_ms: i64, exact: bool, rows: Vec<StatsRow> } }
vm_struct! { Candidate { id: String, name: String, description: String, why: String, body: String, evidence: Vec<String> } }
vm_struct! { Candidates { candidates: Vec<Candidate> } }

// ---- Commands (each carries a client-minted `request_id`: retries are idempotent,
// which matters most on a phone with a flaky connection) -----------------------------

vm_struct! { AgentChoice { agent: Agent, model: Option<String> } }
vm_struct! { StartJobs { request_id: String, repo: String, base: Option<String>, title: String, prompt: String, agents: Vec<AgentChoice> } }
vm_struct! { Started { jobs: Vec<String> } }
vm_enum! { Reviewers { Default {}, Preset { name: String }, Models { models: Vec<ModelPick> } } }
vm_struct! { /// One review run per resolved model, all of the head snapshot.
RequestReview { request_id: String, job: String, reviewers: Reviewers, lens: Lens, focus: Option<String> } }
vm_enum! { DeviceApp { BuildId { id: String }, Fingerprint { hash: String }, ArchiveUrl { url: String }, ExpoGo { sdk: Option<String> } } }
vm_enum! { DeviceDriver { Script { path: String }, Agent { model: ModelPick } } }
vm_enum! { CheckChoice {
    Tla { spec: String, config: Option<String>, engine: Engine, workers: Option<u16> },
    Lean { root: String, files: Vec<String> },
    Device { project: String, platform: Platform, app: DeviceApp, driver: DeviceDriver, max_minutes: u16 },
} }
vm_struct! { RequestCheck { request_id: String, job: String, check: CheckChoice } }
vm_enum! { AnchorInput {
    Code { tree: String, path: String, start: u32, end: u32 },
    Task { task: String },
    Fact { fact: String },
    Window { start_ms: i64, width: Bucket },
} }
vm_struct! { OpenThread { request_id: String, job: String, anchor: AnchorInput, body: String } }
vm_struct! { Reply { request_id: String, job: String, thread: String, body: String } }
vm_struct! { Resolve { job: String, thread: String, resolved: bool } }
vm_struct! { /// Turns an AI finding into a draft thread: the only way a finding reaches the agent.
PromoteFinding { request_id: String, job: String, finding: String } }
vm_struct! { DismissFinding { job: String, finding: String, dismissed: bool } }
vm_struct! { PreviewFeedback { job: String, note: String } }
vm_struct! { /// Byte-for-byte what a RequestChanges now would deliver.
FeedbackText { text: String, comments: u32 } }
vm_struct! { /// `tree`: the head the human looked at (`HeadRef.tree`); refused with `StaleHead`
/// if a newer snapshot arrived. `null` only to abandon a job with no snapshot.
Decide { request_id: String, job: String, tree: Option<String>, verdict: Verdict, note: String } }
vm_struct! { /// Run id is derived from the tree, so a retry needs no request id.
MergeJob { job: String, tree: String } }
vm_struct! { SetTask { job: String, task: Option<String>, parent: Option<String>, title: Option<String>, weight: Option<u32>, status: Option<TaskStatus> } }
vm_struct! { TaskSaved { task: String } }
vm_struct! { SendInput { request_id: String, job: String, run: String, text: String } }
vm_tags! { PermissionAnswer { Allow, AllowAlways, Deny } }
vm_struct! { /// One tap on an `AwaitingPermission` item (the phone's top queue entry). The host
/// turns it into the agent's own keystrokes (`plan::permission_input`).
AnswerPermission { request_id: String, job: String, run: String, answer: PermissionAnswer } }
vm_struct! { Bookmark { request_id: String, job: String, fact: String, kind: BookmarkKind, note: String } }
vm_struct! { RequestInsights { request_id: String, period: String, model: ModelPick } }
vm_struct! { AcceptCandidate { request_id: String, candidate: String, name: String, body: String, target: String } }
vm_struct! { CandidateRef { candidate: String } }
vm_struct! { /// A Claude Code hook payload, forwarded verbatim by `argus hook`.
HookPayload { body: String } }
