//! A job ledger with the real views: what the sequencer admits about decisions and merges.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_core::event::{
    Author, Decision, Event, JobInfo, JobOrigin, Rejected, RunOutcome, RunPurpose, Snapshot,
};
use argus_core::ids::{Attempt, DecisionId, GitOid, Id128, JobId, RunId, TreeOid};
use argus_core::views::JobViews;
use argus_core::{Ledger, LedgerError, Submitted};

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "argus-job-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn oid(c: char) -> GitOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn run(n: u128) -> RunId {
    RunId(Id128(n))
}

struct Job {
    ledger: Ledger<JobViews>,
}

impl Job {
    fn open() -> Self {
        let mut job = Job {
            ledger: Ledger::open(&scratch()).unwrap(),
        };
        let info = JobInfo {
            job: JobId(Id128(1)),
            title: "fix the gate".into(),
            repo: PathBuf::from("/w/argus"),
            base: oid('b'),
            target: "main".into(),
            worktree: PathBuf::from("/w/jobs/1"),
            group: None,
            origin: JobOrigin::Launched,
        };
        job.ok(Author::Host, Event::JobOpened(info));
        job
    }

    fn submit(&mut self, author: Author, event: Event) -> Result<Submitted, LedgerError> {
        self.ledger.submit(author, event)
    }

    fn ok(&mut self, author: Author, event: Event) {
        let done = self.submit(author, event).unwrap();
        assert!(matches!(done, Submitted::Appended(_)), "{done:?}");
    }

    fn snapshot(&mut self, n: u128, tree: TreeOid) {
        let requested = Event::RunRequested {
            run: run(n),
            purpose: RunPurpose::Snapshot { after: None },
        };
        self.ok(Author::Policy, requested);
        let started = Event::RunStarted {
            run: run(n),
            attempt: Attempt(0),
            pid: None,
        };
        self.ok(Author::Host, started);
        let snapshot = Snapshot {
            tree,
            base: oid('b'),
            previous: None,
            delta: Vec::new(),
            files: Vec::new(),
            withheld: Vec::new(),
        };
        let finished = Event::RunFinished {
            run: run(n),
            attempt: Attempt(0),
            outcome: RunOutcome::Snapshot(snapshot),
        };
        self.ok(Author::Host, finished);
    }
}

fn decided(d: u128, tree: Option<TreeOid>, verdict: Decision) -> Event {
    Event::Decided {
        decision: DecisionId(Id128(d)),
        tree,
        verdict,
        note: String::new(),
    }
}

#[test]
fn a_verdict_is_admitted_only_on_the_current_head() {
    let mut job = Job::open();
    // Nothing to judge yet; abandoning needs no tree.
    let early = job.submit(Author::Human, decided(1, Some(oid('1')), Decision::Approve));
    assert!(matches!(
        early,
        Err(LedgerError::Rejected(Rejected::NotHead { head: None }))
    ));
    job.snapshot(10, oid('1'));
    job.snapshot(11, oid('2'));
    let stale = job.submit(Author::Human, decided(2, Some(oid('1')), Decision::Approve));
    assert!(matches!(
        stale,
        Err(LedgerError::Rejected(Rejected::NotHead { head: Some(h) })) if h == oid('2')
    ));
    let no_tree = job.submit(Author::Human, decided(3, None, Decision::Approve));
    assert!(matches!(
        no_tree,
        Err(LedgerError::Rejected(Rejected::NotHead { .. }))
    ));
    job.ok(Author::Human, decided(4, Some(oid('2')), Decision::Approve));
    assert_eq!(job.ledger.views().review().decisions().len(), 1);
}

#[test]
fn a_merge_needs_an_approval_that_is_still_current() {
    let mut job = Job::open();
    job.snapshot(10, oid('1'));
    job.ok(Author::Human, decided(1, Some(oid('1')), Decision::Approve));
    let token = job.ledger.views().review().approved_head().unwrap();

    // A new head arrives between reading the approval and asking for the merge.
    job.snapshot(11, oid('2'));
    let merge = |n: u128, token| Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Merge(token),
    };
    let late = job.submit(Author::Human, merge(20, token.clone()));
    assert!(matches!(
        late,
        Err(LedgerError::Rejected(Rejected::NotApproved))
    ));

    job.ok(Author::Human, decided(2, Some(oid('2')), Decision::Approve));
    let current = job.ledger.views().review().approved_head().unwrap();
    assert_eq!(current.tree(), &oid('2'));
    job.ok(Author::Human, merge(21, current));
    assert!(job.ledger.views().runs().get(&run(21)).is_some());
}

#[test]
fn the_admission_check_survives_a_restart() {
    let dir = scratch();
    let mut job = Job {
        ledger: Ledger::open(&dir).unwrap(),
    };
    job.snapshot(10, oid('1'));
    job.snapshot(11, oid('2'));
    drop(job);
    let mut ledger = Ledger::<JobViews>::open(&dir).unwrap();
    let stale = ledger.submit(Author::Human, decided(1, Some(oid('1')), Decision::Approve));
    assert!(matches!(
        stale,
        Err(LedgerError::Rejected(Rejected::NotHead { .. }))
    ));
    let fresh = ledger.submit(Author::Human, decided(2, Some(oid('2')), Decision::Approve));
    assert!(matches!(fresh, Ok(Submitted::Appended(_))), "{fresh:?}");
}

// ---------------------------------------------------------------- glance

use argus_core::event::{AgentLaunch, TaskPatch, TaskState};
use argus_core::ids::{TaskKey, UnixMillis};
use argus_core::models::AgentCli;
use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};
use argus_core::views::GateState;
use argus_core::views::glance::{Attention, JobState, STALL_AFTER_MS};
use argus_core::views::progress::{Activity, WaitReason};

/// 2026-10-24T03:16:30Z, the time of the first transcript record below.
const T: i64 = 1_792_811_790_000;

fn stamp_of(ms: i64) -> String {
    // Seconds after `T`, as the transcript's RFC 3339 time.
    let s = (ms - T) / 1000;
    format!(
        "2026-10-24T03:{:02}:{:02}.000Z",
        16 + (30 + s) / 60,
        (30 + s) % 60
    )
}

impl Job {
    fn agent(&mut self, n: u128) {
        let requested = Event::RunRequested {
            run: run(n),
            purpose: RunPurpose::Agent(AgentLaunch {
                cli: AgentCli::Claude,
                model: None,
                prompt: "fix the gate".into(),
                resume: None,
            }),
        };
        self.ok(Author::Human, requested);
        self.start(n);
    }

    fn start(&mut self, n: u128) {
        let started = Event::RunStarted {
            run: run(n),
            attempt: Attempt(0),
            pid: None,
        };
        self.ok(Author::Host, started);
    }

    fn finish(&mut self, n: u128, outcome: RunOutcome) {
        let finished = Event::RunFinished {
            run: run(n),
            attempt: Attempt(0),
            outcome,
        };
        self.ok(Author::Host, finished);
    }

    fn feed(&mut self, n: u128, kind: SourceKind, origin: SourceOrigin, lines: &[String]) {
        let meta = SourceMeta {
            id: SourceMeta::derive_id(run(n), Attempt(0), &origin),
            run: run(n),
            attempt: Attempt(0),
            kind,
            origin,
        };
        let cursor = self.ledger.open_source(meta.clone()).unwrap();
        let mut framer = kind.framer(cursor.resume_at);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let frames = framer.push(text.as_bytes());
        self.ledger.observe(meta.id, frames).unwrap();
    }

    /// Transcript records of agent run `n`.
    fn transcript(&mut self, n: u128, lines: &[String]) {
        let origin = SourceOrigin::File(PathBuf::from("/h/.claude/projects/w/s.jsonl"));
        self.feed(n, SourceKind::ClaudeTranscript, origin, lines);
    }

    fn glance(&self, now: i64) -> argus_core::views::glance::Glance {
        self.ledger.views().glance(UnixMillis(now))
    }
}

fn tool_call(uuid: &str, at: i64) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"{}","message":{{"id":"m-{uuid}","content":[{{"type":"tool_use","id":"t-{uuid}","name":"Bash","input":{{"command":"tools/gate"}}}}],"stop_reason":"tool_use"}}}}"#,
        stamp_of(at)
    )
}

fn turn_end(uuid: &str, at: i64) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"{}","message":{{"id":"m-{uuid}","content":[{{"type":"text","text":"done"}}],"stop_reason":"end_turn"}}}}"#,
        stamp_of(at)
    )
}

#[test]
fn a_working_agent_is_shown_and_flagged_only_once_it_stalls() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[tool_call("a1", T)]);
    let g = job.glance(T + 60_000);
    assert_eq!(g.state, JobState::Working);
    assert_eq!(g.share, None);
    assert_eq!(g.review, GateState::NotConfigured);
    assert!(
        matches!(g.now.as_slice(), [(r, Activity::Tool { since, .. })] if *r == run(1) && *since == UnixMillis(T))
    );
    assert!(g.attention.is_empty(), "{:?}", g.attention);

    let g = job.glance(T + STALL_AFTER_MS + 1);
    assert_eq!(
        g.attention,
        vec![Attention::Stalled {
            since: UnixMillis(T),
            run: run(1)
        }]
    );
}

#[test]
fn a_permission_prompt_is_the_most_urgent_thing() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[tool_call("a1", T)]);
    job.snapshot(10, oid('1'));
    let hook = r#"{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#;
    job.feed(
        1,
        SourceKind::ClaudeHook,
        SourceOrigin::Hook,
        &[hook.to_owned()],
    );
    let g = job.glance(T + STALL_AFTER_MS * 100);
    assert!(matches!(
        &g.now[0].1,
        Activity::AwaitingHuman {
            why: WaitReason::Permission { .. },
            ..
        }
    ));
    assert!(matches!(g.attention[0], Attention::AwaitingPermission { run: r, .. } if r == run(1)));
    // Waiting on the human is never "stalled"; the undecided head is listed after.
    assert!(matches!(g.attention[1], Attention::AwaitingDecision { .. }));
    assert_eq!(g.attention.len(), 2, "{:?}", g.attention);
}

#[test]
fn an_agent_at_its_prompt_waits_unless_a_head_needs_a_decision() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[turn_end("a1", T)]);
    let g = job.glance(T + 1000);
    assert_eq!(
        g.attention,
        vec![Attention::AgentWaiting {
            since: UnixMillis(T),
            run: run(1)
        }]
    );
    job.snapshot(10, oid('1'));
    let g = job.glance(T + 1000);
    assert_eq!(g.state, JobState::AwaitingDecision);
    let head_at = job.ledger.views().review().head().unwrap().at;
    assert_eq!(
        g.attention,
        vec![Attention::AwaitingDecision { since: head_at }]
    );
}

#[test]
fn the_job_state_follows_decisions_delivery_and_merge() {
    let mut job = Job::open();
    job.agent(1);
    job.snapshot(10, oid('1'));
    let decision = DecisionId(Id128(1));
    job.ok(
        Author::Human,
        decided(1, Some(oid('1')), Decision::RequestChanges),
    );
    assert_eq!(
        job.glance(T).state,
        JobState::ChangesRequested { delivered: false }
    );

    let delivery = decision.delivery_run();
    let requested = Event::RunRequested {
        run: delivery,
        purpose: RunPurpose::Agent(AgentLaunch {
            cli: AgentCli::Claude,
            model: None,
            prompt: "feedback".into(),
            resume: Some(run(1)),
        }),
    };
    job.ok(Author::Policy, requested);
    let started = Event::RunStarted {
        run: delivery,
        attempt: Attempt(0),
        pid: None,
    };
    job.ok(Author::Host, started);
    assert_eq!(
        job.glance(T).state,
        JobState::ChangesRequested { delivered: true }
    );

    job.snapshot(11, oid('2'));
    assert_eq!(job.glance(T).state, JobState::AwaitingDecision);
    job.ok(Author::Human, decided(2, Some(oid('2')), Decision::Approve));
    assert_eq!(job.glance(T).state, JobState::Approved);
    let token = job.ledger.views().review().approved_head().unwrap();
    let merge = Event::RunRequested {
        run: run(30),
        purpose: RunPurpose::Merge(token),
    };
    job.ok(Author::Human, merge);
    assert_eq!(job.glance(T).state, JobState::Merging);
    job.start(30);
    job.finish(30, RunOutcome::Merged { commit: oid('c') });
    assert_eq!(job.glance(T).state, JobState::Merged);
}

#[test]
fn a_delivery_that_never_reached_the_agent_is_flagged() {
    let mut job = Job::open();
    job.agent(1);
    job.snapshot(10, oid('1'));
    job.ok(
        Author::Human,
        decided(1, Some(oid('1')), Decision::RequestChanges),
    );
    let delivery = DecisionId(Id128(1)).delivery_run();
    let requested = Event::RunRequested {
        run: delivery,
        purpose: RunPurpose::Agent(AgentLaunch {
            cli: AgentCli::Claude,
            model: None,
            prompt: "feedback".into(),
            resume: Some(run(1)),
        }),
    };
    job.ok(Author::Policy, requested);
    let failed = Event::RunFinished {
        run: delivery,
        attempt: Attempt(0),
        outcome: RunOutcome::SpawnFailed {
            error: "claude: not found".into(),
        },
    };
    job.ok(
        Author::Host,
        Event::RunStarted {
            run: delivery,
            attempt: Attempt(0),
            pid: None,
        },
    );
    job.ok(Author::Host, failed);
    let g = job.glance(T);
    assert!(
        matches!(g.attention.as_slice(), [Attention::DeliveryFailed { decision, .. }] if *decision == DecisionId(Id128(1))),
        "{:?}",
        g.attention
    );
}

#[test]
fn an_abandoned_job_is_abandoned_and_a_failed_agent_is_flagged() {
    let mut job = Job::open();
    job.agent(1);
    job.finish(1, RunOutcome::Exited { code: 2 });
    let g = job.glance(T);
    assert!(
        matches!(g.attention.as_slice(), [Attention::RunFailed { run: r, .. }] if *r == run(1))
    );
    // A failed earlier run is not flagged once a newer agent run exists.
    job.agent(2);
    assert!(
        job.glance(T)
            .attention
            .iter()
            .all(|a| !matches!(a, Attention::RunFailed { .. }))
    );

    job.snapshot(10, oid('1'));
    job.ok(Author::Human, decided(1, Some(oid('1')), Decision::Abandon));
    assert_eq!(job.glance(T).state, JobState::Abandoned);
}

#[test]
fn blocked_tasks_and_open_leases_are_listed() {
    use argus_core::event::{DevicePlatform, LeaseProvider};
    use argus_core::ids::LeaseId;
    let mut job = Job::open();
    let blocked = Event::TaskSet {
        task: TaskKey(Id128(5)),
        patch: TaskPatch {
            state: Some(TaskState::Blocked {
                reason: "needs an EAS token".into(),
            }),
            ..TaskPatch::default()
        },
    };
    job.ok(Author::Human, blocked);
    job.agent(1);
    let lease = Event::LeaseOpened {
        lease: LeaseId(Id128(9)),
        run: run(1),
        attempt: Attempt(0),
        provider: LeaseProvider::EasSimulator {
            platform: DevicePlatform::Ios,
        },
        name: "argus-1".into(),
    };
    job.ok(Author::Host, lease);
    let g = job.glance(T);
    assert_eq!(
        g.blocked,
        vec![(TaskKey(Id128(5)), "needs an EAS token".to_owned())]
    );
    assert_eq!(g.open_leases, 1);
    assert_eq!(g.share.map(|s| s.get()), Some(0.0));
}
