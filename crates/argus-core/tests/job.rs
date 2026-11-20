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
        // Each call is a whole stream: what the framer holds back (Lean's last message) is
        // stored too.
        let mut frames = framer.push(text.as_bytes());
        frames.extend(framer.finish());
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

#[test]
fn failing_checks_on_the_head_need_attention() {
    use argus_core::event::{CheckRequest, CheckSpec, LeanMode};
    use argus_core::ids::RepoPath;
    let mut job = Job::open();
    job.snapshot(10, oid('1'));
    let check = |n: u128, job: &mut Job, out: &str, code: i32| {
        let request = CheckRequest {
            tree: oid('1'),
            base: oid('b'),
            spec: CheckSpec::Lean {
                root: RepoPath::parse(&format!("proofs{n}")).unwrap(),
                mode: LeanMode::LakeBuild,
            },
        };
        job.ok(
            Author::Policy,
            Event::RunRequested {
                run: run(n),
                purpose: RunPurpose::Check(request),
            },
        );
        job.start(n);
        let lines: Vec<String> = out.lines().map(str::to_owned).collect();
        job.feed(n, SourceKind::LeanText, SourceOrigin::Stdout, &lines);
        job.finish(n, RunOutcome::Exited { code });
    };
    let fixture = |name| {
        std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lean/").to_owned() + name,
        )
        .unwrap()
    };
    check(20, &mut job, &fixture("sorry.txt"), 0);
    check(21, &mut job, "error: no such file or directory", 1);
    let g = job.glance(T);
    assert_eq!(g.checks, GateState::Red { open: 2 });
    let kinds: Vec<_> = g
        .attention
        .iter()
        .filter(|a| {
            matches!(
                a,
                Attention::Counterexample { .. } | Attention::ChecksFailing { .. }
            )
        })
        .collect();
    assert!(
        matches!(
            kinds.as_slice(),
            [
                Attention::Counterexample { .. },
                Attention::ChecksFailing { .. }
            ]
        ),
        "{:?}",
        g.attention
    );
}

// ---------------------------------------------------------------- review packet

use argus_core::diff::{DiffRequest, HeadDiff, parse_unified};
use argus_core::event::{Anchor, FileState, LlmCall, ReviewLens, ReviewSpec};
use argus_core::ids::{CommentId, LineRange, RepoPath, ThreadId};
use argus_core::models::ModelChoice;
use argus_core::views::packet::{
    Applicability, PacketConfig, Provenance, SENSITIVE_DEFAULT, Signal, Tier,
};

fn repo(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

fn head_diff(tree: char) -> HeadDiff {
    let text = include_str!("fixtures/git/head.diff");
    HeadDiff {
        request: DiffRequest {
            base: oid('b'),
            tree: oid(tree),
        },
        hunks: parse_unified(text),
    }
}

/// The head's files as `Snapshot::files` records them: blob ids and the hunk anchors of
/// the fixture diff.
fn files(blob_of_a: char) -> Vec<FileState> {
    let diff = head_diff('1');
    let blob = |path: &str| match path {
        "src/a.rs" => Some(oid(blob_of_a)),
        "tools/old.sh" => None,
        _ => Some(oid('f')),
    };
    let mut out: Vec<FileState> = Vec::new();
    for h in &diff.hunks {
        let path = h.anchor.path.as_str();
        match out.iter_mut().find(|f| f.path.as_str() == path) {
            Some(f) => f.hunks.push(h.anchor.clone()),
            None => out.push(FileState {
                path: repo(path),
                blob: blob(path),
                hunks: vec![h.anchor.clone()],
            }),
        }
    }
    out
}

fn model(name: &str) -> ModelChoice {
    ModelChoice {
        cli: AgentCli::Claude,
        model: argus_core::ids::ModelId(name.into()),
        effort: None,
    }
}

impl Job {
    fn snapshot_of(
        &mut self,
        n: u128,
        tree: TreeOid,
        previous: Option<TreeOid>,
        files: Vec<FileState>,
    ) {
        let requested = Event::RunRequested {
            run: run(n),
            purpose: RunPurpose::Snapshot { after: None },
        };
        self.ok(Author::Policy, requested);
        self.start(n);
        let snapshot = Snapshot {
            tree,
            base: oid('b'),
            previous,
            delta: Vec::new(),
            files,
            withheld: vec![repo(".env")],
        };
        self.finish(n, RunOutcome::Snapshot(snapshot));
    }

    /// A review of `tree` by `model` that answers with `findings` (the JSON array).
    fn review(&mut self, n: u128, tree: char, model_name: &str, findings: &str) {
        let spec = ReviewSpec {
            tree: oid(tree),
            base: oid('b'),
            lens: ReviewLens::General,
            llm: LlmCall {
                model: model(model_name),
                prompt: "review".into(),
            },
        };
        self.ok(
            Author::Human,
            Event::RunRequested {
                run: run(n),
                purpose: RunPurpose::Review(spec),
            },
        );
        self.start(n);
        let text = format!("Notes.\n\n```json\n{findings}\n```\n");
        let mut line = String::from(r#"{"type":"result","is_error":false,"result":"#);
        line.push_str(&json_string(&text));
        line.push('}');
        self.feed(
            n,
            SourceKind::ClaudeStreamJson,
            SourceOrigin::Stdout,
            &[line],
        );
        self.finish(n, RunOutcome::Exited { code: 0 });
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn edit_call(uuid: &str, path: &str, new_string: &str) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"{}","message":{{"id":"m-{uuid}","content":[{{"type":"tool_use","id":"t-{uuid}","name":"Edit","input":{{"file_path":"{path}","old_string":"x","new_string":"{new_string}"}}}}],"stop_reason":"tool_use"}}}}"#,
        stamp_of(T)
    )
}

const FINDINGS_1: &str = r#"[
  {"path":"src/a.rs","start":1,"end":3,"severity":"high","title":"renamed a line","body":""},
  {"path":"src/new.rs","start":1,"end":1,"severity":"low","title":"empty main","body":""},
  {"path":null,"start":null,"end":null,"severity":"medium","title":"no tests","body":""}
]"#;
const FINDINGS_2: &str = r#"[{"path":"src/a.rs","start":2,"end":2,"severity":"high","title":"line2 is wrong","body":""}]"#;

fn cfg() -> PacketConfig {
    PacketConfig {
        sensitive: SENSITIVE_DEFAULT
            .iter()
            .map(|s| s.to_string())
            .chain(["*.txt".to_owned()])
            .collect(),
    }
}

#[test]
fn the_first_round_puts_agreed_findings_first_and_explains_hunks_from_the_transcript() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[edit_call("a1", "/w/jobs/1/src/a.rs", "line2 changed")]);
    job.snapshot_of(10, oid('1'), None, files('a'));
    job.review(5, '1', "opus", FINDINGS_1);
    job.review(6, '1', "sonnet", FINDINGS_2);

    let diff = head_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    assert_eq!(packet.round, 1);
    let h = &packet.headline;
    assert_eq!((h.files, h.added, h.removed, h.withheld), (4, 4, 4, 1));
    assert_eq!(h.pending_reviews, 0);
    assert_eq!(h.ai.len(), 2);
    assert_eq!(
        h.ai.iter().map(|t| t.by_severity).collect::<Vec<_>>(),
        vec![[1, 1, 1, 0], [1, 0, 0, 0]]
    );

    let first = &packet.agenda[0];
    assert_eq!(
        (first.hunk.anchor.path.as_str(), first.hunk.header.new_start),
        ("src/a.rs", 1)
    );
    assert_eq!(first.tier, Tier::Blocking);
    assert!(first.signals.contains(&Signal::Finding {
        severity: argus_core::views::review::Severity::High,
        agreeing_models: 2
    }));
    assert!(matches!(
        &first.provenance,
        Provenance::Direct {
            coverage_permille: 1000,
            ..
        }
    ));
    assert_eq!(first.findings.len(), 2);
    assert!(
        first
            .findings
            .iter()
            .all(|f| f.applicability == Applicability::Current && f.agreeing_models == 2)
    );

    // Every hunk is on the agenda once, ordered by tier.
    assert_eq!(packet.agenda.len(), 5);
    let tiers: Vec<Tier> = packet.agenda.iter().map(|a| a.tier).collect();
    let mut sorted = tiers.clone();
    sorted.sort();
    assert_eq!(tiers, sorted);
    let tail = packet
        .agenda
        .iter()
        .find(|a| a.hunk.anchor.path.as_str() == "tools/tail.txt")
        .unwrap();
    assert!(tail.signals.contains(&Signal::SensitivePath));
    let added = packet
        .agenda
        .iter()
        .find(|a| a.hunk.header.new_start == 8)
        .unwrap();
    assert_eq!(added.provenance, Provenance::Unexplained);
    assert_eq!(added.tier, Tier::Suspicious);
    assert_eq!(h.unexplained, 4);

    // The unlocated finding is shown, not dropped.
    assert_eq!(packet.unattached.len(), 1);
    assert_eq!(packet.unattached[0].finding.title, "no tests");
    assert!(packet.approved.is_none());
}

#[test]
fn a_packet_for_an_old_head_is_refused() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, files('a'));
    job.snapshot_of(11, oid('2'), Some(oid('1')), files('a'));
    let stale = head_diff('1');
    assert!(job.ledger.views().review_packet(&stale, &cfg()).is_err());
    assert!(
        job.ledger
            .views()
            .review_packet(&head_diff('2'), &cfg())
            .is_ok()
    );
}

#[test]
fn the_second_round_is_an_interdiff_with_unaddressed_threads_first() {
    let mut job = Job::open();
    job.agent(1);
    job.snapshot_of(10, oid('1'), None, files('a'));
    job.review(5, '1', "opus", FINDINGS_2);
    let anchor = Anchor::Code {
        tree: oid('1'),
        path: repo("src/a.rs"),
        lines: LineRange::new(2, 2).unwrap(),
    };
    let thread = Event::ThreadOpened {
        thread: ThreadId(Id128(1)),
        comment: CommentId(Id128(100)),
        anchor,
        body: "keep the old name".into(),
    };
    job.ok(Author::Human, thread);
    job.ok(
        Author::Human,
        decided(1, Some(oid('1')), Decision::RequestChanges),
    );
    // The agent produced a new tree but left src/a.rs as it was.
    job.snapshot_of(11, oid('2'), Some(oid('1')), files('a'));

    let diff = head_diff('2');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    assert_eq!(packet.round, 2);
    assert_eq!(packet.headline.unaddressed, 1);
    let first = &packet.agenda[0];
    assert_eq!(first.hunk.header.new_start, 1);
    assert!(
        first
            .signals
            .contains(&Signal::Unaddressed(ThreadId(Id128(1))))
    );
    assert_eq!(first.tier, Tier::Blocking);
    assert_eq!(first.threads.len(), 1);
    assert_eq!(first.threads[0].at, LineRange::new(2, 2));
    // The old review still holds: the file is byte-identical at head.
    assert_eq!(first.findings.len(), 1);
    assert_eq!(
        first.findings[0].applicability,
        Applicability::CarriedForward { from: oid('1') }
    );

    // A hunk the human already decided on, with nothing new about it, is collapsed.
    let tail = packet
        .agenda
        .iter()
        .find(|a| a.hunk.anchor.path.as_str() == "tools/tail.txt")
        .unwrap();
    assert_eq!(tail.tier, Tier::Collapsed);
    assert_eq!(packet.agenda.last().unwrap().tier, Tier::Collapsed);
}

#[test]
fn a_finding_on_a_changed_file_is_not_carried_forward() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, files('a'));
    job.review(5, '1', "opus", FINDINGS_2);
    job.snapshot_of(11, oid('2'), Some(oid('1')), files('c'));
    let diff = head_diff('2');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    assert!(packet.agenda.iter().all(|a| a.findings.is_empty()));
    assert!(packet.unattached.is_empty());
    assert!(packet.headline.ai.is_empty());
}

// ---------------------------------------------------------------- policy

use argus_core::event::{CheckRequest, CheckSpec, LeanMode, TlaEngine};
use argus_core::models::ModelSet;
use argus_core::policy::{PolicyConfig, decide};
use argus_core::views::timeline::BucketWidth;

impl Job {
    /// Decides, submits what was decided, and returns it.
    fn decide(&mut self, cfg: &PolicyConfig, now: i64) -> Vec<Event> {
        let events = decide(self.ledger.views(), cfg, UnixMillis(now));
        for e in &events {
            self.ok(Author::Policy, e.clone());
        }
        events
    }
}

fn requested_purposes(events: &[Event]) -> Vec<&RunPurpose> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::RunRequested { purpose, .. } => Some(purpose),
            _ => None,
        })
        .collect()
}

#[test]
fn a_turn_end_is_snapshotted_once() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[turn_end("a1", T)]);
    let off = PolicyConfig::default();
    assert!(decide(job.ledger.views(), &off, UnixMillis(T)).is_empty());

    let cfg = PolicyConfig {
        snapshot_on_turn_end: true,
        ..PolicyConfig::default()
    };
    let turn_end_fact = job
        .ledger
        .views()
        .transcript()
        .latest_turn_end(&run(1))
        .cloned();
    let first = job.decide(&cfg, T);
    assert!(
        matches!(requested_purposes(&first).as_slice(), [RunPurpose::Snapshot { after }] if *after == turn_end_fact),
        "{first:?}"
    );
    assert!(job.decide(&cfg, T).is_empty());
    // The next turn end is a new snapshot.
    job.transcript(1, &[turn_end("a2", T + 5000)]);
    assert_eq!(job.decide(&cfg, T).len(), 1);
}

#[test]
fn a_new_head_gets_the_configured_review_and_the_checks_it_can_affect() {
    let mut job = Job::open();
    let models = ModelSet::new([model("opus"), model("sonnet")]).unwrap();
    let lean = CheckSpec::Lean {
        root: repo("proofs"),
        mode: LeanMode::LakeBuild,
    };
    let tla = CheckSpec::Tla {
        spec: repo("specs/tla/Lease.tla"),
        config: None,
        engine: TlaEngine::JavaTlc,
        workers: None,
    };
    let cfg = PolicyConfig {
        review_on_snapshot: Some((models, ReviewLens::General)),
        checks_on_snapshot: vec![lean.clone(), tla],
        ..PolicyConfig::default()
    };
    assert!(job.decide(&cfg, T).is_empty());
    let mut changed = files('a');
    changed.push(FileState {
        path: repo("proofs/ArgusProofs/Replay.lean"),
        blob: Some(oid('e')),
        hunks: Vec::new(),
    });
    job.snapshot_of(10, oid('1'), None, changed);
    let events = job.decide(&cfg, T);
    let purposes = requested_purposes(&events);
    let reviews: Vec<_> = purposes
        .iter()
        .filter_map(|p| match p {
            RunPurpose::Review(spec) => Some((spec.tree.clone(), spec.llm.model.to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        reviews,
        vec![
            (oid('1'), "claude:opus".to_owned()),
            (oid('1'), "claude:sonnet".to_owned())
        ]
    );
    // Only the Lean package changed; the TLA+ spec did not.
    let checks: Vec<_> = purposes
        .iter()
        .filter_map(|p| match p {
            RunPurpose::Check(CheckRequest { tree, spec, .. }) => {
                Some((tree.clone(), spec.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(checks, vec![(oid('1'), lean)]);
    assert!(job.decide(&cfg, T).is_empty());
}

#[test]
fn feedback_waits_for_the_turn_then_resumes_the_agent_with_exactly_the_preview() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[tool_call("a1", T)]);
    job.snapshot_of(10, oid('1'), None, files('a'));
    let anchor = Anchor::Code {
        tree: oid('1'),
        path: repo("src/a.rs"),
        lines: LineRange::new(2, 2).unwrap(),
    };
    job.ok(
        Author::Human,
        Event::ThreadOpened {
            thread: ThreadId(Id128(1)),
            comment: CommentId(Id128(100)),
            anchor,
            body: "keep the old name".into(),
        },
    );
    let preview = job.ledger.views().preview_feedback("Almost.");
    let decision = Event::Decided {
        decision: DecisionId(Id128(1)),
        tree: Some(oid('1')),
        verdict: Decision::RequestChanges,
        note: "Almost.".into(),
    };
    job.ok(Author::Human, decision);
    let cfg = PolicyConfig::default();

    // Mid-turn: nothing yet.
    assert!(job.decide(&cfg, T).is_empty());
    // At its prompt: ask it to exit, once.
    job.transcript(1, &[turn_end("a2", T + 1000)]);
    assert_eq!(
        job.decide(&cfg, T),
        vec![Event::RunCancelRequested { run: run(1) }]
    );
    assert!(job.decide(&cfg, T).is_empty());
    // Gone: resume it with the feedback.
    job.finish(
        1,
        RunOutcome::Killed {
            signal: 15,
            cancelled: true,
        },
    );
    let events = job.decide(&cfg, T);
    let [
        Event::RunRequested {
            run: delivery,
            purpose: RunPurpose::Agent(launch),
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(*delivery, DecisionId(Id128(1)).delivery_run());
    assert_eq!(launch.resume, Some(run(1)));
    assert_eq!(launch.cli, AgentCli::Claude);
    assert_eq!(launch.prompt, preview);
    assert!(launch.prompt.contains("- src/a.rs:2: keep the old name"));
    assert!(job.decide(&cfg, T).is_empty());
}

#[test]
fn abandoning_a_job_stops_every_live_run() {
    let mut job = Job::open();
    job.agent(1);
    job.agent(2);
    job.finish(2, RunOutcome::Exited { code: 0 });
    job.snapshot_of(10, oid('1'), None, files('a'));
    job.ok(Author::Human, decided(1, Some(oid('1')), Decision::Abandon));
    let cfg = PolicyConfig::default();
    assert_eq!(
        job.decide(&cfg, T),
        vec![Event::RunCancelRequested { run: run(1) }]
    );
    assert!(job.decide(&cfg, T).is_empty());
}

#[test]
fn every_ended_window_with_activity_is_summarized_once() {
    let mut job = Job::open();
    job.agent(1);
    job.transcript(1, &[tool_call("a1", T), turn_end("a2", T + 20_000)]);
    let cfg = PolicyConfig {
        summaries: Some((BucketWidth::M15, model("haiku"))),
        ..PolicyConfig::default()
    };
    let later = T + 3 * 60 * 60 * 1000;
    let events = job.decide(&cfg, later);
    let windows: Vec<_> = requested_purposes(&events)
        .into_iter()
        .filter_map(|p| match p {
            RunPurpose::Summarize { window, llm } => Some((*window, llm.clone())),
            _ => None,
        })
        .collect();
    // The agent's records, and the job's own events (stamped now) in a window not yet ended.
    assert_eq!(windows.len(), 1, "{events:?}");
    let (window, llm) = &windows[0];
    assert_eq!(window.width, BucketWidth::M15);
    assert!(window.start.0 <= T && T < window.end().0);
    assert_eq!(llm.model.to_string(), "claude:haiku");
    assert!(llm.prompt.contains("Bash"), "{}", llm.prompt);
    assert!(job.decide(&cfg, later).is_empty());
}

#[test]
fn a_job_with_nothing_to_judge_can_only_be_abandoned() {
    let mut job = Job::open();
    let approve = job.submit(Author::Human, decided(1, None, Decision::Approve));
    assert!(matches!(
        approve,
        Err(LedgerError::Rejected(Rejected::NotHead { head: None }))
    ));
    let changes = job.submit(Author::Human, decided(2, None, Decision::RequestChanges));
    assert!(matches!(
        changes,
        Err(LedgerError::Rejected(Rejected::NotHead { .. }))
    ));
    job.ok(Author::Human, decided(3, None, Decision::Abandon));
    // Once there is a head, abandoning names it like any verdict.
    job.snapshot(10, oid('1'));
    let blind = job.submit(Author::Human, decided(4, None, Decision::Abandon));
    assert!(matches!(
        blind,
        Err(LedgerError::Rejected(Rejected::NotHead { .. }))
    ));
}

// ---------------------------------------------------------------- packet, found by mutation testing

/// Three files: two hunks in `src/a.rs` (three added lines at the top; one line replaced at
/// 12), one in `src/b.rs`, one in a Lean package.
const SMALL_DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -0,0 +1,3 @@
+one
+two
+three
@@ -10,1 +12,1 @@
-old
+new
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,1 @@
-b
+B
diff --git a/proofs/A.lean b/proofs/A.lean
--- a/proofs/A.lean
+++ b/proofs/A.lean
@@ -1,1 +1,2 @@
-x
+y
+z
";

fn small_diff(tree: char) -> HeadDiff {
    HeadDiff {
        request: DiffRequest {
            base: oid('b'),
            tree: oid(tree),
        },
        hunks: parse_unified(SMALL_DIFF),
    }
}

fn agenda_line(item: &argus_core::views::packet::AgendaItem<'_>) -> (String, u32) {
    (
        item.hunk.anchor.path.as_str().to_owned(),
        item.hunk.header.new_start,
    )
}

fn finding_signals(item: &argus_core::views::packet::AgendaItem<'_>) -> Vec<u16> {
    item.signals
        .iter()
        .filter_map(|s| match s {
            Signal::Finding {
                agreeing_models, ..
            } => Some(*agreeing_models),
            _ => None,
        })
        .collect()
}

#[test]
fn a_lone_high_finding_is_suspicious_and_two_agreeing_ones_block() {
    use argus_core::views::packet::tier;
    use argus_core::views::review::Severity;
    let high = |agreeing_models| Signal::Finding {
        severity: Severity::High,
        agreeing_models,
    };
    assert_eq!(tier(&[high(1)], false), Tier::Suspicious);
    assert_eq!(tier(&[high(2)], false), Tier::Blocking);
}

#[test]
fn hunks_of_one_tier_are_ordered_by_how_much_they_change() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, Vec::new());
    let diff = small_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    // All unexplained, so one tier: 3 added beats 2 added, beats 1 added + 1 removed
    // and 1 + 1 (a product would order them otherwise).
    let order: Vec<_> = packet.agenda.iter().map(agenda_line).collect();
    assert_eq!(order[0], ("src/a.rs".to_owned(), 1));
    assert_eq!(order[1], ("proofs/A.lean".to_owned(), 1));
    assert!(packet.agenda.iter().all(|a| a.tier == Tier::Suspicious));
}

#[test]
fn overlapping_findings_agree_transitively_and_others_stand_alone() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, Vec::new());
    let at = |path: &str, start: u32, end: u32| {
        format!(
            r#"{{"path":"{path}","start":{start},"end":{end},"severity":"high","title":"t","body":""}}"#
        )
    };
    // 1-1 and 3-3 do not overlap, but 1-3 overlaps both: one cluster of three models.
    job.review(
        5,
        '1',
        "opus",
        &format!("[{}, {}]", at("src/a.rs", 1, 1), at("src/b.rs", 1, 1)),
    );
    job.review(6, '1', "sonnet", &format!("[{}]", at("src/a.rs", 3, 3)));
    job.review(7, '1', "haiku", &format!("[{}]", at("src/a.rs", 1, 3)));
    // The same lines in another file are another place.
    job.review(8, '1', "fable", &format!("[{}]", at("src/c.rs", 1, 1)));
    let diff = small_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    let a = packet
        .agenda
        .iter()
        .find(|i| agenda_line(i) == ("src/a.rs".to_owned(), 1))
        .unwrap();
    assert_eq!(finding_signals(a), vec![3, 3, 3]);
    let b = packet
        .agenda
        .iter()
        .find(|i| agenda_line(i).0 == "src/b.rs")
        .unwrap();
    assert_eq!(finding_signals(b), vec![1]);
    let replaced = packet
        .agenda
        .iter()
        .find(|i| agenda_line(i) == ("src/a.rs".to_owned(), 12))
        .unwrap();
    assert!(replaced.findings.is_empty());
    assert_eq!(packet.unattached.len(), 1);
    assert_eq!(packet.unattached[0].agreeing_models, 1);
}

#[test]
fn a_dismissed_finding_is_shown_but_raises_no_signal() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, Vec::new());
    job.review(
        5,
        '1',
        "opus",
        r#"[{"path":"src/b.rs","start":1,"end":1,"severity":"high","title":"t","body":""}]"#,
    );
    let finding = {
        let views = job.ledger.views();
        let (_, r) = views.review().reviews().next().unwrap();
        let argus_core::views::review::ReviewOutput::Findings(found) = &r.output else {
            panic!()
        };
        found[0].id.clone()
    };
    job.ok(
        Author::Human,
        Event::FindingDismissed {
            finding,
            dismissed: true,
        },
    );
    let diff = small_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    let b = packet
        .agenda
        .iter()
        .find(|i| agenda_line(i).0 == "src/b.rs")
        .unwrap();
    assert_eq!(b.findings.len(), 1);
    assert!(b.findings[0].dismissed);
    assert!(finding_signals(b).is_empty());
}

#[test]
fn only_a_sent_unresolved_thread_whose_lines_survived_is_unaddressed() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, Vec::new());
    let open = |job: &mut Job, t: u128, path: &str, line: u32| {
        let anchor = Anchor::Code {
            tree: oid('1'),
            path: repo(path),
            lines: LineRange::new(line, line).unwrap(),
        };
        job.ok(
            Author::Human,
            Event::ThreadOpened {
                thread: ThreadId(Id128(t)),
                comment: CommentId(Id128(t * 100)),
                anchor,
                body: format!("thread {t}"),
            },
        );
    };
    open(&mut job, 1, "src/a.rs", 12);
    open(&mut job, 2, "src/a.rs", 2);
    // Outside every hunk: a loose thread.
    open(&mut job, 3, "src/b.rs", 5);
    job.ok(
        Author::Human,
        decided(1, Some(oid('1')), Decision::RequestChanges),
    );
    job.ok(
        Author::Human,
        Event::ThreadResolved {
            thread: ThreadId(Id128(2)),
            resolved: true,
        },
    );
    // A draft opened after the decision was never sent.
    open(&mut job, 4, "src/a.rs", 3);
    job.snapshot_of(11, oid('2'), Some(oid('1')), Vec::new());

    let diff = small_diff('2');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    let unaddressed: Vec<u128> = packet
        .agenda
        .iter()
        .flat_map(|a| &a.threads)
        .chain(&packet.loose_threads)
        .filter(|t| t.unaddressed)
        .map(|t| t.id.0.0)
        .collect();
    // Thread 3 is outside every hunk, but its lines survived too: loose and unaddressed.
    assert_eq!(unaddressed, vec![1, 3]);
    let loose: Vec<u128> = packet.loose_threads.iter().map(|t| t.id.0.0).collect();
    assert_eq!(loose, vec![3]);
    let top = packet
        .agenda
        .iter()
        .find(|i| agenda_line(i) == ("src/a.rs".to_owned(), 1))
        .unwrap();
    let mut held: Vec<u128> = top.threads.iter().map(|t| t.id.0.0).collect();
    held.sort();
    assert_eq!(held, vec![2, 4]);
}

#[test]
fn a_failing_check_on_the_head_marks_the_hunk_it_points_into() {
    use argus_core::event::{CheckRequest, CheckSpec, LeanMode};
    let mut job = Job::open();
    job.snapshot_of(10, oid('0'), None, Vec::new());
    job.snapshot_of(11, oid('1'), Some(oid('0')), Vec::new());
    let check = |job: &mut Job, n: u128, tree: char, root: &str, out: &str, code: i32| {
        let request = CheckRequest {
            tree: oid(tree),
            base: oid('b'),
            spec: CheckSpec::Lean {
                root: repo(root),
                mode: LeanMode::LakeBuild,
            },
        };
        job.ok(
            Author::Policy,
            Event::RunRequested {
                run: run(n),
                purpose: RunPurpose::Check(request),
            },
        );
        job.start(n);
        let lines: Vec<String> = out.lines().map(str::to_owned).collect();
        job.feed(n, SourceKind::LeanText, SourceOrigin::Stdout, &lines);
        job.finish(n, RunOutcome::Exited { code });
    };
    let error = "error: A.lean:2:1: unsolved goals";
    check(&mut job, 20, '1', "proofs", error, 1);
    // The same message from a check of an older tree, and a check that passed.
    check(
        &mut job,
        21,
        '0',
        "other",
        "error: A.lean:2:1: unsolved goals",
        1,
    );
    check(&mut job, 22, '1', "src", "warning: a.rs:1:1: unused", 0);
    let diff = small_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    let failed: Vec<_> = packet
        .agenda
        .iter()
        .filter(|a| {
            a.signals
                .iter()
                .any(|s| matches!(s, Signal::CheckFailed { .. }))
        })
        .map(agenda_line)
        .collect();
    assert_eq!(failed, vec![("proofs/A.lean".to_owned(), 1)]);
    assert_eq!(packet.agenda[0].tier, Tier::Blocking);
}

#[test]
fn reviews_still_running_are_counted_not_tallied() {
    let mut job = Job::open();
    job.snapshot_of(10, oid('1'), None, Vec::new());
    for (n, name) in [(5, "opus"), (6, "sonnet")] {
        let spec = ReviewSpec {
            tree: oid('1'),
            base: oid('b'),
            lens: ReviewLens::General,
            llm: LlmCall {
                model: model(name),
                prompt: "review".into(),
            },
        };
        job.ok(
            Author::Human,
            Event::RunRequested {
                run: run(n),
                purpose: RunPurpose::Review(spec),
            },
        );
    }
    let diff = small_diff('1');
    let packet = job.ledger.views().review_packet(&diff, &cfg()).unwrap();
    assert_eq!(packet.headline.pending_reviews, 2);
    assert!(packet.headline.ai.is_empty());
}
