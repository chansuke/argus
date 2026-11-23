//! The only `argus_api::Handler`: maps view models to core events (validation at the
//! boundary: strings -> ids, paths, trees; wire enums -> core enums) and core views to
//! view models. Business rules stay in the core; this is translation plus routing to
//! ledger threads.

use argus_api::vm::*;
use argus_api::{ApiError, Caller, Handler};
use std::path::PathBuf;

use argus_core::diff::HeadDiff;
use argus_core::event::{self, CheckRequest, CheckSpec, DevicePlatform, LeanMode, TlaEngine};
use argus_core::event::{
    AgentLaunch, Anchor, Author, Decision, Event, JobInfo, JobOrigin, Rejected, ReviewLens,
    ReviewSpec, RunPurpose, TaskPatch, TaskState, Weight,
};
use argus_core::ids::{
    BookmarkId, CommentId, CommitOid, DecisionId, FactId, FindingId, GitOid, Id128, InputId, JobId,
    LineRange, ModelId, RepoPath, RunId, SourceId, TaskKey, ThreadId, TreeOid, UnixMillis,
};
use argus_core::ledger::Reading;
use argus_core::ledger::{JobLedger, LedgerError, Submitted};
use argus_core::models::AgentCli;
use argus_core::models::{self, ModelChoice, ModelSet, PresetName, ReviewerChoice, UnknownPreset};
use argus_core::source::{PlanStatus, SourceKind, SourceMeta, SourceOrigin};
use argus_core::views::packet::{self, PacketConfig};
use argus_core::views::timeline::{BucketWidth, Window};
use argus_core::views::{GateState, checks, glance, progress, review, timeline};
use argus_json::Json;
use argus_log::{EventId, Seq};

use crate::ledger_thread::LedgerHandle;
use crate::pulse::Dep;
use crate::{HostInner, git};

pub(crate) struct ApiHandler<'h> {
    host: &'h HostInner,
    /// What this evaluation read (`LedgerHandle::read` appends). `Host::subscribe` keeps
    /// it as the subscription's dependencies; one-shot calls drop it.
    pub(crate) deps: Vec<Dep>,
}

impl<'h> ApiHandler<'h> {
    pub(crate) fn new(host: &'h HostInner) -> Self {
        Self {
            host,
            deps: Vec::new(),
        }
    }
}

/// `Caller` -> `Author`. Desktop, CLI, and paired phones are `Human`; agents are bound to
/// their own run and cannot speak as anyone else.
fn author(caller: &Caller) -> Result<argus_core::event::Author, ApiError> {
    match caller {
        Caller::Local | Caller::Phone { .. } => Ok(Author::Human),
        Caller::Agent { run } => Ok(Author::Agent(RunId(id(run, "caller.run")?))),
    }
}

fn id(text: &str, path: &str) -> Result<Id128, ApiError> {
    Id128::parse(text).ok_or_else(|| ApiError::BadInput {
        path: path.into(),
        what: format!("not an id: {text:?}"),
    })
}

/// A request id becomes the id of what it creates, so a retried request creates nothing new.
fn derived(namespace: &str, request_id: &str) -> Id128 {
    Id128::derive(namespace, request_id.as_bytes())
}

fn tree(text: &str, path: &str) -> Result<GitOid, ApiError> {
    GitOid::parse(text).ok_or_else(|| ApiError::BadInput {
        path: path.into(),
        what: format!("not a git object id: {text:?}"),
    })
}

fn finding(text: &str) -> Result<FindingId, ApiError> {
    FindingId::parse(text).ok_or_else(|| ApiError::BadInput {
        path: "finding".into(),
        what: format!("not a finding id: {text:?}"),
    })
}

fn anchor(input: AnchorInput) -> Result<Anchor, ApiError> {
    let bad = |path: &str, what: String| ApiError::BadInput {
        path: format!("anchor.{path}"),
        what,
    };
    Ok(match input {
        AnchorInput::Code {
            tree: t,
            path,
            start,
            end,
        } => Anchor::Code {
            tree: tree(&t, "anchor.tree")?,
            path: RepoPath::parse(&path)
                .ok_or_else(|| bad("path", format!("not a path in the repository: {path:?}")))?,
            lines: LineRange::new(start, end)
                .ok_or_else(|| bad("start", format!("not a line range: {start}-{end}")))?,
        },
        AnchorInput::Task { task } => Anchor::Task(TaskKey(id(&task, "anchor.task")?)),
        AnchorInput::Fact { fact } => Anchor::Fact(
            FactId::parse(&fact).ok_or_else(|| bad("fact", format!("not a fact id: {fact:?}")))?,
        ),
        AnchorInput::Window { start_ms, width } => Anchor::Window(Window {
            width: bucket_width(width),
            start: UnixMillis(start_ms),
        }),
    })
}

const RAW_PAGE_MAX: u16 = 500;
const REVIEW_TRIES: usize = 3;

/// `None` when `diff` is not of the current head (it moved): read again.
fn review_screen(
    l: &JobLedger,
    job: String,
    diff: Option<&HeadDiff>,
    cfg: &PacketConfig,
    now: UnixMillis,
) -> Option<ReviewScreen> {
    let views = l.views();
    let review = views.review();
    let head_tree = review.head().map(|h| h.snapshot.tree.clone());
    let mut screen = ReviewScreen {
        job,
        state: job_state(views.glance(now).state),
        base: views
            .runs()
            .job()
            .map(|j| j.base.as_str().to_owned())
            .unwrap_or_default(),
        head: None,
        headline: None,
        agenda: Vec::new(),
        unattached: Vec::new(),
        threads: Vec::new(),
        reviews: review
            .reviews()
            .map(|(run, r)| review_card(run, r))
            .collect(),
        checks: views
            .checks()
            .latest_runs()
            .map(|(run, c)| check_card(run, c, head_tree.as_ref()))
            .collect(),
        window: Vec::new(),
        drafts: review.feedback(None).len() as u32,
        can_merge: false,
    };
    let Some(diff) = diff else {
        // No head yet; one that arrived since the first read needs its diff.
        return head_tree.is_none().then_some(screen);
    };
    let packet = views.review_packet(diff, cfg).ok()?;
    screen.head = Some(HeadRef {
        tree: packet.head.snapshot.tree.as_str().to_owned(),
        seq: review.snapshots().len() as u32,
    });
    screen.headline = Some(headline(&packet));
    screen.agenda = packet.agenda.iter().map(agenda_item).collect();
    screen.unattached = packet.unattached.iter().map(finding_view).collect();
    screen.threads = packet.loose_threads.iter().map(thread_view).collect();
    screen.window = packet.window.iter().map(|e| e.fact.to_string()).collect();
    screen.can_merge = packet.approved.is_some();
    Some(screen)
}

fn headline(p: &packet::ReviewPacket<'_>) -> Headline {
    let h = &p.headline;
    Headline {
        round: p.round,
        files: h.files,
        added: h.added,
        removed: h.removed,
        checks: gate(h.checks),
        ai: h
            .ai
            .iter()
            .map(|t| AiTally {
                model: model_pick(&t.model),
                high: t.by_severity[0],
                medium: t.by_severity[1],
                low: t.by_severity[2],
                nit: t.by_severity[3],
                unstructured: t.unstructured,
            })
            .collect(),
        pending_reviews: h.pending_reviews,
        unaddressed: h.unaddressed,
        unexplained: h.unexplained,
        withheld: p
            .head
            .snapshot
            .withheld
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect(),
    }
}

fn model_pick(m: &ModelChoice) -> ModelPick {
    ModelPick {
        agent: agent_wire(m.cli),
        model: m.model.0.clone(),
        effort: m.effort.map(|e| match e {
            models::Effort::Low => Effort::Low,
            models::Effort::Medium => Effort::Medium,
            models::Effort::High => Effort::High,
            models::Effort::Max => Effort::Max,
        }),
    }
}

fn severity(s: review::Severity) -> Severity {
    match s {
        review::Severity::High => Severity::High,
        review::Severity::Medium => Severity::Medium,
        review::Severity::Low => Severity::Low,
        review::Severity::Nit => Severity::Nit,
    }
}

fn lines(path: &RepoPath, at: LineRange) -> Lines {
    Lines {
        path: path.as_str().to_owned(),
        start: at.start(),
        end: at.end(),
    }
}

fn applicability(a: &packet::Applicability) -> Applicability {
    match a {
        packet::Applicability::Current => Applicability::Current {},
        packet::Applicability::CarriedForward { from } => Applicability::CarriedForward {
            from_tree: from.as_str().to_owned(),
        },
    }
}

fn facts(f: &[FactId]) -> Vec<String> {
    f.iter().map(|f| f.to_string()).collect()
}

fn agenda_item(a: &packet::AgendaItem<'_>) -> AgendaItem {
    let h = a.hunk;
    AgendaItem {
        hunk: HunkView {
            path: h.anchor.path.as_str().to_owned(),
            old_start: h.header.old_start,
            old_len: h.header.old_len,
            new_start: h.header.new_start,
            new_len: h.header.new_len,
            text: h.text.clone(),
        },
        tier: match a.tier {
            packet::Tier::Blocking => Tier::Blocking,
            packet::Tier::Suspicious => Tier::Suspicious,
            packet::Tier::Normal => Tier::Normal,
            packet::Tier::Collapsed => Tier::Collapsed,
        },
        signals: a
            .signals
            .iter()
            .map(|s| match s {
                packet::Signal::CheckFailed { run } => Signal::CheckFailed {
                    run: run.0.to_string(),
                },
                packet::Signal::Finding {
                    severity: s,
                    agreeing_models,
                } => Signal::Finding {
                    severity: severity(*s),
                    agreeing_models: *agreeing_models,
                },
                packet::Signal::Unaddressed(t) => Signal::Unaddressed {
                    thread: t.0.to_string(),
                },
                packet::Signal::Unexplained => Signal::Unexplained {},
                packet::Signal::SensitivePath => Signal::SensitivePath {},
            })
            .collect(),
        provenance: match &a.provenance {
            packet::Provenance::Direct {
                facts: f,
                coverage_permille,
            } => Provenance::Direct {
                facts: facts(f),
                coverage_permille: *coverage_permille,
            },
            packet::Provenance::ViaShell { facts: f } => Provenance::ViaShell { facts: facts(f) },
            packet::Provenance::Unexplained => Provenance::Unexplained {},
        },
        findings: a.findings.iter().map(finding_view).collect(),
        threads: a.threads.iter().map(thread_view).collect(),
    }
}

fn finding_view(f: &packet::FindingRef<'_>) -> FindingView {
    let at = match &f.finding.anchor {
        Some(Anchor::Code {
            path, lines: at, ..
        }) => Some(lines(path, *at)),
        _ => None,
    };
    FindingView {
        id: f.finding.id.to_string(),
        run: f.run.0.to_string(),
        model: model_pick(f.model),
        severity: severity(f.finding.severity),
        title: f.finding.title.clone(),
        body: f.finding.body.clone(),
        at,
        applicability: applicability(&f.applicability),
        agreeing_models: f.agreeing_models,
        dismissed: f.dismissed,
        promoted: f.promoted,
    }
}

fn thread_view(t: &packet::ThreadRef<'_>) -> ThreadView {
    let code_path = match &t.thread.anchor {
        Anchor::Code { path, .. } => Some(path),
        _ => None,
    };
    ThreadView {
        id: t.id.0.to_string(),
        at: code_path.zip(t.at).map(|(path, at)| lines(path, at)),
        // A code thread whose lines no longer exist at head.
        outdated: code_path.is_some() && t.at.is_none(),
        resolved: t.thread.resolved,
        unaddressed: t.unaddressed,
        from_finding: match &t.thread.origin {
            review::ThreadOrigin::Human => None,
            review::ThreadOrigin::Promoted(f) => Some(f.to_string()),
        },
        comments: t
            .thread
            .comments
            .iter()
            .map(|c| Comment {
                id: c.id.0.to_string(),
                at_ms: c.at.0,
                body: c.body.clone(),
                draft: c.sent_by.is_none(),
            })
            .collect(),
    }
}

fn review_card(run: &RunId, r: &review::ReviewRun) -> ReviewRunCard {
    ReviewRunCard {
        run: run.0.to_string(),
        model: model_pick(&r.spec.llm.model),
        lens: match r.spec.lens {
            ReviewLens::General => Lens::General,
            ReviewLens::Correctness => Lens::Correctness,
            ReviewLens::Security => Lens::Security,
            ReviewLens::Tests => Lens::Tests,
        },
        tree: r.spec.tree.as_str().to_owned(),
        state: match &r.output {
            review::ReviewOutput::Running => ReviewRunState::Running {},
            review::ReviewOutput::Findings(f) => ReviewRunState::Findings {
                count: f.len() as u32,
            },
            review::ReviewOutput::Unstructured { text, .. } => {
                ReviewRunState::Unstructured { text: text.clone() }
            }
            review::ReviewOutput::Failed { reason } => ReviewRunState::Failed {
                reason: reason.clone(),
            },
        },
        raw_source: r.sources.first().map(|s| s.0.to_string()),
    }
}

fn platform(p: DevicePlatform) -> Platform {
    match p {
        DevicePlatform::Ios => Platform::Ios,
        DevicePlatform::Android => Platform::Android,
    }
}

fn check_card(run: &RunId, c: &checks::CheckRun, head: Option<&TreeOid>) -> CheckCard {
    let tree = &c.request.tree;
    CheckCard {
        run: run.0.to_string(),
        target: match c.request.spec.target() {
            argus_core::event::CheckTarget::Tla { spec, config } => CheckTarget::Tla {
                spec: spec.as_str().to_owned(),
                config: config.map(|c| c.as_str().to_owned()),
            },
            argus_core::event::CheckTarget::Lean { root } => CheckTarget::Lean {
                root: root.as_str().to_owned(),
            },
            argus_core::event::CheckTarget::Device {
                project,
                platform: p,
            } => CheckTarget::Device {
                project: project.as_str().to_owned(),
                platform: platform(p),
            },
        },
        engine: match &c.request.spec {
            CheckSpec::Tla { engine, .. } => Some(match engine {
                TlaEngine::JavaTlc => Engine::JavaTlc,
                TlaEngine::Tlars => Engine::Tlars,
            }),
            _ => None,
        },
        tree: tree.as_str().to_owned(),
        applicability: if head == Some(tree) {
            Applicability::Current {}
        } else {
            Applicability::CarriedForward {
                from_tree: tree.as_str().to_owned(),
            }
        },
        verdict: c.verdict.as_ref().map(|(_, v)| match v {
            checks::CheckVerdict::Proved => CheckVerdict::Proved {},
            checks::CheckVerdict::Holes { sorries } => CheckVerdict::Holes { sorries: *sorries },
            checks::CheckVerdict::Refuted { summary, trace } => CheckVerdict::Refuted {
                summary: summary.clone(),
                trace: facts(trace),
            },
            checks::CheckVerdict::Passed => CheckVerdict::Passed {},
            checks::CheckVerdict::ToolError { summary } => CheckVerdict::ToolError {
                summary: summary.clone(),
            },
            checks::CheckVerdict::Cancelled => CheckVerdict::Cancelled {},
        }),
        evidence: c
            .evidence
            .iter()
            .map(|a| Evidence {
                run: run.0.to_string(),
                name: a.name.clone(),
                media_type: a.media_type.clone(),
                blob: a.blob.hex(),
                len: a.len.to_string(),
            })
            .collect(),
        // Device previews are served once device checks run.
        preview_url: None,
    }
}

/// The source a frame belongs to, when its actor is a source (`src-<id>`).
fn source_of(event: &EventId) -> Option<SourceId> {
    let id = event.actor.as_str().strip_prefix("src-")?;
    Id128::parse(id).map(SourceId)
}

fn fact_of(event: EventId) -> FactId {
    FactId { event, part: 0 }
}

fn not_found(e: LedgerError) -> ApiError {
    match e {
        LedgerError::UnknownSource(s) => ApiError::NotFound {
            what: format!("source {}", s.0),
        },
        LedgerError::Log(argus_log::LogError::NotFound { .. }) => ApiError::NotFound {
            what: "no such frame".into(),
        },
        other => ledger_error(other),
    }
}

fn raw_format(kind: SourceKind) -> RawFormat {
    match kind {
        SourceKind::Terminal => RawFormat::Terminal,
        SourceKind::TlcTool | SourceKind::LeanText | SourceKind::PlainText => RawFormat::Text,
        SourceKind::ClaudeTranscript
        | SourceKind::ClaudeStreamJson
        | SourceKind::ClaudeHook
        | SourceKind::CodexRollout
        | SourceKind::CodexExec
        | SourceKind::LeanJson
        | SourceKind::EasEvents => RawFormat::Json,
    }
}

/// Exact bytes, and argus's reading beside them: an authored event is its stored JSON;
/// an observed frame lists what argus decoded from it (display only).
fn raw_record(r: argus_core::ledger::RawRecord) -> RawRecord {
    let (text, is_utf8) = match String::from_utf8(r.bytes) {
        Ok(t) => (t, true),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), false),
    };
    let (source, format, repeat, reading) = match &r.reading {
        Reading::Authored { .. } => {
            let reading = argus_json::parse(text.as_bytes()).unwrap_or_else(|_| Json::str(&text));
            (None, RawFormat::Event, false, reading)
        }
        Reading::Observed {
            source,
            decoded,
            repeat,
        } => {
            let observations = decoded
                .iter()
                .map(|o| Json::str(format!("{o:?}")))
                .collect();
            let reading = Json::Obj(vec![
                ("kind".into(), Json::str(format!("{:?}", source.kind))),
                ("observations".into(), Json::Arr(observations)),
            ]);
            (
                Some(source.id.0.to_string()),
                raw_format(source.kind),
                *repeat,
                reading,
            )
        }
    };
    RawRecord {
        fact: fact_of(r.id).to_string(),
        source,
        format,
        stamp: format!("{}.{}", r.stamp.wall_ns, r.stamp.logical),
        text,
        is_utf8,
        repeat,
        reading,
    }
}

fn ledger_error(e: LedgerError) -> ApiError {
    match e {
        LedgerError::Rejected(Rejected::NotHead { head }) => ApiError::StaleHead {
            head: head.map(|h| h.as_str().to_owned()),
        },
        LedgerError::Rejected(r) => ApiError::Rejected {
            what: format!("{r:?}"),
        },
        other => ApiError::Internal {
            what: format!("{other:?}"),
        },
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal {
        what: e.to_string(),
    }
}

fn now() -> UnixMillis {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    UnixMillis(since.map_or(0, |d| d.as_millis() as i64))
}

impl ApiHandler<'_> {
    fn job_handle(&self, job: &str) -> Result<(JobId, LedgerHandle), ApiError> {
        let job = JobId(id(job, "job")?);
        let handle = self.host.job(job).ok_or_else(|| ApiError::NotFound {
            what: format!("job {}", job.0),
        })?;
        Ok((job, handle))
    }

    fn submit(&self, handle: &LedgerHandle, author: Author, event: Event) -> Result<Ack, ApiError> {
        match handle.submit(author, event).map_err(ledger_error)? {
            Submitted::Appended(_) => Ok(Ack { duplicate: false }),
            Submitted::Duplicate(_) => Ok(Ack { duplicate: true }),
        }
    }
}

/// The head snapshot's tree and base, for a run that looks at the head.
fn head(handle: &LedgerHandle, job: JobId, to: &str) -> Result<(TreeOid, CommitOid), ApiError> {
    let head = handle.read(&mut Vec::new(), |l| {
        l.views()
            .review()
            .head()
            .map(|h| (h.snapshot.tree.clone(), h.snapshot.base.clone()))
    });
    head.ok_or_else(|| ApiError::NotFound {
        what: format!("a snapshot of job {} to {to}", job.0),
    })
}

// Core -> wire for what the overview shows.
fn job_state(s: glance::JobState) -> JobState {
    match s {
        glance::JobState::Working => JobState::Working {},
        glance::JobState::AwaitingDecision => JobState::AwaitingDecision {},
        glance::JobState::ChangesRequested { delivered } => {
            JobState::ChangesRequested { delivered }
        }
        glance::JobState::Approved => JobState::Approved {},
        glance::JobState::Merging => JobState::Merging {},
        glance::JobState::Merged => JobState::Merged {},
        glance::JobState::Abandoned => JobState::Abandoned {},
    }
}
fn gate(g: GateState) -> Gate {
    match g {
        GateState::NotConfigured => Gate::NotConfigured {},
        GateState::Pending => Gate::Pending {},
        GateState::Green => Gate::Green {},
        GateState::Red { open } => Gate::Red { open },
        GateState::Stale => Gate::Stale {},
    }
}
fn activity(a: &progress::Activity) -> Activity {
    use progress::{Activity as A, WaitReason};
    match a {
        A::Starting { since } => Activity::Starting { since_ms: since.0 },
        A::Thinking { since } => Activity::Thinking { since_ms: since.0 },
        A::Writing { since } => Activity::Writing { since_ms: since.0 },
        A::Tool { name, since, call } => Activity::Tool {
            name: name.0.clone(),
            since_ms: since.0,
            fact: call.to_string(),
        },
        A::AwaitingHuman { since, why } => Activity::AwaitingHuman {
            since_ms: since.0,
            permission: match why {
                WaitReason::Permission { message } => Some(message.clone()),
                WaitReason::TurnEnded => None,
            },
        },
        A::Exited { at, ok } => Activity::Exited {
            at_ms: at.0,
            ok: *ok,
        },
    }
}
fn attention(a: &glance::Attention) -> Attention {
    use glance::Attention as A;
    match a {
        A::AwaitingPermission { since, run } => Attention::AwaitingPermission {
            since_ms: since.0,
            run: run.0.to_string(),
        },
        A::Counterexample { since } => Attention::Counterexample { since_ms: since.0 },
        A::ChecksFailing { since } => Attention::ChecksFailing { since_ms: since.0 },
        A::DeliveryFailed { since, decision } => Attention::DeliveryFailed {
            since_ms: since.0,
            decision: decision.0.to_string(),
        },
        A::AwaitingDecision { since } => Attention::AwaitingDecision { since_ms: since.0 },
        A::AgentWaiting { since, run } => Attention::AgentWaiting {
            since_ms: since.0,
            run: run.0.to_string(),
        },
        A::Stalled { since, run } => Attention::Stalled {
            since_ms: since.0,
            run: run.0.to_string(),
        },
        A::RunFailed { since, run } => Attention::RunFailed {
            since_ms: since.0,
            run: run.0.to_string(),
        },
    }
}

fn task_status(s: &TaskState) -> TaskStatus {
    match s {
        TaskState::Pending => TaskStatus::Pending {},
        TaskState::InProgress => TaskStatus::InProgress {},
        TaskState::Blocked { reason } => TaskStatus::Blocked {
            reason: reason.clone(),
        },
        TaskState::Done { result } => TaskStatus::Done {
            result: result.clone(),
        },
        TaskState::Dropped => TaskStatus::Dropped {},
    }
}

fn task_state(s: TaskStatus) -> TaskState {
    match s {
        TaskStatus::Pending {} => TaskState::Pending,
        TaskStatus::InProgress {} => TaskState::InProgress,
        TaskStatus::Blocked { reason } => TaskState::Blocked { reason },
        TaskStatus::Done { result } => TaskState::Done { result },
        TaskStatus::Dropped {} => TaskState::Dropped,
    }
}

fn plan_status(s: PlanStatus) -> TaskStatus {
    match s {
        PlanStatus::Pending => TaskStatus::Pending {},
        PlanStatus::InProgress => TaskStatus::InProgress {},
        PlanStatus::Completed => TaskStatus::Done { result: None },
        PlanStatus::Deleted => TaskStatus::Dropped {},
    }
}

/// Depth-first rows: a task, then its children one level deeper; an agent's own plan
/// is one row per item, its source the run.
fn plan_rows(node: &progress::PlanNode<'_>, depth: u16, out: &mut Vec<PlanRow>) {
    match node {
        progress::PlanNode::Task {
            key,
            node,
            share,
            children,
        } => {
            out.push(PlanRow {
                depth,
                key: Some(key.0.to_string()),
                title: node.title.clone(),
                status: task_status(&node.state),
                share: share.get(),
                source: match node.by {
                    progress::PlanAuthor::Human => "human".into(),
                    progress::PlanAuthor::Agent(run) => run.0.to_string(),
                },
                counted: node.state != TaskState::Dropped,
            });
            for child in children {
                plan_rows(child, depth + 1, out);
            }
        }
        progress::PlanNode::Observed {
            run, plan, counted, ..
        } => {
            for (key, (title, status)) in &plan.items {
                out.push(PlanRow {
                    depth,
                    key: Some(key.clone()),
                    title: title.clone(),
                    status: plan_status(*status),
                    share: if *status == PlanStatus::Completed {
                        1.0
                    } else {
                        0.0
                    },
                    source: run.0.to_string(),
                    counted: *counted,
                });
            }
        }
    }
}

fn bucket_width(b: Bucket) -> BucketWidth {
    match b {
        Bucket::M15 => BucketWidth::M15,
        Bucket::H1 => BucketWidth::H1,
        Bucket::H4 => BucketWidth::H4,
    }
}

fn bucket_wire(w: BucketWidth) -> Bucket {
    match w {
        BucketWidth::M15 => Bucket::M15,
        BucketWidth::H1 => Bucket::H1,
        BucketWidth::H4 => Bucket::H4,
    }
}

fn timeline_item(at: UnixMillis, fact: &FactId, item: &timeline::Item) -> TimelineItem {
    use timeline::ItemKind as K;
    let lane = match item.lane {
        timeline::Lane::Agent(run) => Lane::Agent {
            run: run.0.to_string(),
        },
        timeline::Lane::Run(run) => Lane::Run {
            run: run.0.to_string(),
        },
        timeline::Lane::Human => Lane::Human {},
        timeline::Lane::Host => Lane::Host {},
    };
    let mut ok = None;
    let kind = match &item.kind {
        K::Prompt => ItemKind::Prompt,
        K::Reply => ItemKind::Reply,
        K::Thinking => ItemKind::Thinking,
        K::ToolCall { ok: done, .. } => {
            ok = *done;
            ItemKind::ToolCall
        }
        K::PlanChange => ItemKind::PlanChange,
        K::Snapshot => ItemKind::Snapshot,
        K::RunLifecycle => ItemKind::RunLifecycle,
        K::Finding => ItemKind::Finding,
        K::CheckVerdict => ItemKind::CheckVerdict,
        K::Artifact => ItemKind::Artifact,
        K::Comment => ItemKind::Comment,
        K::TaskChange => ItemKind::TaskChange,
        K::HumanInput => ItemKind::HumanInput,
        K::Decision => ItemKind::Decision,
        K::Discontinuity => ItemKind::Discontinuity,
        K::Unrecognized => ItemKind::Unrecognized,
    };
    TimelineItem {
        fact: fact.to_string(),
        lane,
        kind,
        title: item.title.clone(),
        at_ms: at.0,
        end_ms: item.ended_at.map(|t| t.0),
        ok,
    }
}

/// What one job contributes to the overview, read in one go on its loop.
struct JobSummary {
    glance: glance::Glance,
    card: JobCard,
}

fn summarize(ledger: &JobLedger, now: UnixMillis) -> Option<JobSummary> {
    let views = ledger.views();
    let info = views.runs().job()?.clone();
    let g = views.glance(now);
    let staleness: std::collections::BTreeMap<RunId, u32> =
        views.progress().staleness().map(|(r, n)| (*r, n)).collect();
    let agents = g
        .now
        .iter()
        .filter_map(|(run, a)| {
            let RunPurpose::Agent(launch) = &views.runs().get(run)?.purpose else {
                return None;
            };
            Some(AgentNow {
                run: run.0.to_string(),
                agent: agent_wire(launch.cli),
                model: launch.model.as_ref().map(|m| m.0.clone()),
                activity: activity(a),
                calls_since_plan_change: staleness.get(run).copied().unwrap_or(0),
            })
        })
        .collect();
    let updated = views
        .runs()
        .iter()
        .map(|(_, r)| r.requested_at.0)
        .max()
        .unwrap_or(0);
    let card = JobCard {
        job: info.job.0.to_string(),
        title: info.title.clone(),
        group: info.group.map(|g| g.to_string()),
        state: job_state(g.state),
        share: g.share.map(|s| s.get()),
        agents,
        review: gate(g.review),
        checks: gate(g.checks),
        open_leases: g.open_leases,
        attention: g.attention.iter().map(attention).collect(),
        updated_ms: updated,
    };
    Some(JobSummary { glance: g, card })
}

// Wire <-> core for closed sets. Exhaustive in both directions, so a variant added on
// either side fails to compile until mapped (the two lists cannot drift).
fn agent_cli(a: Agent) -> AgentCli {
    match a {
        Agent::Claude => AgentCli::Claude,
        Agent::Codex => AgentCli::Codex,
    }
}
/// A model named on the wire. The name becomes a command-line argument, so it may not
/// be empty, hold whitespace, or start like an option.
fn model_choice(pick: ModelPick) -> Result<ModelChoice, ApiError> {
    let name = pick.model;
    if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_whitespace) {
        return Err(ApiError::BadInput {
            path: "model".into(),
            what: format!("not a model name: {name:?}"),
        });
    }
    Ok(ModelChoice {
        cli: agent_cli(pick.agent),
        model: ModelId(name),
        effort: pick.effort.map(|e| match e {
            Effort::Low => models::Effort::Low,
            Effort::Medium => models::Effort::Medium,
            Effort::High => models::Effort::High,
            Effort::Max => models::Effort::Max,
        }),
    })
}
fn agent_wire(a: AgentCli) -> Agent {
    match a {
        AgentCli::Claude => Agent::Claude,
        AgentCli::Codex => Agent::Codex,
    }
}
fn decision(v: Verdict) -> Decision {
    match v {
        Verdict::Approve => Decision::Approve,
        Verdict::RequestChanges => Decision::RequestChanges,
        Verdict::Abandon => Decision::Abandon,
    }
}
fn lens(l: Lens) -> ReviewLens {
    match l {
        Lens::General => ReviewLens::General,
        Lens::Correctness => ReviewLens::Correctness,
        Lens::Security => ReviewLens::Security,
        Lens::Tests => ReviewLens::Tests,
    }
}

impl Handler for ApiHandler<'_> {
    fn overview(&mut self, caller: &Caller, input: Empty) -> Result<Overview, ApiError> {
        self.deps.push(Dep::JobSet);
        let now = now();
        let mut summaries = Vec::new();
        for job in self.host.job_ids() {
            let Some(handle) = self.host.job(job) else {
                continue;
            };
            if let Some(summary) = handle.read(&mut self.deps, move |l| summarize(l, now)) {
                summaries.push((job, summary));
            }
        }
        let titles: std::collections::BTreeMap<JobId, String> = summaries
            .iter()
            .map(|(j, s)| (*j, s.card.title.clone()))
            .collect();
        let queue = glance::queue(summaries.iter().map(|(j, s)| (*j, &s.glance)))
            .into_iter()
            .map(|(job, a)| QueueEntry {
                job: job.0.to_string(),
                title: titles[&job].clone(),
                attention: attention(a),
            })
            .collect();
        let jobs = summaries.into_iter().map(|(_, s)| s.card).collect();
        Ok(Overview { queue, jobs })
    }
    /// The job's card, plan, and head. `cursor` is the stamp of what was read, so it moves
    /// exactly when something shown here changed.
    fn job(&mut self, caller: &Caller, input: JobRef) -> Result<JobPulse, ApiError> {
        let (_, handle) = self.job_handle(&input.job)?;
        let now = now();
        let mut deps = Vec::new();
        let detail = handle.read(&mut deps, move |l| {
            let summary = summarize(l, now)?;
            let views = l.views();
            let info = views.runs().job()?;
            let mut plan = Vec::new();
            for node in &views.progress().tree().roots {
                plan_rows(node, 0, &mut plan);
            }
            Some(JobDetail {
                card: summary.card,
                plan,
                worktree: info.worktree.display().to_string(),
                base: info.base.as_str().to_owned(),
                head: views.review().head().map(|h| HeadRef {
                    tree: h.snapshot.tree.as_str().to_owned(),
                    seq: views.review().snapshots().len() as u32,
                }),
            })
        });
        let cursor = deps.iter().map(|d| match d {
            Dep::Views { stamp, .. } => stamp.get().to_string(),
            _ => String::new(),
        });
        let cursor = cursor.collect::<Vec<_>>().join(".");
        self.deps.extend(deps);
        let detail = detail.ok_or_else(|| ApiError::NotFound {
            what: format!("job {}", input.job),
        })?;
        Ok(JobPulse { cursor, detail })
    }
    /// Items in `[from_ms, to_ms)`, and per-window counts when `bucket` is set.
    fn timeline(
        &mut self,
        caller: &Caller,
        input: TimelineQuery,
    ) -> Result<TimelinePage, ApiError> {
        let (_, handle) = self.job_handle(&input.job)?;
        let (from, to) = (UnixMillis(input.from_ms), UnixMillis(input.to_ms));
        let width = input.bucket.map(bucket_width);
        let page = handle.read(&mut self.deps, move |l| {
            let timeline = l.views().timeline();
            let items = timeline
                .range(from, to)
                .map(|(at, fact, item)| timeline_item(at, fact, item))
                .collect();
            let buckets = width
                .map(|w| timeline.buckets(w, from, to))
                .unwrap_or_default()
                .into_iter()
                .map(|b| {
                    let summary = b.summary.as_ref().and_then(|fact| {
                        // A summary arrives after its window closed.
                        timeline
                            .range(b.window.end(), UnixMillis(i64::MAX))
                            .find(|(_, f, _)| *f == fact)
                            .map(|(_, _, i)| i.title.clone())
                    });
                    TimelineBucket {
                        start_ms: b.window.start.0,
                        width: bucket_wire(b.window.width),
                        tool_calls: b.counts.values().map(|c| c.tool_calls).sum(),
                        tool_errors: b.counts.values().map(|c| c.tool_errors).sum(),
                        summary,
                    }
                })
                .collect();
            TimelinePage { items, buckets }
        });
        Ok(page)
    }
    /// Stored frames exactly as stored. With a source: a page of it in order, from
    /// `around_fact` (or its start); `next` is the fact to ask from for the page after.
    /// With only a fact: its source's page from there, or the one authored event.
    fn raw(&mut self, caller: &Caller, input: RawQuery) -> Result<RawPage, ApiError> {
        let (_, handle) = self.job_handle(&input.job)?;
        if input.limit == 0 || input.limit > RAW_PAGE_MAX {
            return Err(ApiError::BadInput {
                path: "limit".into(),
                what: format!("between 1 and {RAW_PAGE_MAX}"),
            });
        }
        let around = input
            .around_fact
            .as_deref()
            .map(|f| {
                FactId::parse(f).ok_or_else(|| ApiError::BadInput {
                    path: "around_fact".into(),
                    what: format!("not a fact id: {f:?}"),
                })
            })
            .transpose()?;
        let source = match (input.source.as_deref(), &around) {
            (Some(s), _) => Some(SourceId(id(s, "source")?)),
            (None, Some(fact)) => source_of(&fact.event),
            (None, None) => {
                return Err(ApiError::BadInput {
                    path: "source".into(),
                    what: "a source or a fact to start from".into(),
                });
            }
        };
        let limit = usize::from(input.limit);
        handle.read(&mut self.deps, move |l| match source {
            Some(source) => {
                let from = around
                    .filter(|f| source_of(&f.event) == Some(source))
                    .map_or(Seq::FIRST, |f| f.event.seq);
                let mut records = l.raw_page(source, from, limit + 1).map_err(not_found)?;
                let next = (records.len() > limit).then(|| {
                    let extra = records.pop().expect("more than limit");
                    fact_of(extra.id).to_string()
                });
                let records = records.into_iter().map(raw_record).collect();
                Ok(RawPage { records, next })
            }
            None => {
                let fact = around.expect("a fact when there is no source");
                let record = l.raw(&fact.event).map_err(not_found)?;
                Ok(RawPage {
                    records: vec![raw_record(record)],
                    next: None,
                })
            }
        })
    }
    /// The review packet as a screen. Two short reads with git between them, so the
    /// job's sequencer is never held while git runs; a snapshot landing in between
    /// (`HeadMoved`) starts over.
    fn review(&mut self, caller: &Caller, input: JobRef) -> Result<ReviewScreen, ApiError> {
        let (_, handle) = self.job_handle(&input.job)?;
        let now = now();
        for _ in 0..REVIEW_TRIES {
            let (req, repo) = handle.read(&mut Vec::new(), |l| {
                let views = l.views();
                (
                    views.review().diff_request(),
                    views.runs().job().map(|j| j.repo.clone()),
                )
            });
            let diff = match (req, repo) {
                (Some(req), Some(repo)) => {
                    Some(self.host.diffs.get(&repo, &req).map_err(internal)?)
                }
                _ => None,
            };
            let cfg = self.host.config.packet.clone();
            let job = input.job.clone();
            let screen = handle.read(&mut self.deps, move |l| {
                review_screen(l, job, diff.as_deref(), &cfg, now)
            });
            if let Some(screen) = screen {
                return Ok(screen);
            }
        }
        Err(internal("the head kept moving while the review was read"))
    }
    fn presets(&mut self, caller: &Caller, input: Empty) -> Result<Presets, ApiError> {
        let presets = &self.host.config.presets;
        Ok(Presets {
            default: presets.default_name().as_str().to_owned(),
            presets: presets
                .iter()
                .map(|(name, models)| Preset {
                    name: name.as_str().to_owned(),
                    models: models.iter().map(model_pick).collect(),
                })
                .collect(),
        })
    }
    /// One review run per resolved model, all of the head snapshot. The models are
    /// resolved now and stored in the requests, so later preset edits change nothing here.
    fn request_review(
        &mut self,
        caller: &Caller,
        input: RequestReview,
    ) -> Result<RunRefs, ApiError> {
        let author = author(caller)?;
        let (job, handle) = self.job_handle(&input.job)?;
        let choice = match input.reviewers {
            Reviewers::Default {} => ReviewerChoice::Default,
            Reviewers::Preset { name } => {
                ReviewerChoice::Preset(PresetName::parse(&name).ok_or_else(|| {
                    ApiError::BadInput {
                        path: "reviewers.name".into(),
                        what: format!("not a preset name: {name:?}"),
                    }
                })?)
            }
            Reviewers::Models { models } => {
                let models = models
                    .into_iter()
                    .map(model_choice)
                    .collect::<Result<Vec<_>, _>>()?;
                ReviewerChoice::Models(ModelSet::new(models).ok_or_else(|| ApiError::BadInput {
                    path: "reviewers.models".into(),
                    what: "a review needs at least one model".into(),
                })?)
            }
        };
        let models = self
            .host
            .config
            .presets
            .resolve(&choice)
            .map_err(|UnknownPreset(name)| ApiError::BadInput {
                path: "reviewers.name".into(),
                what: format!("no preset named {:?}", name.as_str()),
            })?;
        let (tree, base) = head(&handle, job, "review")?;
        let panel = ReviewSpec::panel(
            derived("review", &input.request_id),
            &tree,
            &base,
            lens(input.lens),
            input.focus.as_deref(),
            &models,
        );
        let mut runs = Vec::new();
        for (run, purpose) in panel {
            self.submit(
                &handle,
                author.clone(),
                Event::RunRequested { run, purpose },
            )?;
            runs.push(RunRef {
                job: job.0.to_string(),
                run: run.0.to_string(),
            });
        }
        Ok(RunRefs { runs })
    }
    /// One check of the head snapshot; its run id derives from `request_id`. Device checks
    /// are refused until their idle cap and agent-driver prompt are designed.
    fn request_check(&mut self, caller: &Caller, input: RequestCheck) -> Result<RunRef, ApiError> {
        let author = author(caller)?;
        let (job, handle) = self.job_handle(&input.job)?;
        let path = |p: &str, at: &str| {
            RepoPath::parse(p).ok_or_else(|| ApiError::BadInput {
                path: format!("check.{at}"),
                what: format!("not a path in the repository: {p:?}"),
            })
        };
        let spec = match input.check {
            CheckChoice::Tla {
                spec,
                config,
                engine,
                workers,
            } => CheckSpec::Tla {
                spec: path(&spec, "spec")?,
                config: config.as_deref().map(|c| path(c, "config")).transpose()?,
                engine: match engine {
                    Engine::JavaTlc => TlaEngine::JavaTlc,
                    Engine::Tlars => TlaEngine::Tlars,
                },
                workers,
            },
            CheckChoice::Lean { root, files } => CheckSpec::Lean {
                root: path(&root, "root")?,
                mode: if files.is_empty() {
                    LeanMode::LakeBuild
                } else {
                    LeanMode::Files(
                        files
                            .iter()
                            .map(|f| path(f, "files"))
                            .collect::<Result<_, _>>()?,
                    )
                },
            },
            CheckChoice::Device { .. } => {
                return Err(ApiError::Rejected {
                    what: "device checks cannot be requested yet".into(),
                });
            }
        };
        let (tree, base) = head(&handle, job, "check")?;
        let run = RunId(derived("check", &input.request_id));
        let purpose = RunPurpose::Check(CheckRequest { tree, base, spec });
        self.submit(&handle, author, Event::RunRequested { run, purpose })?;
        Ok(RunRef {
            job: job.0.to_string(),
            run: run.0.to_string(),
        })
    }
    /// A draft thread; its id and its first comment's derive from `request_id`.
    fn open_thread(&mut self, caller: &Caller, input: OpenThread) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::ThreadOpened {
            thread: ThreadId(derived("thread", &input.request_id)),
            comment: CommentId(derived("comment", &input.request_id)),
            anchor: anchor(input.anchor)?,
            body: input.body,
        };
        self.submit(&handle, author, event)
    }
    fn reply(&mut self, caller: &Caller, input: Reply) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::Replied {
            comment: CommentId(derived("comment", &input.request_id)),
            thread: ThreadId(id(&input.thread, "thread")?),
            body: input.body,
        };
        self.submit(&handle, author, event)
    }
    fn resolve(&mut self, caller: &Caller, input: Resolve) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::ThreadResolved {
            thread: ThreadId(id(&input.thread, "thread")?),
            resolved: input.resolved,
        };
        self.submit(&handle, author, event)
    }
    fn promote_finding(&mut self, caller: &Caller, input: PromoteFinding) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::FindingPromoted {
            finding: finding(&input.finding)?,
            thread: ThreadId(derived("promote", &input.request_id)),
            comment: CommentId(derived("comment", &input.request_id)),
        };
        self.submit(&handle, author, event)
    }
    fn dismiss_finding(&mut self, caller: &Caller, input: DismissFinding) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::FindingDismissed {
            finding: finding(&input.finding)?,
            dismissed: input.dismissed,
        };
        self.submit(&handle, author, event)
    }
    /// Byte-for-byte what a `RequestChanges` now would deliver (the same `render_feedback`
    /// the policy uses).
    fn preview_feedback(
        &mut self,
        caller: &Caller,
        input: PreviewFeedback,
    ) -> Result<FeedbackText, ApiError> {
        let (_, handle) = self.job_handle(&input.job)?;
        let note = input.note;
        let (text, comments) = handle.read(&mut self.deps, move |l| {
            let views = l.views();
            (
                views.preview_feedback(&note),
                views.review().feedback(None).len() as u32,
            )
        });
        Ok(FeedbackText { text, comments })
    }
    /// A verdict on the tree the human looked at; a newer head refuses it (`StaleHead`).
    fn decide(&mut self, caller: &Caller, input: Decide) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::Decided {
            decision: DecisionId(derived("decision", &input.request_id)),
            tree: input.tree.as_deref().map(|t| tree(t, "tree")).transpose()?,
            verdict: decision(input.verdict),
            note: input.note,
        };
        self.submit(&handle, author, event)
    }
    /// Merges the approved head. Without a current approval of exactly `tree`, `StaleHead`;
    /// the ledger checks again on the sequencer, so an approval revoked meanwhile fails too.
    fn merge(&mut self, caller: &Caller, input: MergeJob) -> Result<RunRef, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let wanted = tree(&input.tree, "tree")?;
        let (token, head) = handle.read(&mut self.deps, |l| {
            let review = l.views().review();
            (
                review.approved_head(),
                review.head().map(|h| h.snapshot.tree.as_str().to_owned()),
            )
        });
        let stale = || ApiError::StaleHead { head: head.clone() };
        let token = token.filter(|t| t.tree() == &wanted).ok_or_else(stale)?;
        let run = RunId(Id128::derive("merge", wanted.as_str().as_bytes()));
        let request = Event::RunRequested {
            run,
            purpose: RunPurpose::Merge(token),
        };
        match handle.submit(author, request) {
            Err(LedgerError::Rejected(Rejected::NotApproved)) => Err(stale()),
            Err(e) => Err(ledger_error(e)),
            Ok(_) => Ok(RunRef {
                job: input.job,
                run: run.0.to_string(),
            }),
        }
    }
    /// One job per chosen agent, each in its own worktree of `repo` at `base` (default:
    /// `HEAD`), merging into the branch checked out there. Several agents share a group,
    /// for side-by-side comparison. Ids derive from `request_id`, so a retry is harmless.
    fn start_jobs(&mut self, caller: &Caller, input: StartJobs) -> Result<Started, ApiError> {
        let author = author(caller)?;
        let repo = PathBuf::from(&input.repo);
        let base = git::resolve(&repo, input.base.as_deref().unwrap_or("HEAD")).map_err(|e| {
            ApiError::BadInput {
                path: "base".into(),
                what: e.to_string(),
            }
        })?;
        let target = git::current_branch(&repo).unwrap_or_else(|| "main".into());
        let group = (input.agents.len() > 1).then(|| derived("group", &input.request_id));
        let mut jobs = Vec::new();
        for (i, choice) in input.agents.iter().enumerate() {
            let job = JobId(derived("job", &format!("{}\0{i}", input.request_id)));
            let at = self
                .host
                .config
                .data_root
                .join("worktrees")
                .join(job.0.to_string());
            let worktree = git::create_worktree(&repo, &base, job, &at).map_err(internal)?;
            let info = JobInfo {
                job,
                title: input.title.clone(),
                repo: repo.clone(),
                base: base.clone(),
                target: target.clone(),
                worktree,
                group,
                origin: JobOrigin::Launched,
            };
            self.host.create_job(info).map_err(internal)?;
            let handle = self
                .host
                .job(job)
                .ok_or_else(|| internal("the job did not open"))?;
            let launch = AgentLaunch {
                cli: agent_cli(choice.agent),
                model: choice.model.clone().map(ModelId),
                prompt: input.prompt.clone(),
                resume: None,
            };
            let run = RunId(Id128::derive("agent", &job.0.0.to_le_bytes()));
            self.submit(
                &handle,
                author.clone(),
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Agent(launch),
                },
            )?;
            jobs.push(job.0.to_string());
        }
        Ok(Started { jobs })
    }
    /// Bytes for the run's terminal, recorded first (`RunInput`), typed once.
    fn send_input(&mut self, caller: &Caller, input: SendInput) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let event = Event::RunInput {
            input: InputId(derived("input", &input.request_id)),
            run: RunId(id(&input.run, "run")?),
            bytes: input.text.into_bytes(),
        };
        self.submit(&handle, author, event)
    }
    fn answer_permission(
        &mut self,
        caller: &Caller,
        input: AnswerPermission,
    ) -> Result<Ack, ApiError> {
        // TODO: cli of the run; submit(Human, RunInput { input: uuid(request_id), run,
        //       bytes: plan::permission_input(cli, answer) }).
        todo!("not implemented")
    }
    fn cancel_run(&mut self, caller: &Caller, input: RunRef) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let run = RunId(id(&input.run, "run")?);
        let (_, handle) = self.job_handle(&input.job)?;
        self.submit(&handle, author, Event::RunCancelRequested { run })
    }
    /// A new task (no `task`) needs a title; its key derives from who adds it, the
    /// parent, and the title, so a retried `argus task add` adds nothing. An agent edits
    /// only the tasks it wrote last; it may add new ones under any parent.
    fn set_task(&mut self, caller: &Caller, input: SetTask) -> Result<TaskSaved, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let parent = input
            .parent
            .as_deref()
            .map(|p| id(p, "parent").map(TaskKey))
            .transpose()?;
        let weight = input
            .weight
            .map(|w| {
                std::num::NonZeroU32::new(w)
                    .map(Weight)
                    .ok_or_else(|| ApiError::BadInput {
                        path: "weight".into(),
                        what: "a weight is at least 1".into(),
                    })
            })
            .transpose()?;
        let task = match &input.task {
            Some(task) => TaskKey(id(task, "task")?),
            None => {
                let Some(title) = &input.title else {
                    return Err(ApiError::BadInput {
                        path: "title".into(),
                        what: "a new task needs a title".into(),
                    });
                };
                let by = match author {
                    Author::Agent(run) => run.0.to_string(),
                    _ => "human".into(),
                };
                let parent = parent.map_or(String::new(), |p| p.0.to_string());
                TaskKey(Id128::derive(
                    "task",
                    format!("{by}\0{parent}\0{title}").as_bytes(),
                ))
            }
        };
        if let Author::Agent(run) = author {
            let by = handle.read(&mut Vec::new(), move |l| {
                l.views().progress().task(&task).map(|t| t.by)
            });
            if by.is_some_and(|by| by != progress::PlanAuthor::Agent(run)) {
                return Err(ApiError::Forbidden {
                    op: "set_task".into(),
                });
            }
        }
        let patch = TaskPatch {
            // A new task without a parent is a root; an edit without one keeps its place.
            parent: match (parent, &input.task) {
                (Some(p), _) => Some(Some(p)),
                (None, None) => Some(None),
                (None, Some(_)) => None,
            },
            title: input.title,
            weight,
            state: input.status.map(task_state),
        };
        self.submit(&handle, author, Event::TaskSet { task, patch })?;
        Ok(TaskSaved {
            task: task.0.to_string(),
        })
    }
    /// A Claude Code hook payload from inside a run, appended verbatim as one record of
    /// that run's hook log. Only the agent itself may send one.
    fn hook(&mut self, caller: &Caller, input: HookPayload) -> Result<Ack, ApiError> {
        let Author::Agent(run) = author(caller)? else {
            return Err(ApiError::Forbidden { op: "hook".into() });
        };
        let owner = self.host.job_ids().into_iter().find_map(|job| {
            let handle = self.host.job(job)?;
            let attempt = handle.read(&mut Vec::new(), move |l| {
                l.views()
                    .runs()
                    .get(&run)
                    .map(|r| r.attempts.last().map(|a| a.attempt))
            })?;
            Some((handle, attempt))
        });
        let (handle, attempt) = match owner {
            Some((handle, Some(attempt))) => (handle, attempt),
            Some((_, None)) => {
                return Err(ApiError::NotFound {
                    what: format!("a started attempt of run {}", run.0),
                });
            }
            None => {
                return Err(ApiError::NotFound {
                    what: format!("run {}", run.0),
                });
            }
        };
        let origin = SourceOrigin::Hook;
        let meta = SourceMeta {
            id: SourceMeta::derive_id(run, attempt, &origin),
            run,
            attempt,
            kind: SourceKind::ClaudeHook,
            origin,
        };
        crate::ledger_thread::record(&handle.sender(), meta, input.body.into_bytes())
            .map_err(internal)?;
        Ok(Ack { duplicate: false })
    }
    fn bookmark(&mut self, caller: &Caller, input: Bookmark) -> Result<Ack, ApiError> {
        let author = author(caller)?;
        let (_, handle) = self.job_handle(&input.job)?;
        let at = FactId::parse(&input.fact).ok_or_else(|| ApiError::BadInput {
            path: "fact".into(),
            what: format!("not a fact id: {:?}", input.fact),
        })?;
        let kind = match input.kind {
            BookmarkKind::Idea => event::BookmarkKind::Idea,
            BookmarkKind::Improvement => event::BookmarkKind::Improvement,
            BookmarkKind::Pitfall => event::BookmarkKind::Pitfall,
            BookmarkKind::Exemplar => event::BookmarkKind::Exemplar,
        };
        let event = Event::Bookmarked {
            bookmark: BookmarkId(derived("bookmark", &input.request_id)),
            at,
            kind,
            note: input.note,
        };
        self.submit(&handle, author, event)
    }
    fn stats(&mut self, caller: &Caller, input: StatsQuery) -> Result<StatsTable, ApiError> {
        // TODO: Period::parse(input.period, now, tz); store.analytics()?.slice(..)
        todo!("not implemented")
    }
    fn candidates(&mut self, caller: &Caller, input: Empty) -> Result<Candidates, ApiError> {
        todo!("not implemented")
    }
    fn request_insights(
        &mut self,
        caller: &Caller,
        input: RequestInsights,
    ) -> Result<RunRef, ApiError> {
        // TODO: prompt = store.insight_prompt(period, existing skills) (stored verbatim in
        //       the request); meta ledger submit RunRequested{ Insight{period, llm} }.
        todo!("not implemented")
    }
    fn accept_candidate(
        &mut self,
        caller: &Caller,
        input: AcceptCandidate,
    ) -> Result<RunRef, ApiError> {
        todo!("not implemented")
    }
    fn dismiss_candidate(&mut self, caller: &Caller, input: CandidateRef) -> Result<Ack, ApiError> {
        todo!("not implemented")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    use argus_api::{ApiError, Caller};
    use argus_json::{Json, parse};

    use crate::Host;

    fn scratch() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "argus-handler-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
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

    /// A host over a fresh store, and a repository on `main` with one commit.
    fn host() -> (Host, PathBuf, PathBuf) {
        let root = scratch();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);
        let host = Host::start(crate::tests::config(root.join("data"))).unwrap();
        (host, root, repo)
    }

    fn call(host: &Host, caller: &Caller, op: &str, input: &str) -> Result<Json, ApiError> {
        host.call(caller, op, &parse(input.as_bytes()).unwrap())
    }

    fn text<'a>(v: &'a Json, path: &[&str]) -> &'a str {
        let mut at = v;
        for key in path {
            at = at
                .get(key)
                .unwrap_or_else(|| panic!("no {key} in {}", argus_json::write(at)));
        }
        at.as_str().unwrap()
    }

    /// Starts one Codex job (Codex cannot run yet, so no process starts) and returns its id.
    fn start(host: &Host, repo: &Path, request: &str) -> String {
        let input = format!(
            r#"{{"request_id":"{request}","repo":"{}","base":null,"title":"fix the gate","prompt":"go","agents":[{{"agent":"Codex","model":null}}]}}"#,
            repo.display()
        );
        let started = call(host, &Caller::Local, "start_jobs", &input).unwrap();
        let Some(Json::Arr(jobs)) = started.get("jobs") else {
            panic!("{}", argus_json::write(&started))
        };
        assert_eq!(jobs.len(), 1);
        jobs[0].as_str().unwrap().to_owned()
    }

    #[test]
    fn starting_a_job_makes_its_worktree_and_shows_it_in_the_overview() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-1");
        let worktree = root.join("data").join("worktrees").join(&job);
        assert_eq!(
            git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            format!("argus/{job}")
        );
        assert_eq!(
            git(&worktree, &["rev-parse", "HEAD"]),
            git(&repo, &["rev-parse", "main"])
        );
        // A retried request is the same job.
        assert_eq!(start(&host, &repo, "req-1"), job);

        let overview = call(&host, &Caller::Local, "overview", "{}").unwrap();
        let Some(Json::Arr(cards)) = overview.get("jobs") else {
            panic!("{}", argus_json::write(&overview))
        };
        assert_eq!(cards.len(), 1);
        assert_eq!(text(&cards[0], &["job"]), job);
        assert_eq!(text(&cards[0], &["title"]), "fix the gate");
        assert_eq!(text(&cards[0], &["state", "kind"]), "Working");
        host.shutdown();
    }

    #[test]
    fn typed_input_and_cancels_are_recorded_for_the_run() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-2");
        // The agent run's id derives from its job (Codex fails at once, so the overview
        // no longer lists it as active).
        let job_id = argus_core::ids::Id128::parse(&job).unwrap();
        let run = argus_core::ids::Id128::derive("agent", &job_id.0.to_le_bytes()).to_string();
        let input = format!(r#"{{"request_id":"in-1","job":"{job}","run":"{run}","text":"y\r"}}"#);
        let ack = call(&host, &Caller::Local, "send_input", &input).unwrap();
        assert_eq!(ack.get("duplicate"), Some(&Json::Bool(false)));
        let again = call(&host, &Caller::Local, "send_input", &input).unwrap();
        assert_eq!(again.get("duplicate"), Some(&Json::Bool(true)));
        let cancel = format!(r#"{{"job":"{job}","run":"{run}"}}"#);
        call(&host, &Caller::Local, "cancel_run", &cancel).unwrap();

        let id: argus_core::ids::JobId =
            argus_core::ids::JobId(argus_core::ids::Id128::parse(&job).unwrap());
        let run_id = argus_core::ids::RunId(argus_core::ids::Id128::parse(&run).unwrap());
        let handle = host.inner.jobs.lock().unwrap()[&id].0.clone();
        let (cancelled, typed) = handle.read(&mut Vec::new(), move |l| {
            let cancelled = l.views().runs().get(&run_id).unwrap().cancel_requested;
            let typed = l
                .views()
                .timeline()
                .range(
                    argus_core::ids::UnixMillis(0),
                    argus_core::ids::UnixMillis(i64::MAX),
                )
                .filter(|(_, _, i)| {
                    matches!(i.kind, argus_core::views::timeline::ItemKind::HumanInput)
                })
                .count();
            (cancelled, typed)
        });
        assert!(cancelled);
        assert_eq!(typed, 1);
        host.shutdown();
    }

    #[test]
    fn callers_are_who_they_say_and_ids_are_checked() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-3");
        let agent = Caller::Agent {
            run: "0f8fad5d-d9cb-469f-a165-70867728950e".into(),
        };
        let input = format!(
            r#"{{"request_id":"in-1","job":"{job}","run":"0f8fad5d-d9cb-469f-a165-70867728950e","text":"x"}}"#
        );
        assert!(matches!(
            call(&host, &agent, "send_input", &input),
            Err(ApiError::Forbidden { .. })
        ));
        let bad = r#"{"job":"not-a-job","run":"also-not"}"#;
        assert!(matches!(
            call(&host, &Caller::Local, "cancel_run", bad),
            Err(ApiError::BadInput { .. })
        ));
        let unknown = r#"{"job":"0f8fad5d-d9cb-469f-a165-70867728950e","run":"0f8fad5d-d9cb-469f-a165-70867728950e"}"#;
        assert!(matches!(
            call(&host, &Caller::Local, "cancel_run", unknown),
            Err(ApiError::NotFound { .. })
        ));
        host.shutdown();
    }

    /// Edits the job's worktree and records a snapshot of it through the job's loop,
    /// the way the policy would; returns the head tree.
    fn head_after_edit(host: &Host, root: &Path, job: &str) -> String {
        use argus_core::event::{Author, Event, RunPurpose};
        use argus_core::ids::{Id128, JobId, RunId};
        let worktree = root.join("data").join("worktrees").join(job);
        std::fs::write(worktree.join("a.txt"), "one\ntwo\n").unwrap();
        let id = JobId(Id128::parse(job).unwrap());
        let handle = host.inner.job(id).unwrap();
        let run = RunId(Id128::derive("test-snapshot", job.as_bytes()));
        let request = Event::RunRequested {
            run,
            purpose: RunPurpose::Snapshot { after: None },
        };
        handle.submit(Author::Policy, request).unwrap();
        for _ in 0..200 {
            let head = handle.read(&mut Vec::new(), |l| {
                l.views()
                    .review()
                    .head()
                    .map(|h| h.snapshot.tree.as_str().to_owned())
            });
            if let Some(tree) = head {
                return tree;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no snapshot");
    }

    #[test]
    fn a_verdict_on_the_head_then_a_merge_moves_the_target() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-merge");
        let tree = head_after_edit(&host, &root, &job);

        let stale = format!(
            r#"{{"request_id":"d-0","job":"{job}","tree":"{}","verdict":"Approve","note":""}}"#,
            "1".repeat(40)
        );
        match call(&host, &Caller::Local, "decide", &stale) {
            Err(ApiError::StaleHead { head }) => assert_eq!(head.as_deref(), Some(tree.as_str())),
            other => panic!("{other:?}"),
        }
        let early = format!(r#"{{"job":"{job}","tree":"{tree}"}}"#);
        assert!(matches!(
            call(&host, &Caller::Local, "merge", &early),
            Err(ApiError::StaleHead { .. })
        ));

        let approve = format!(
            r#"{{"request_id":"d-1","job":"{job}","tree":"{tree}","verdict":"Approve","note":"lgtm"}}"#
        );
        call(&host, &Caller::Local, "decide", &approve).unwrap();
        let merged = call(&host, &Caller::Local, "merge", &early).unwrap();
        assert_eq!(text(&merged, &["job"]), job);
        for _ in 0..200 {
            if git(&repo, &["rev-parse", "main^{tree}"]) == tree {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(git(&repo, &["rev-parse", "main^{tree}"]), tree);
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        host.shutdown();
    }

    #[test]
    fn threads_build_the_feedback_the_agent_would_receive() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-threads");
        let tree = head_after_edit(&host, &root, &job);
        let open = format!(
            r#"{{"request_id":"t-1","job":"{job}","anchor":{{"kind":"Code","tree":"{tree}","path":"a.txt","start":1,"end":1}},"body":"keep it"}}"#
        );
        call(&host, &Caller::Local, "open_thread", &open).unwrap();
        let thread = argus_core::ids::Id128::derive("thread", b"t-1").to_string();
        let reply = format!(
            r#"{{"request_id":"t-2","job":"{job}","thread":"{thread}","body":"and test it"}}"#
        );
        call(&host, &Caller::Local, "reply", &reply).unwrap();

        let preview = format!(r#"{{"job":"{job}","note":"Almost."}}"#);
        let text_of = |v: &Json| (text(v, &["text"]).to_owned(), v.get("comments").cloned());
        let (body, comments) =
            text_of(&call(&host, &Caller::Local, "preview_feedback", &preview).unwrap());
        assert_eq!(
            body,
            "Almost.\n\n- a.txt:1: keep it\n- a.txt:1: and test it\n"
        );
        assert_eq!(comments, Some(Json::Num(argus_json::Number::from_u64(2))));

        let resolve = format!(r#"{{"job":"{job}","thread":"{thread}","resolved":true}}"#);
        call(&host, &Caller::Local, "resolve", &resolve).unwrap();
        let (_, comments) =
            text_of(&call(&host, &Caller::Local, "preview_feedback", &preview).unwrap());
        assert_eq!(comments, Some(Json::Num(argus_json::Number::from_u64(0))));

        let bad = format!(
            r#"{{"request_id":"t-3","job":"{job}","anchor":{{"kind":"Code","tree":"{tree}","path":"../x","start":1,"end":1}},"body":"x"}}"#
        );
        assert!(matches!(
            call(&host, &Caller::Local, "open_thread", &bad),
            Err(ApiError::BadInput { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn a_finding_can_be_dismissed_by_its_id() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-finding");
        let finding = "src-0f8fad5d-d9cb-469f-a165-70867728950e:3.0#1";
        let dismiss = format!(r#"{{"job":"{job}","finding":"{finding}","dismissed":true}}"#);
        call(&host, &Caller::Local, "dismiss_finding", &dismiss).unwrap();
        let id = argus_core::ids::JobId(argus_core::ids::Id128::parse(&job).unwrap());
        let parsed = argus_core::ids::FindingId::parse(finding).unwrap();
        let dismissed = host.inner.job(id).unwrap().read(&mut Vec::new(), move |l| {
            l.views().review().is_dismissed(&parsed)
        });
        assert!(dismissed);
        let bad = format!(r#"{{"job":"{job}","finding":"nope","dismissed":true}}"#);
        assert!(matches!(
            call(&host, &Caller::Local, "dismiss_finding", &bad),
            Err(ApiError::BadInput { .. })
        ));
        host.shutdown();
    }

    fn get<'a>(v: &'a Json, key: &str) -> &'a Json {
        v.get(key)
            .unwrap_or_else(|| panic!("no {key} in {}", argus_json::write(v)))
    }

    #[test]
    fn a_job_shows_its_worktree_plan_and_head_and_a_new_cursor_after_each_change() {
        use argus_core::event::{Author, Event, TaskPatch};
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-job");
        let ask = format!(r#"{{"job":"{job}"}}"#);
        let first = call(&host, &Caller::Local, "job", &ask).unwrap();
        let detail = get(&first, "detail");
        assert_eq!(text(detail, &["card", "job"]), job);
        let worktree = root.join("data").join("worktrees").join(&job);
        assert_eq!(text(detail, &["worktree"]), worktree.display().to_string());
        assert_eq!(text(detail, &["base"]), git(&repo, &["rev-parse", "main"]));
        assert_eq!(get(detail, "head"), &Json::Null);
        assert_eq!(get(detail, "plan"), &Json::Arr(vec![]));

        let id = argus_core::ids::JobId(argus_core::ids::Id128::parse(&job).unwrap());
        let task = argus_core::ids::TaskKey(argus_core::ids::Id128(5));
        let patch = TaskPatch {
            title: Some("ship it".into()),
            ..TaskPatch::default()
        };
        host.inner
            .job(id)
            .unwrap()
            .submit(Author::Human, Event::TaskSet { task, patch })
            .unwrap();
        let second = call(&host, &Caller::Local, "job", &ask).unwrap();
        assert_ne!(text(&first, &["cursor"]), text(&second, &["cursor"]));
        let Json::Arr(plan) = get(get(&second, "detail"), "plan") else {
            panic!()
        };
        assert_eq!(plan.len(), 1);
        assert_eq!(text(&plan[0], &["title"]), "ship it");
        assert_eq!(text(&plan[0], &["source"]), "human");
        assert_eq!(text(&plan[0], &["status", "kind"]), "Pending");
        assert_eq!(text(&plan[0], &["key"]), task.0.to_string());

        let tree = head_after_edit(&host, &root, &job);
        let third = call(&host, &Caller::Local, "job", &ask).unwrap();
        let head = get(get(&third, "detail"), "head");
        assert_eq!(text(head, &["tree"]), tree);
        assert_eq!(
            get(head, "seq"),
            &Json::Num(argus_json::Number::from_u64(1))
        );

        let missing = r#"{"job":"0f8fad5d-d9cb-469f-a165-70867728950e"}"#;
        assert!(matches!(
            call(&host, &Caller::Local, "job", missing),
            Err(ApiError::NotFound { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn tasks_are_created_once_per_title_and_agents_edit_only_their_own() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-tasks");
        let plan = |host: &Host| {
            let v = call(
                host,
                &Caller::Local,
                "job",
                &format!(r#"{{"job":"{job}"}}"#),
            )
            .unwrap();
            let Json::Arr(rows) = get(get(&v, "detail"), "plan").clone() else {
                panic!()
            };
            rows
        };
        let add = format!(r#"{{"job":"{job}","title":"ship it"}}"#);
        let saved = call(&host, &Caller::Local, "set_task", &add).unwrap();
        let task = text(&saved, &["task"]).to_owned();
        let again = call(&host, &Caller::Local, "set_task", &add).unwrap();
        assert_eq!(
            text(&again, &["task"]),
            task,
            "a retried add makes no second task"
        );
        assert_eq!(plan(&host).len(), 1);

        let done = format!(
            r#"{{"job":"{job}","task":"{task}","status":{{"kind":"Done","result":"shipped"}}}}"#
        );
        call(&host, &Caller::Local, "set_task", &done).unwrap();
        let rows = plan(&host);
        assert_eq!(text(&rows[0], &["status", "kind"]), "Done");
        assert_eq!(text(&rows[0], &["status", "result"]), "shipped");
        assert_eq!(
            text(&rows[0], &["title"]),
            "ship it",
            "a patch keeps what it omits"
        );

        let id = argus_core::ids::Id128::parse(&job).unwrap();
        let run = argus_core::ids::Id128::derive("agent", &id.0.to_le_bytes()).to_string();
        let agent = Caller::Agent { run: run.clone() };
        let step = format!(r#"{{"job":"{job}","parent":"{task}","title":"step","weight":2}}"#);
        let saved = call(&host, &agent, "set_task", &step).unwrap();
        let sub = text(&saved, &["task"]).to_owned();
        assert_ne!(sub, task);
        let rows = plan(&host);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(text(&rows[1], &["key"]), sub);
        assert_eq!(text(&rows[1], &["source"]), run);
        assert_eq!(
            rows[1].get("depth"),
            Some(&Json::Num(argus_json::Number::from_u64(1)))
        );

        let mine = format!(r#"{{"job":"{job}","task":"{sub}","status":{{"kind":"InProgress"}}}}"#);
        call(&host, &agent, "set_task", &mine).unwrap();
        assert!(matches!(
            call(&host, &agent, "set_task", &done),
            Err(ApiError::Forbidden { .. })
        ));

        for bad in [
            format!(r#"{{"job":"{job}"}}"#),
            format!(r#"{{"job":"{job}","title":"x","weight":0}}"#),
            format!(r#"{{"job":"{job}","task":"nope","title":"x"}}"#),
        ] {
            assert!(
                matches!(
                    call(&host, &Caller::Local, "set_task", &bad),
                    Err(ApiError::BadInput { .. })
                ),
                "{bad}"
            );
        }
        host.shutdown();
    }

    #[test]
    fn a_bookmark_marks_a_fact_once_and_shows_on_the_timeline() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-bookmark");
        let ask =
            format!(r#"{{"job":"{job}","from_ms":0,"to_ms":9007199254740991,"bucket":"H1"}}"#);
        let page = call(&host, &Caller::Local, "timeline", &ask).unwrap();
        let Json::Arr(items) = get(&page, "items") else {
            panic!()
        };
        let fact = text(&items[0], &["fact"]).to_owned();
        let mark = format!(
            r#"{{"request_id":"bm-1","job":"{job}","fact":"{fact}","kind":"Pitfall","note":"slow start\nmore"}}"#
        );
        let ack = call(&host, &Caller::Local, "bookmark", &mark).unwrap();
        assert_eq!(get(&ack, "duplicate"), &Json::Bool(false));
        let ack = call(&host, &Caller::Local, "bookmark", &mark).unwrap();
        assert_eq!(get(&ack, "duplicate"), &Json::Bool(true));

        let page = call(&host, &Caller::Local, "timeline", &ask).unwrap();
        let Json::Arr(items) = get(&page, "items") else {
            panic!()
        };
        let titles: Vec<&str> = items.iter().map(|i| text(i, &["title"])).collect();
        assert_eq!(
            titles
                .iter()
                .filter(|t| **t == "Pitfall: slow start")
                .count(),
            1,
            "{titles:?}"
        );

        let bad = format!(
            r#"{{"request_id":"bm-2","job":"{job}","fact":"nope","kind":"Idea","note":""}}"#
        );
        assert!(matches!(
            call(&host, &Caller::Local, "bookmark", &bad),
            Err(ApiError::BadInput { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn a_hook_payload_is_kept_verbatim_in_its_runs_hook_log() {
        use argus_core::ids::{Attempt, Id128, RunId};
        use argus_core::source::{SourceMeta, SourceOrigin};
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-hook");
        let id = Id128::parse(&job).unwrap();
        let run = RunId(Id128::derive("agent", &id.0.to_le_bytes()));
        let agent = Caller::Agent {
            run: run.0.to_string(),
        };
        let bodies = [
            r#"{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#,
            r#"{"hook_event_name":"Stop"}"#,
        ];
        for body in bodies {
            let input = Json::Obj(vec![("body".into(), Json::str(body))]);
            host.call(&agent, "hook", &input).unwrap();
        }
        let source = SourceMeta::derive_id(run, Attempt(0), &SourceOrigin::Hook);
        let ask = format!(r#"{{"job":"{job}","source":"{}","limit":10}}"#, source.0);
        let page = call(&host, &Caller::Local, "raw", &ask).unwrap();
        assert_eq!(records(&page), bodies);

        let input = Json::Obj(vec![("body".into(), Json::str(bodies[1]))]);
        assert!(matches!(
            host.call(&Caller::Local, "hook", &input),
            Err(ApiError::Forbidden { .. })
        ));
        let stranger = Caller::Agent {
            run: Id128(7).to_string(),
        };
        assert!(matches!(
            host.call(&stranger, "hook", &input),
            Err(ApiError::NotFound { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn the_presets_list_their_models_and_the_default() {
        let (host, _root, _repo) = host();
        let presets = call(&host, &Caller::Local, "presets", "{}").unwrap();
        assert_eq!(text(&presets, &["default"]), "default");
        let Json::Arr(list) = get(&presets, "presets") else {
            panic!()
        };
        assert_eq!(list.len(), 1);
        assert_eq!(text(&list[0], &["name"]), "default");
        let Json::Arr(models) = get(&list[0], "models") else {
            panic!()
        };
        assert_eq!(text(&models[0], &["agent"]), "Claude");
        assert_eq!(text(&models[0], &["model"]), "opus");
        host.shutdown();
    }

    #[test]
    fn a_review_request_asks_each_model_once_about_the_head() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-review-runs");
        let ask = |reviewers: &str, request: &str| {
            format!(
                r#"{{"request_id":"{request}","job":"{job}","reviewers":{reviewers},"lens":"Security","focus":"the parser"}}"#
            )
        };
        let default = ask(r#"{"kind":"Default"}"#, "rv-1");
        assert!(
            matches!(
                call(&host, &Caller::Local, "request_review", &default),
                Err(ApiError::NotFound { .. })
            ),
            "nothing to review before the first snapshot"
        );

        let tree = head_after_edit(&host, &root, &job);
        let runs = |v: &Json| -> Vec<String> {
            let Json::Arr(runs) = get(v, "runs") else {
                panic!()
            };
            runs.iter().map(|r| text(r, &["run"]).to_owned()).collect()
        };
        let first = runs(&call(&host, &Caller::Local, "request_review", &default).unwrap());
        assert_eq!(first.len(), 1);
        let again = runs(&call(&host, &Caller::Local, "request_review", &default).unwrap());
        assert_eq!(again, first, "a retried request asks no one new");

        let two = ask(
            r#"{"kind":"Models","models":[{"agent":"Codex","model":"gpt-5","effort":"High"},{"agent":"Claude","model":"sonnet","effort":null}]}"#,
            "rv-2",
        );
        let panel = runs(&call(&host, &Caller::Local, "request_review", &two).unwrap());
        assert_eq!(panel.len(), 2);
        assert!(panel.iter().all(|r| !first.contains(r)));

        let screen = call(
            &host,
            &Caller::Local,
            "review",
            &format!(r#"{{"job":"{job}"}}"#),
        )
        .unwrap();
        let Json::Arr(cards) = get(&screen, "reviews") else {
            panic!()
        };
        let mut shown: Vec<&str> = cards.iter().map(|c| text(c, &["run"])).collect();
        shown.sort();
        let mut asked: Vec<&str> = first.iter().chain(&panel).map(String::as_str).collect();
        asked.sort();
        assert_eq!(shown, asked);
        assert!(cards.iter().all(|c| text(c, &["tree"]) == tree));
        assert!(cards.iter().all(|c| text(c, &["lens"]) == "Security"));

        for bad in [
            ask(r#"{"kind":"Preset","name":"nope"}"#, "rv-3"),
            ask(r#"{"kind":"Preset","name":"Not Kebab"}"#, "rv-4"),
            ask(r#"{"kind":"Models","models":[]}"#, "rv-5"),
            ask(
                r#"{"kind":"Models","models":[{"agent":"Claude","model":"--help","effort":null}]}"#,
                "rv-6",
            ),
        ] {
            assert!(
                matches!(
                    call(&host, &Caller::Local, "request_review", &bad),
                    Err(ApiError::BadInput { .. })
                ),
                "{bad}"
            );
        }
        host.shutdown();
    }

    #[test]
    fn a_check_request_runs_once_on_the_head_and_shows_on_the_review_screen() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-check");
        let ask = |check: &str, request: &str| {
            format!(r#"{{"request_id":"{request}","job":"{job}","check":{check}}}"#)
        };
        let tla = ask(
            r#"{"kind":"Tla","spec":"specs/Job.tla","config":"specs/Job.cfg","engine":"Tlars","workers":4}"#,
            "ck-1",
        );
        assert!(matches!(
            call(&host, &Caller::Local, "request_check", &tla),
            Err(ApiError::NotFound { .. })
        ));

        let tree = head_after_edit(&host, &root, &job);
        let first = call(&host, &Caller::Local, "request_check", &tla).unwrap();
        assert_eq!(text(&first, &["job"]), job);
        let again = call(&host, &Caller::Local, "request_check", &tla).unwrap();
        assert_eq!(text(&again, &["run"]), text(&first, &["run"]));
        let lean = ask(r#"{"kind":"Lean","root":"proofs","files":[]}"#, "ck-2");
        let second = call(&host, &Caller::Local, "request_check", &lean).unwrap();

        let screen = call(
            &host,
            &Caller::Local,
            "review",
            &format!(r#"{{"job":"{job}"}}"#),
        )
        .unwrap();
        let Json::Arr(cards) = get(&screen, "checks") else {
            panic!()
        };
        let mut shown: Vec<(&str, &str)> = cards
            .iter()
            .map(|c| (text(c, &["run"]), text(c, &["target", "kind"])))
            .collect();
        shown.sort();
        let mut asked = vec![
            (text(&first, &["run"]), "Tla"),
            (text(&second, &["run"]), "Lean"),
        ];
        asked.sort();
        assert_eq!(shown, asked);
        assert!(cards.iter().all(|c| text(c, &["tree"]) == tree));
        let tla_card = cards
            .iter()
            .find(|c| text(c, &["target", "kind"]) == "Tla")
            .unwrap();
        assert_eq!(text(tla_card, &["target", "config"]), "specs/Job.cfg");
        assert_eq!(text(tla_card, &["engine"]), "Tlars");

        let device = ask(
            r#"{"kind":"Device","project":"app","platform":"Ios","app":{"kind":"ExpoGo","sdk":null},"driver":{"kind":"Script","path":"e2e.sh"},"max_minutes":10}"#,
            "ck-3",
        );
        assert!(matches!(
            call(&host, &Caller::Local, "request_check", &device),
            Err(ApiError::Rejected { .. })
        ));
        let outside = ask(
            r#"{"kind":"Tla","spec":"../Job.tla","config":null,"engine":"JavaTlc","workers":null}"#,
            "ck-4",
        );
        assert!(matches!(
            call(&host, &Caller::Local, "request_check", &outside),
            Err(ApiError::BadInput { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn the_timeline_lists_what_happened_and_its_quarters() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-timeline");
        let ask =
            format!(r#"{{"job":"{job}","from_ms":0,"to_ms":9007199254740991,"bucket":"M15"}}"#);
        // The job loop starts the run after the batch that requested it, so a read in that
        // batch sees only the request: wait for the start.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let page = loop {
            let page = call(&host, &Caller::Local, "timeline", &ask).unwrap();
            let started = argus_json::write(&page).contains("attempt 0 started");
            if started || std::time::Instant::now() >= deadline {
                break page;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let Json::Arr(items) = get(&page, "items") else {
            panic!()
        };
        let titles: Vec<&str> = items.iter().map(|i| text(i, &["title"])).collect();
        assert!(titles.contains(&"requested agent"), "{titles:?}");
        assert!(titles.contains(&"attempt 0 started"), "{titles:?}");
        let lanes: Vec<&str> = items.iter().map(|i| text(i, &["lane", "kind"])).collect();
        assert!(
            lanes.iter().all(|l| *l == "Agent" || *l == "Host"),
            "{lanes:?}"
        );
        assert!(
            items
                .iter()
                .all(|i| argus_core::ids::FactId::parse(text(i, &["fact"])).is_some())
        );
        let Json::Arr(buckets) = get(&page, "buckets") else {
            panic!()
        };
        assert!(!buckets.is_empty());
        assert_eq!(text(&buckets[0], &["width"]), "M15");

        let no_buckets = format!(r#"{{"job":"{job}","from_ms":0,"to_ms":1,"bucket":null}}"#);
        let empty = call(&host, &Caller::Local, "timeline", &no_buckets).unwrap();
        assert_eq!(get(&empty, "items"), &Json::Arr(vec![]));
        assert_eq!(get(&empty, "buckets"), &Json::Arr(vec![]));
        host.shutdown();
    }

    /// Records `lines` as a plain-text source of the job's agent run; returns its id.
    fn feed_source(host: &Host, job: &str, lines: &[&str]) -> String {
        use argus_core::ids::{Attempt, Id128, JobId, RunId};
        use argus_core::source::{RawFrame, SourceKind, SourceMeta, SourceOrigin};
        let id = JobId(Id128::parse(job).unwrap());
        let handle = host.inner.job(id).unwrap();
        let run = RunId(Id128::derive("agent", &id.0.0.to_le_bytes()));
        let origin = SourceOrigin::Stdin;
        let meta = SourceMeta {
            id: SourceMeta::derive_id(run, Attempt(0), &origin),
            run,
            attempt: Attempt(0),
            kind: SourceKind::PlainText,
            origin,
        };
        let source = meta.id;
        crate::ledger_thread::open_source(&handle.sender(), meta).unwrap();
        let mut offset = 0;
        let frames = lines
            .iter()
            .map(|l| {
                let frame = RawFrame::Bytes {
                    offset,
                    bytes: l.as_bytes().to_vec(),
                };
                offset += l.len() as u64;
                frame
            })
            .collect();
        crate::ledger_thread::observe(&handle.sender(), source, frames);
        source.0.to_string()
    }

    fn records(page: &Json) -> Vec<&str> {
        let Json::Arr(records) = get(page, "records") else {
            panic!()
        };
        records.iter().map(|r| text(r, &["text"])).collect()
    }

    #[test]
    fn the_raw_log_pages_a_source_and_shows_an_authored_event_as_stored() {
        let (host, _root, repo) = host();
        let job = start(&host, &repo, "req-raw");
        let source = feed_source(
            &host,
            &job,
            &["line 0\n", "line 1\n", "line 2\n", "line 3\n", "line 4\n"],
        );
        let page = |around: Option<&str>, source: Option<&str>| {
            let quote = |v: Option<&str>| v.map_or("null".to_owned(), |s| format!("\"{s}\""));
            let ask = format!(
                r#"{{"job":"{job}","source":{},"around_fact":{},"limit":2}}"#,
                quote(source),
                quote(around)
            );
            call(&host, &Caller::Local, "raw", &ask)
        };
        // The source is fed through the loop; wait until all of it is stored.
        for _ in 0..200 {
            if records(&page(None, Some(&source)).unwrap()).len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let first = page(None, Some(&source)).unwrap();
        assert_eq!(records(&first), ["line 0\n", "line 1\n"]);
        let Json::Arr(rs) = get(&first, "records") else {
            panic!()
        };
        assert_eq!(text(&rs[0], &["format"]), "Text");
        assert_eq!(text(&rs[0], &["source"]), source);
        assert_eq!(get(&rs[0], "is_utf8"), &Json::Bool(true));
        assert_eq!(get(&rs[0], "repeat"), &Json::Bool(false));
        let next = text(&first, &["next"]).to_owned();
        let second = page(Some(&next), Some(&source)).unwrap();
        assert_eq!(records(&second), ["line 2\n", "line 3\n"]);
        // A fact of a source names its source.
        assert_eq!(
            records(&page(Some(&next), None).unwrap()),
            ["line 2\n", "line 3\n"]
        );
        let last = page(Some(text(&second, &["next"])), Some(&source)).unwrap();
        assert_eq!(records(&last), ["line 4\n"]);
        assert_eq!(get(&last, "next"), &Json::Null);

        let ask =
            format!(r#"{{"job":"{job}","from_ms":0,"to_ms":9007199254740991,"bucket":null}}"#);
        let timeline = call(&host, &Caller::Local, "timeline", &ask).unwrap();
        let Json::Arr(items) = get(&timeline, "items") else {
            panic!()
        };
        let opened = items
            .iter()
            .find(|i| text(i, &["lane", "kind"]) == "Host")
            .unwrap();
        let fact = text(opened, &["fact"]);
        let authored = page(Some(fact), None).unwrap();
        let Json::Arr(rs) = get(&authored, "records") else {
            panic!()
        };
        assert_eq!(rs.len(), 1);
        assert_eq!(text(&rs[0], &["format"]), "Event");
        assert_eq!(get(&rs[0], "source"), &Json::Null);
        assert_eq!(text(&rs[0], &["fact"]), fact);
        assert!(text(&rs[0], &["text"]).contains("fix the gate"));
        assert!(matches!(get(&rs[0], "reading"), Json::Obj(_)));
        assert_eq!(get(&authored, "next"), &Json::Null);

        assert!(matches!(page(None, None), Err(ApiError::BadInput { .. })));
        let unknown = Some("0f8fad5d-d9cb-469f-a165-70867728950e");
        assert!(matches!(
            page(None, unknown),
            Err(ApiError::NotFound { .. })
        ));
        host.shutdown();
    }

    #[test]
    fn the_review_screen_shows_the_head_hunks_their_threads_and_whether_it_can_merge() {
        let (host, root, repo) = host();
        let job = start(&host, &repo, "req-review");
        let ask = format!(r#"{{"job":"{job}"}}"#);
        let empty = call(&host, &Caller::Local, "review", &ask).unwrap();
        assert_eq!(get(&empty, "head"), &Json::Null);
        assert_eq!(get(&empty, "headline"), &Json::Null);
        assert_eq!(get(&empty, "agenda"), &Json::Arr(vec![]));
        assert_eq!(get(&empty, "can_merge"), &Json::Bool(false));

        let tree = head_after_edit(&host, &root, &job);
        let open = format!(
            r#"{{"request_id":"r-1","job":"{job}","anchor":{{"kind":"Code","tree":"{tree}","path":"a.txt","start":2,"end":2}},"body":"why two?"}}"#
        );
        call(&host, &Caller::Local, "open_thread", &open).unwrap();
        let screen = call(&host, &Caller::Local, "review", &ask).unwrap();
        assert_eq!(text(&screen, &["job"]), job);
        assert_eq!(text(&screen, &["base"]), git(&repo, &["rev-parse", "main"]));
        assert_eq!(text(&screen, &["head", "tree"]), tree);
        let num = |n: u64| Json::Num(argus_json::Number::from_u64(n));
        let headline = get(&screen, "headline");
        for (key, n) in [
            ("round", 1),
            ("files", 1),
            ("added", 1),
            ("removed", 0),
            ("unexplained", 1),
        ] {
            assert_eq!(get(headline, key), &num(n), "{key}");
        }
        assert_eq!(get(headline, "withheld"), &Json::Arr(vec![]));
        let Json::Arr(agenda) = get(&screen, "agenda") else {
            panic!()
        };
        assert_eq!(agenda.len(), 1);
        let item = &agenda[0];
        assert_eq!(text(item, &["hunk", "path"]), "a.txt");
        assert!(text(item, &["hunk", "text"]).contains("+two"));
        assert_eq!(get(get(item, "hunk"), "new_start"), &num(1));
        assert_eq!(get(get(item, "hunk"), "new_len"), &num(2));
        assert_eq!(text(item, &["provenance", "kind"]), "Unexplained");
        assert_eq!(text(item, &["tier"]), "Suspicious");
        let Json::Arr(threads) = get(item, "threads") else {
            panic!()
        };
        assert_eq!(threads.len(), 1);
        assert_eq!(text(&threads[0], &["at", "path"]), "a.txt");
        assert_eq!(get(get(&threads[0], "at"), "start"), &num(2));
        let Json::Arr(comments) = get(&threads[0], "comments") else {
            panic!()
        };
        assert_eq!(text(&comments[0], &["body"]), "why two?");
        assert_eq!(get(&comments[0], "draft"), &Json::Bool(true));
        assert_eq!(get(&screen, "drafts"), &num(1));
        assert_eq!(get(&screen, "threads"), &Json::Arr(vec![]));
        assert_eq!(get(&screen, "reviews"), &Json::Arr(vec![]));
        assert_eq!(get(&screen, "checks"), &Json::Arr(vec![]));
        assert_eq!(get(&screen, "can_merge"), &Json::Bool(false));

        let approve = format!(
            r#"{{"request_id":"r-2","job":"{job}","tree":"{tree}","verdict":"Approve","note":""}}"#
        );
        call(&host, &Caller::Local, "decide", &approve).unwrap();
        let approved = call(&host, &Caller::Local, "review", &ask).unwrap();
        assert_eq!(get(&approved, "can_merge"), &Json::Bool(true));
        assert_eq!(text(&approved, &["state", "kind"]), "Approved");
        host.shutdown();
    }
}
