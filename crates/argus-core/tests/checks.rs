//! Formal and device checks end to end: captured TLC and Lean output (real runs, paths
//! rewritten) goes through the framer, the decoder, and the checks view, the way the host
//! feeds it.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_core::event::{
    AppSource, Author, CheckRequest, CheckSpec, CheckTarget, DeviceBudget, DeviceDriver,
    DevicePlatform, Event, LeanMode, LedgerKind, Rejected, RunOutcome, RunPurpose, TlaEngine,
};
use argus_core::fact::Fact;
use argus_core::ids::{Attempt, BlobId, GitOid, Id128, LineRange, RepoPath, RunId, TreeOid};
use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};
use argus_core::views::checks::{CheckOutput, CheckVerdict, Checks};
use argus_core::views::{GateState, Projection, ViewMask, ViewSet, ViewStamp};
use argus_core::{Ledger, Submitted};

#[derive(Default)]
struct CheckViews {
    checks: Checks,
}

impl ViewSet for CheckViews {
    const KIND: LedgerKind = LedgerKind::Job;
    fn wants(source: &SourceMeta) -> bool {
        Checks::READS.contains(&source.kind.family())
    }
    fn apply(&mut self, fact: &Fact<'_>) {
        self.checks.apply(fact);
    }
    fn admit(&self, _: &Author, _: &Event) -> Result<(), Rejected> {
        Ok(())
    }
    fn take_reads(&self) -> ViewMask {
        ViewMask::default()
    }
    fn stamp(&self, _: ViewMask) -> ViewStamp {
        ViewStamp::default()
    }
}

struct Job {
    ledger: Ledger<CheckViews>,
}

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "argus-checks-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn run(n: u128) -> RunId {
    RunId(Id128(n))
}

fn tree(c: char) -> TreeOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn path(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

fn tla(engine: TlaEngine) -> CheckSpec {
    CheckSpec::Tla {
        spec: path("specs/tla/Lease.tla"),
        config: Some(path("specs/tla/Lease.cfg")),
        engine,
        workers: None,
    }
}

fn lean(mode: LeanMode) -> CheckSpec {
    CheckSpec::Lean {
        root: path("proofs"),
        mode,
    }
}

fn device() -> CheckSpec {
    CheckSpec::Device {
        project: path("apps/mobile"),
        platform: DevicePlatform::Ios,
        app: AppSource::ExpoGo { sdk: None },
        driver: DeviceDriver::Script(path("apps/mobile/e2e/smoke.sh")),
        budget: DeviceBudget {
            max_minutes: 5,
            max_idle_minutes: 1,
        },
    }
}

impl Job {
    fn new() -> Self {
        Job {
            ledger: Ledger::open(&scratch()).unwrap(),
        }
    }

    fn checks(&self) -> &Checks {
        &self.ledger.views().checks
    }

    fn submit(&mut self, author: Author, event: Event) {
        let done = self.ledger.submit(author, event).unwrap();
        assert!(matches!(done, Submitted::Appended(_)), "{done:?}");
    }

    fn request(&mut self, n: u128, on: char, spec: CheckSpec) {
        let request = CheckRequest {
            tree: tree(on),
            base: tree('b'),
            spec,
        };
        let event = Event::RunRequested {
            run: run(n),
            purpose: RunPurpose::Check(request),
        };
        self.submit(Author::Policy, event);
    }

    fn start(&mut self, n: u128, attempt: u16) {
        let event = Event::RunStarted {
            run: run(n),
            attempt: Attempt(attempt),
            pid: None,
        };
        self.submit(Author::Host, event);
    }

    /// Feeds `stdout` as the attempt's stdout source, in two chunks split mid-line.
    fn output(&mut self, n: u128, attempt: u16, kind: SourceKind, stdout: &[u8]) {
        let origin = SourceOrigin::Stdout;
        let meta = SourceMeta {
            id: SourceMeta::derive_id(run(n), Attempt(attempt), &origin),
            run: run(n),
            attempt: Attempt(attempt),
            kind,
            origin,
        };
        self.ledger.open_source(meta.clone()).unwrap();
        let mut framer = kind.framer(0);
        let (a, b) = stdout.split_at(stdout.len() / 2);
        let mut frames = framer.push(a);
        frames.extend(framer.push(b));
        frames.extend(framer.finish());
        self.ledger.observe(meta.id, frames).unwrap();
    }

    fn finish(&mut self, n: u128, attempt: u16, outcome: RunOutcome) {
        let event = Event::RunFinished {
            run: run(n),
            attempt: Attempt(attempt),
            outcome,
        };
        self.submit(Author::Host, event);
    }

    /// One whole attempt: started, output captured, exited with `code`.
    fn check(
        &mut self,
        n: u128,
        on: char,
        spec: CheckSpec,
        kind: SourceKind,
        out: &[u8],
        code: i32,
    ) {
        self.request(n, on, spec);
        self.start(n, 0);
        self.output(n, 0, kind, out);
        self.finish(n, 0, RunOutcome::Exited { code });
    }

    fn verdict(&self, n: u128) -> CheckVerdict {
        let check = self.checks().run(&run(n)).unwrap();
        check.verdict.clone().map(|(_, v)| v).expect("a verdict")
    }
}

// ---------------------------------------------------------------- TLA+

#[test]
fn tlc_success_is_proved() {
    let mut job = Job::new();
    let out = fixture("tlc/pass.out");
    job.check(
        1,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &out,
        0,
    );
    assert_eq!(job.verdict(1), CheckVerdict::Proved);
    let CheckOutput::Tlc(messages) = &job.checks().run(&run(1)).unwrap().output else {
        panic!()
    };
    // Every STARTMSG block, and nothing else (stray lines stay in the raw log).
    assert_eq!(messages.len(), 17);
    assert!(messages.iter().any(|(_, m)| m.code == 2193 && m.class == 0));
}

#[test]
fn an_invariant_violation_is_refuted_with_its_trace() {
    let mut job = Job::new();
    let out = fixture("tlc/violated.out");
    job.check(
        1,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &out,
        12,
    );
    let CheckVerdict::Refuted { summary, trace } = job.verdict(1) else {
        panic!("{:?}", job.verdict(1))
    };
    assert_eq!(summary, "Invariant Tracked is violated.");
    let CheckOutput::Tlc(messages) = &job.checks().run(&run(1)).unwrap().output else {
        panic!()
    };
    let states: Vec<_> = messages
        .iter()
        .filter(|(_, m)| m.class == 4)
        .map(|(f, _)| f.clone())
        .collect();
    assert_eq!(states.len(), 2);
    assert_eq!(trace, states);
    let first = &messages.iter().find(|(f, _)| f == &trace[0]).unwrap().1;
    assert!(
        first
            .text
            .starts_with("1: <Initial predicate>\n/\\ remote ="),
        "{first:?}"
    );
}

#[test]
fn tlc_output_and_exit_code_must_agree() {
    let mut job = Job::new();
    // A reported violation with a success exit, and success messages with a failure exit.
    job.check(
        1,
        '1',
        tla(TlaEngine::Tlars),
        SourceKind::TlcTool,
        &fixture("tlc/violated.out"),
        0,
    );
    job.check(
        2,
        '1',
        tla(TlaEngine::Tlars),
        SourceKind::TlcTool,
        &fixture("tlc/pass.out"),
        12,
    );
    assert!(
        matches!(job.verdict(1), CheckVerdict::ToolError { .. }),
        "{:?}",
        job.verdict(1)
    );
    assert!(
        matches!(job.verdict(2), CheckVerdict::ToolError { .. }),
        "{:?}",
        job.verdict(2)
    );
}

#[test]
fn a_spec_that_does_not_parse_is_a_tool_error() {
    let mut job = Job::new();
    let out = fixture("tlc/parse-error.out");
    job.check(
        1,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &out,
        150,
    );
    assert_eq!(
        job.verdict(1),
        CheckVerdict::ToolError {
            summary: "Parsing or semantic analysis failed.".into()
        }
    );
}

#[test]
fn engines_that_disagree_on_one_tree_are_reported() {
    let mut job = Job::new();
    let (pass, violated) = (fixture("tlc/pass.out"), fixture("tlc/violated.out"));
    job.check(
        1,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &pass,
        0,
    );
    job.check(2, '1', tla(TlaEngine::Tlars), SourceKind::TlcTool, &pass, 0);
    assert!(job.checks().disagreements().is_empty());
    job.check(
        3,
        '2',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &pass,
        0,
    );
    job.check(
        4,
        '2',
        tla(TlaEngine::Tlars),
        SourceKind::TlcTool,
        &violated,
        12,
    );
    let target = tla(TlaEngine::JavaTlc).target();
    assert_eq!(job.checks().disagreements(), vec![(&target, &tree('2'))]);
}

// ---------------------------------------------------------------- Lean

#[test]
fn a_clean_lake_build_is_proved() {
    let mut job = Job::new();
    let out = fixture("lean/ok.txt");
    job.check(
        1,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        &out,
        0,
    );
    assert_eq!(job.verdict(1), CheckVerdict::Proved);
}

#[test]
fn sorry_builds_but_is_never_proved() {
    let mut job = Job::new();
    let text = fixture("lean/sorry.txt");
    job.check(
        1,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        &text,
        0,
    );
    assert_eq!(job.verdict(1), CheckVerdict::Holes { sorries: 1 });
    let files = LeanMode::Files(vec![path("proofs/ArgusProofs/Holey.lean")]);
    job.check(
        2,
        '1',
        lean(files),
        SourceKind::LeanJson,
        &fixture("lean/holey.json"),
        0,
    );
    assert_eq!(job.verdict(2), CheckVerdict::Holes { sorries: 1 });
}

#[test]
fn a_failed_proof_is_refuted_and_keeps_its_whole_message() {
    let mut job = Job::new();
    let text = fixture("lean/err.txt");
    job.check(
        1,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        &text,
        1,
    );
    let files = LeanMode::Files(vec![path("proofs/ArgusProofs/Broken.lean")]);
    job.check(
        2,
        '1',
        lean(files),
        SourceKind::LeanJson,
        &fixture("lean/broken.json"),
        1,
    );

    for n in [1, 2] {
        let CheckVerdict::Refuted { summary, trace } = job.verdict(n) else {
            panic!("{:?}", job.verdict(n))
        };
        assert_eq!(summary, "ArgusProofs/Broken.lean:1:40: unsolved goals");
        assert_eq!(trace.len(), 1);
        let CheckOutput::Lean(messages) = &job.checks().run(&run(n)).unwrap().output else {
            panic!()
        };
        let error = &messages.iter().find(|(f, _)| f == &trace[0]).unwrap().1;
        // The goal state on the following lines belongs to the message.
        assert_eq!(error.text, "unsolved goals\nn : Nat\n⊢ False");
        assert_eq!((error.line, error.col), (1, 40));
    }
    // Locations are repo-relative: the package root plus Lean's own path.
    assert_eq!(
        job.checks().located(&run(1)),
        vec![
            (
                path("proofs/ArgusProofs/Holey.lean"),
                LineRange::new(1, 1).unwrap()
            ),
            (
                path("proofs/ArgusProofs/Broken.lean"),
                LineRange::new(1, 1).unwrap()
            ),
        ]
    );
}

#[test]
fn lake_failing_without_a_located_error_is_a_tool_error() {
    let mut job = Job::new();
    let out = b"error: no such file or directory (error code: 2)\n  file: ./lakefile.toml\n";
    job.check(
        1,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        out,
        1,
    );
    assert!(
        matches!(job.verdict(1), CheckVerdict::ToolError { .. }),
        "{:?}",
        job.verdict(1)
    );
}

// ---------------------------------------------------------------- device, lifecycle, gate

#[test]
fn a_device_check_passes_on_exit_zero_and_keeps_its_evidence() {
    let mut job = Job::new();
    job.request(1, '1', device());
    job.start(1, 0);
    let shot = Event::ArtifactCaptured {
        run: run(1),
        attempt: Attempt(0),
        blob: BlobId([7; 32]),
        name: "home.png".into(),
        media_type: "image/png".into(),
        len: 3,
    };
    job.submit(Author::Host, shot);
    job.finish(1, 0, RunOutcome::Exited { code: 0 });
    assert_eq!(job.verdict(1), CheckVerdict::Passed);
    let evidence = &job.checks().run(&run(1)).unwrap().evidence;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].name, "home.png");

    job.request(2, '1', device());
    job.start(2, 0);
    job.finish(2, 0, RunOutcome::Exited { code: 1 });
    assert!(matches!(job.verdict(2), CheckVerdict::ToolError { .. }));
}

#[test]
fn a_retried_attempt_starts_from_clean_output() {
    let mut job = Job::new();
    job.request(1, '1', tla(TlaEngine::JavaTlc));
    job.start(1, 0);
    job.finish(1, 0, RunOutcome::Abandoned);
    assert!(job.checks().run(&run(1)).unwrap().verdict.is_none());
    job.start(1, 1);
    // The first attempt's output is ingested late, after the retry started.
    job.output(1, 0, SourceKind::TlcTool, &fixture("tlc/violated.out"));
    job.output(1, 1, SourceKind::TlcTool, &fixture("tlc/pass.out"));
    job.finish(1, 1, RunOutcome::Exited { code: 0 });
    assert_eq!(job.verdict(1), CheckVerdict::Proved);
    let CheckOutput::Tlc(messages) = &job.checks().run(&run(1)).unwrap().output else {
        panic!()
    };
    assert_eq!(messages.len(), 17);
}

#[test]
fn a_cancelled_check_says_so() {
    let mut job = Job::new();
    job.request(1, '1', tla(TlaEngine::JavaTlc));
    job.start(1, 0);
    let killed = RunOutcome::Killed {
        signal: 15,
        cancelled: true,
    };
    job.finish(1, 0, killed);
    assert_eq!(job.verdict(1), CheckVerdict::Cancelled);
}

#[test]
fn the_checks_gate_reads_the_latest_run_of_each_target_on_the_head() {
    let mut job = Job::new();
    let head = tree('1');
    assert_eq!(job.checks().gate(Some(&head)), GateState::NotConfigured);
    job.request(1, '1', tla(TlaEngine::JavaTlc));
    assert_eq!(job.checks().gate(Some(&head)), GateState::Pending);
    job.start(1, 0);
    job.output(1, 0, SourceKind::TlcTool, &fixture("tlc/pass.out"));
    job.finish(1, 0, RunOutcome::Exited { code: 0 });
    assert_eq!(job.checks().gate(Some(&head)), GateState::Green);

    job.check(
        2,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        &fixture("lean/sorry.txt"),
        0,
    );
    assert_eq!(job.checks().gate(Some(&head)), GateState::Red { open: 1 });
    // A rerun of the same target replaces its verdict.
    job.check(
        3,
        '1',
        lean(LeanMode::LakeBuild),
        SourceKind::LeanText,
        &fixture("lean/ok.txt"),
        0,
    );
    assert_eq!(job.checks().gate(Some(&head)), GateState::Green);
    assert_eq!(job.checks().latest().count(), 2);

    // A new head that nothing has checked yet.
    assert_eq!(job.checks().gate(Some(&tree('2'))), GateState::Stale);
    assert_eq!(job.checks().gate(None), GateState::NotConfigured);
}

#[test]
fn a_target_depends_only_on_its_own_files() {
    let tla = tla(TlaEngine::JavaTlc).target();
    assert!(tla.depends_on(&path("specs/tla/Lease.tla")));
    assert!(tla.depends_on(&path("specs/tla/Lease.cfg")));
    assert!(!tla.depends_on(&path("specs/tla/ReviewGate.tla")));
    let lean = lean(LeanMode::LakeBuild).target();
    assert!(lean.depends_on(&path("proofs/ArgusProofs/Replay.lean")));
    assert!(lean.depends_on(&path("proofs/lakefile.toml")));
    assert!(!lean.depends_on(&path("proofsx/a.lean")));
    let device = device().target();
    assert!(device.depends_on(&path("apps/mobile/app/index.tsx")));
    assert!(!device.depends_on(&path("apps/desktop/src/main.tsx")));
    assert_eq!(
        device,
        CheckTarget::Device {
            project: path("apps/mobile"),
            platform: DevicePlatform::Ios
        }
    );
}

#[test]
fn a_rerun_on_the_same_engine_is_not_a_disagreement() {
    let mut job = Job::new();
    let (pass, violated) = (fixture("tlc/pass.out"), fixture("tlc/violated.out"));
    job.check(
        1,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &pass,
        0,
    );
    job.check(
        2,
        '1',
        tla(TlaEngine::JavaTlc),
        SourceKind::TlcTool,
        &violated,
        12,
    );
    assert!(job.checks().disagreements().is_empty());
}
