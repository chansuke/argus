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
        let overview = call(&host, &Caller::Local, "overview", "{}").unwrap();
        let run = {
            let Some(Json::Arr(cards)) = overview.get("jobs") else {
                panic!()
            };
            let Some(Json::Arr(agents)) = cards[0].get("agents") else {
                panic!()
            };
            text(&agents[0], &["run"]).to_owned()
        };
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
}
