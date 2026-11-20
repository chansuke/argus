//! The only `argus_api::Handler`: maps view models to core events (validation at the
//! boundary: strings -> ids, paths, trees; wire enums -> core enums) and core views to
//! view models. Business rules stay in the core; this is translation plus routing to
//! ledger threads.

use argus_api::vm::*;
use argus_api::{ApiError, Caller, Handler};
use std::path::PathBuf;

use argus_core::event::{
    AgentLaunch, Author, Decision, Event, JobInfo, JobOrigin, Rejected, ReviewLens, RunPurpose,
};
use argus_core::ids::{Id128, InputId, JobId, ModelId, RunId, UnixMillis};
use argus_core::ledger::{JobLedger, LedgerError, Submitted};
use argus_core::models::AgentCli;
use argus_core::views::{GateState, glance, progress};

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
}
