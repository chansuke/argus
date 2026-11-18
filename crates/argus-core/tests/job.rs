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
