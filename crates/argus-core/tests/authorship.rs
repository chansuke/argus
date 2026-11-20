//! Who may write which event, into which ledger (`Event::check`): the table behind every
//! submission, checked before any view-dependent rule.

use argus_core::event::{
    AgentLaunch, Author, Decision, Event, LedgerKind, LlmCall, Rejected, ReviewLens, ReviewSpec,
    RunOutcome, RunPurpose, Snapshot,
};
use argus_core::ids::{
    Attempt, CandidateId, DecisionId, FactId, FindingId, GitOid, Id128, RunId, ThreadId, UnixMillis,
};
use argus_core::models::{AgentCli, ModelChoice};
use argus_core::period::Period;
use argus_core::views::review::ReviewBoard;

mod common;

fn oid(c: char) -> GitOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn llm() -> LlmCall {
    LlmCall {
        model: ModelChoice {
            cli: AgentCli::Claude,
            model: argus_core::ids::ModelId("opus".into()),
            effort: None,
        },
        prompt: "p".into(),
    }
}

fn requested(purpose: RunPurpose) -> Event {
    Event::RunRequested {
        run: RunId(Id128(1)),
        purpose,
    }
}

fn agent() -> RunPurpose {
    RunPurpose::Agent(AgentLaunch {
        cli: AgentCli::Claude,
        model: None,
        prompt: "go".into(),
        resume: None,
    })
}

fn review() -> RunPurpose {
    RunPurpose::Review(ReviewSpec {
        tree: oid('1'),
        base: oid('b'),
        lens: ReviewLens::General,
        llm: llm(),
    })
}

/// A merge purpose needs a real approval: snapshot, then approve, on a board.
fn merge() -> RunPurpose {
    let mut board = ReviewBoard::default();
    let mut feed = common::Feed::default();
    let run = RunId(Id128(9));
    feed.authored(
        &mut board,
        1,
        Author::Policy,
        requested_run(run, RunPurpose::Snapshot { after: None }),
    );
    feed.authored(
        &mut board,
        2,
        Author::Host,
        Event::RunStarted {
            run,
            attempt: Attempt(0),
            pid: None,
        },
    );
    let snapshot = Snapshot {
        tree: oid('1'),
        base: oid('b'),
        previous: None,
        delta: Vec::new(),
        files: Vec::new(),
        withheld: Vec::new(),
    };
    feed.authored(
        &mut board,
        3,
        Author::Host,
        Event::RunFinished {
            run,
            attempt: Attempt(0),
            outcome: RunOutcome::Snapshot(snapshot),
        },
    );
    let approve = Event::Decided {
        decision: DecisionId(Id128(1)),
        tree: Some(oid('1')),
        verdict: Decision::Approve,
        note: String::new(),
    };
    feed.authored(&mut board, 4, Author::Human, approve);
    RunPurpose::Merge(board.approved_head().unwrap())
}

fn requested_run(run: RunId, purpose: RunPurpose) -> Event {
    Event::RunRequested { run, purpose }
}

const AGENT: Author = Author::Agent(RunId(Id128(2)));
const ALL: [Author; 4] = [Author::Human, AGENT, Author::Host, Author::Policy];

/// The authors (of the four) that may write `event` into a job ledger.
fn writers(event: &Event) -> Vec<Author> {
    ALL.into_iter()
        .filter(|a| match event.check(a, LedgerKind::Job) {
            Ok(()) => true,
            Err(Rejected::WrongAuthor) => false,
            Err(other) => panic!("{event:?} by {a:?}: {other:?}"),
        })
        .collect()
}

#[test]
fn each_event_has_its_authors() {
    let started = Event::RunStarted {
        run: RunId(Id128(1)),
        attempt: Attempt(0),
        pid: None,
    };
    assert_eq!(writers(&started), vec![Author::Host]);
    let cancel = Event::RunCancelRequested {
        run: RunId(Id128(1)),
    };
    assert_eq!(writers(&cancel), vec![Author::Human, Author::Policy]);
    let task = Event::TaskSet {
        task: argus_core::ids::TaskKey(Id128(1)),
        patch: Default::default(),
    };
    assert_eq!(writers(&task), vec![Author::Human, AGENT]);
    let resolve = Event::ThreadResolved {
        thread: ThreadId(Id128(1)),
        resolved: true,
    };
    assert_eq!(writers(&resolve), vec![Author::Human]);
}

#[test]
fn who_may_request_which_run() {
    assert_eq!(
        writers(&requested(agent())),
        vec![Author::Human, Author::Policy]
    );
    // Agents may ask for review or checks of their own work, nothing else.
    assert_eq!(
        writers(&requested(review())),
        vec![Author::Human, AGENT, Author::Policy]
    );
    // Merging is the human's call alone.
    assert_eq!(writers(&requested(merge())), vec![Author::Human]);
}

#[test]
fn each_event_belongs_to_one_kind_of_ledger() {
    let insight = requested(RunPurpose::Insight {
        period: Period {
            from: UnixMillis(0),
            to: UnixMillis(1),
        },
        llm: llm(),
    });
    assert_eq!(
        insight.check(&Author::Human, LedgerKind::Job),
        Err(Rejected::WrongLedger)
    );
    assert_eq!(insight.check(&Author::Human, LedgerKind::Meta), Ok(()));
    assert_eq!(
        requested(agent()).check(&Author::Human, LedgerKind::Meta),
        Err(Rejected::WrongLedger)
    );

    let candidate: CandidateId = FindingId {
        fact: FactId::parse("human:0.0").unwrap(),
        index: 0,
    };
    let dismissed = Event::CandidateDismissed { candidate };
    assert_eq!(dismissed.check(&Author::Human, LedgerKind::Meta), Ok(()));
    assert_eq!(
        dismissed.check(&Author::Human, LedgerKind::Job),
        Err(Rejected::WrongLedger)
    );
    let resolve = Event::ThreadResolved {
        thread: ThreadId(Id128(1)),
        resolved: true,
    };
    assert_eq!(
        resolve.check(&Author::Human, LedgerKind::Meta),
        Err(Rejected::WrongLedger)
    );
    // The author is judged first.
    assert_eq!(
        dismissed.check(&Author::Policy, LedgerKind::Job),
        Err(Rejected::WrongAuthor)
    );
}
