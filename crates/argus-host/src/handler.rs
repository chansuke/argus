//! The only `argus_api::Handler`: maps view models to core events (validation at the
//! boundary: strings -> ids, paths, trees; wire enums -> core enums) and core views to
//! view models. Business rules stay in the core; this is translation plus routing to
//! ledger threads.

use argus_api::vm::*;
use argus_api::{ApiError, Caller, Handler};
use argus_core::event::{Decision, ReviewLens};
use argus_core::models::AgentCli;

use crate::HostInner;
use crate::pulse::Dep;

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
    todo!("not implemented")
}

// Wire <-> core for closed sets. Exhaustive in both directions, so a variant added on
// either side fails to compile until mapped (the two lists cannot drift).
fn agent_cli(a: Agent) -> AgentCli {
    match a {
        Agent::Claude => AgentCli::Claude,
        Agent::Codex => AgentCli::Codex,
    }
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
        // TODO: deps.push(JobSet); per job: read(|l| l.views().glance(now)) -> JobCard;
        //       queue = views::glance::queue(..) -> QueueEntry.
        todo!("not implemented")
    }
    fn job(&mut self, caller: &Caller, input: JobRef) -> Result<JobPulse, ApiError> {
        todo!("not implemented")
    }
    fn timeline(
        &mut self,
        caller: &Caller,
        input: TimelineQuery,
    ) -> Result<TimelinePage, ApiError> {
        todo!("not implemented")
    }
    fn raw(&mut self, caller: &Caller, input: RawQuery) -> Result<RawPage, ApiError> {
        // TODO: ledger.raw_page / ledger.raw around a fact (`FactId::parse`); bytes as text
        //       when UTF-8; format from the source family; repeat from `Reading`.
        todo!("not implemented")
    }
    fn review(&mut self, caller: &Caller, input: JobRef) -> Result<ReviewScreen, ApiError> {
        // TODO: two short reads with git between them, on this API thread, so the job's
        //       sequencer is never held while shelling out:
        //   1. req = read(|l| l.views().review().diff_request())   (None -> empty screen)
        //   2. diff = host.diffs.get(repo, &req)                     (memoized, immutable)
        //   3. read(|l| l.views().review_packet(&diff, &cfg).map(to_screen));
        //      Err(HeadMoved) -> back to 1 (a newer snapshot landed in between).
        todo!("not implemented")
    }
    fn presets(&mut self, caller: &Caller, input: Empty) -> Result<Presets, ApiError> {
        todo!("not implemented")
    }
    fn request_review(
        &mut self,
        caller: &Caller,
        input: RequestReview,
    ) -> Result<RunRefs, ApiError> {
        // TODO: models = presets.resolve(reviewers)?; head = read(review().head());
        //       for (run, purpose) in ReviewSpec::panel(uuid(request_id), head, base,
        //       lens(input.lens), focus, &models): submit(author(caller)?, RunRequested).
        //       Duplicates are success (same runs returned).
        todo!("not implemented")
    }
    fn request_check(&mut self, caller: &Caller, input: RequestCheck) -> Result<RunRef, ApiError> {
        todo!("not implemented")
    }
    fn open_thread(&mut self, caller: &Caller, input: OpenThread) -> Result<Ack, ApiError> {
        // TODO: AnchorInput -> Anchor (GitOid::parse, RepoPath::parse, LineRange::new).
        todo!("not implemented")
    }
    fn reply(&mut self, caller: &Caller, input: Reply) -> Result<Ack, ApiError> {
        todo!("not implemented")
    }
    fn resolve(&mut self, caller: &Caller, input: Resolve) -> Result<Ack, ApiError> {
        todo!("not implemented")
    }
    fn promote_finding(&mut self, caller: &Caller, input: PromoteFinding) -> Result<Ack, ApiError> {
        // TODO: submit(Human, FindingPromoted { finding, thread: derive("promote", request_id),
        //       comment: uuid(request_id) }).
        todo!("not implemented")
    }
    fn dismiss_finding(&mut self, caller: &Caller, input: DismissFinding) -> Result<Ack, ApiError> {
        todo!("not implemented")
    }
    fn preview_feedback(
        &mut self,
        caller: &Caller,
        input: PreviewFeedback,
    ) -> Result<FeedbackText, ApiError> {
        // TODO: read(|l| l.views().preview_feedback(&note)): the same `render_feedback`
        //       the policy uses for delivery.
        todo!("not implemented")
    }
    fn decide(&mut self, caller: &Caller, input: Decide) -> Result<Ack, ApiError> {
        // TODO: submit(author, Decided { decision: DecisionId(uuid(request_id)),
        //       tree: input.tree.map(GitOid::parse), verdict: decision(input.verdict), note }).
        //       Rejected::NotHead { head } -> ApiError::StaleHead { head }.
        todo!("not implemented")
    }
    fn merge(&mut self, caller: &Caller, input: MergeJob) -> Result<RunRef, ApiError> {
        // TODO: token = read(|l| l.views().review().approved_head()); none, or
        //       token.tree() != input.tree -> StaleHead; submit(Human, RunRequested {
        //       run: derive("merge", tree), purpose: Merge(token) }). The ledger re-checks
        //       on the sequencer (NotApproved -> StaleHead).
        todo!("not implemented")
    }
    fn start_jobs(&mut self, caller: &Caller, input: StartJobs) -> Result<Started, ApiError> {
        // TODO: per agent choice (fan-out shares a group id derived from request_id):
        //       git::create_worktree; store.open_job; submit(Host, JobOpened);
        //       submit(Human, RunRequested{ Agent(AgentLaunch{ cli: agent_cli(..), .. }) }).
        todo!("not implemented")
    }
    fn send_input(&mut self, caller: &Caller, input: SendInput) -> Result<Ack, ApiError> {
        todo!("not implemented")
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
        todo!("not implemented")
    }
    fn set_task(&mut self, caller: &Caller, input: SetTask) -> Result<TaskSaved, ApiError> {
        // TODO: agent callers may only touch tasks they created or create new ones under
        //       any parent; submit(author, TaskSet{ task, patch }).
        todo!("not implemented")
    }
    fn hook(&mut self, caller: &Caller, input: HookPayload) -> Result<Ack, ApiError> {
        // TODO: agent only; open_source(SourceMeta{ run, kind: ClaudeHook, origin: Hook })
        //       and observe one RawFrame::Bytes with the payload verbatim.
        todo!("not implemented")
    }
    fn bookmark(&mut self, caller: &Caller, input: Bookmark) -> Result<Ack, ApiError> {
        todo!("not implemented")
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
