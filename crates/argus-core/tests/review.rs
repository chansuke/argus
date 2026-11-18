//! The review board: which snapshot is the head, what an approval covers, what AI review
//! found, and what a `RequestChanges` decision sends the agent.

use argus_core::event::{
    Anchor, Author, Decision, Event, FileDelta, Hunk, ReviewLens, ReviewSpec, RunOutcome,
    RunPurpose, Snapshot,
};
use argus_core::ids::{
    Attempt, CommentId, DecisionId, FindingId, GitOid, Id128, LineRange, RepoPath, Stamp, ThreadId,
    TreeOid, UnixMillis,
};
use argus_core::source::AgentWhat;
use argus_core::views::GateState;
use argus_core::views::review::{ReviewBoard, ReviewOutput, Severity, ThreadOrigin};

mod common;
use common::*;

const T0: i64 = 1_792_810_800_000;

fn oid(c: char) -> GitOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn path(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

fn lines(start: u32, end: u32) -> LineRange {
    LineRange::new(start, end).unwrap()
}

fn hunk(old_start: u32, old_len: u32, new_start: u32, new_len: u32) -> Hunk {
    Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    }
}

/// Requests, starts, and finishes snapshot run `n` with `tree`; `delta` is against the
/// previous tree. Returns the stamp of the start, which becomes the entry's cut.
fn snapshot(
    board: &mut ReviewBoard,
    feed: &mut Feed,
    n: u128,
    tree: TreeOid,
    previous: Option<TreeOid>,
    delta: Vec<(&str, Vec<Hunk>)>,
) -> Stamp {
    let at = T0 + n as i64 * 1000;
    let requested = Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Snapshot { after: None },
    };
    feed.authored(board, at, Author::Policy, requested);
    let started = Event::RunStarted {
        run: run(n),
        attempt: Attempt(0),
        pid: None,
    };
    feed.authored(board, at + 1, Author::Host, started);
    let cut = feed.last_stamp();
    let snapshot = Snapshot {
        tree,
        base: oid('b'),
        previous,
        delta: delta
            .into_iter()
            .map(|(p, hunks)| FileDelta {
                path: path(p),
                renamed_from: None,
                hunks,
            })
            .collect(),
        files: Vec::new(),
        withheld: Vec::new(),
    };
    let finished = Event::RunFinished {
        run: run(n),
        attempt: Attempt(0),
        outcome: RunOutcome::Snapshot(snapshot),
    };
    feed.authored(board, at + 2, Author::Host, finished);
    cut
}

fn decide(board: &mut ReviewBoard, feed: &mut Feed, d: u128, tree: char, verdict: Decision) {
    let event = Event::Decided {
        decision: DecisionId(Id128(d)),
        tree: Some(oid(tree)),
        verdict,
        note: "note".into(),
    };
    feed.authored(board, T0 + 50_000 + d as i64, Author::Human, event);
}

fn review_requested(n: u128, tree: char) -> Event {
    Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Review(ReviewSpec {
            tree: oid(tree),
            base: oid('b'),
            lens: ReviewLens::General,
            llm: llm(),
        }),
    }
}

/// A review run of `tree` that answers `text` and exits.
fn review(board: &mut ReviewBoard, feed: &mut Feed, n: u128, tree: char, text: &str) {
    let at = T0 + 20_000 + n as i64 * 10;
    feed.authored(board, at, Author::Human, review_requested(n, tree));
    let result = AgentWhat::Result {
        text: text.into(),
        is_error: false,
    };
    feed.agent(board, &headless(run(n)), at + 1, result);
    let finished = Event::RunFinished {
        run: run(n),
        attempt: Attempt(0),
        outcome: RunOutcome::Exited { code: 0 },
    };
    feed.authored(board, at + 2, Author::Host, finished);
}

const FINDINGS: &str = r#"The gate hides failures.

```json
[
  {"path": "tools/gate", "start": 12, "end": 14, "severity": "high", "title": "step failures are masked", "body": "set -e is off inside the loop"},
  {"path": "/etc/passwd", "start": 1, "end": 1, "severity": "low", "title": "outside the repo", "body": ""},
  {"path": null, "start": null, "end": null, "severity": "nit", "title": "naming", "body": "step_ prefix"}
]
```
"#;

fn thread(board: &mut ReviewBoard, feed: &mut Feed, t: u128, anchor: Anchor, body: &str) {
    let event = Event::ThreadOpened {
        thread: ThreadId(Id128(t)),
        comment: CommentId(Id128(t * 100)),
        anchor,
        body: body.into(),
    };
    feed.authored(board, T0 + 30_000 + t as i64, Author::Human, event);
}

#[test]
fn a_snapshot_that_changed_nothing_is_not_a_new_head() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    assert!(board.head().is_none());
    let cut = snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    let head = board.head().unwrap();
    assert_eq!((head.run, head.cut), (run(1), cut));
    assert_eq!(head.at, UnixMillis(T0 + 1002));

    snapshot(&mut board, &mut feed, 2, oid('1'), Some(oid('1')), vec![]);
    assert_eq!(board.snapshots().len(), 1);
    assert_eq!(board.head().unwrap().run, run(1));
}

#[test]
fn an_approval_covers_exactly_the_head_it_was_given_on() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    decide(&mut board, &mut feed, 1, '1', Decision::Approve);
    assert_eq!(board.approved_head().unwrap().tree(), &oid('1'));

    // A no-op turn keeps the approval; a real change revokes it by construction.
    snapshot(&mut board, &mut feed, 2, oid('1'), Some(oid('1')), vec![]);
    assert!(board.approved_head().is_some());
    snapshot(&mut board, &mut feed, 3, oid('2'), Some(oid('1')), vec![]);
    assert!(board.approved_head().is_none());

    // The latest decision on the head wins.
    decide(&mut board, &mut feed, 2, '2', Decision::Approve);
    decide(&mut board, &mut feed, 3, '2', Decision::RequestChanges);
    assert!(board.approved_head().is_none());
    assert_eq!(board.decisions().len(), 3);
}

#[test]
fn review_findings_are_parsed_and_anchored_on_the_reviewed_tree() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    feed.authored(
        &mut board,
        T0 + 20_000,
        Author::Human,
        review_requested(5, '1'),
    );
    assert!(matches!(
        board.reviews().next().unwrap().1.output,
        ReviewOutput::Running
    ));
    let result = AgentWhat::Result {
        text: FINDINGS.into(),
        is_error: false,
    };
    let (fact, _) = feed.agent(&mut board, &headless(run(5)), T0 + 20_001, result);

    let (_, r) = board.reviews().next().unwrap();
    assert_eq!(r.sources, vec![headless(run(5)).id]);
    let ReviewOutput::Findings(findings) = &r.output else {
        panic!("{:?}", r.output)
    };
    assert_eq!(findings.len(), 3);
    assert_eq!(
        findings[0].id,
        FindingId {
            fact: fact.clone(),
            index: 0
        }
    );
    assert_eq!(
        findings[0].anchor,
        Some(Anchor::Code {
            tree: oid('1'),
            path: path("tools/gate"),
            lines: lines(12, 14)
        })
    );
    assert_eq!(findings[0].severity, Severity::High);
    assert_eq!(findings[0].title, "step failures are masked");
    // A location outside the repo, or none, is kept unlocated rather than dropped.
    assert_eq!(findings[1].anchor, None);
    assert_eq!(
        (findings[2].anchor.as_ref(), findings[2].severity),
        (None, Severity::Nit)
    );
}

#[test]
fn an_answer_without_the_contract_is_kept_whole() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    let text = "Looks fine to me, but I could not run the tests.";
    review(&mut board, &mut feed, 5, '1', text);
    let (_, r) = board.reviews().next().unwrap();
    match &r.output {
        ReviewOutput::Unstructured { text: kept, .. } => assert_eq!(kept, text),
        other => panic!("{other:?}"),
    }

    // A malformed block is not half-read either.
    let broken = "```json\n[{\"path\": \"a\", \"severity\": \"high\"\n```";
    review(&mut board, &mut feed, 6, '1', broken);
    let r = board.reviews().nth(1).unwrap().1;
    assert!(matches!(r.output, ReviewOutput::Unstructured { .. }));
}

#[test]
fn a_review_that_exits_without_an_answer_failed() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    feed.authored(&mut board, T0, Author::Human, review_requested(5, '1'));
    let finished = Event::RunFinished {
        run: run(5),
        attempt: Attempt(0),
        outcome: RunOutcome::Exited { code: 1 },
    };
    feed.authored(&mut board, T0 + 1, Author::Host, finished);
    let (_, r) = board.reviews().next().unwrap();
    assert!(
        matches!(r.output, ReviewOutput::Failed { .. }),
        "{:?}",
        r.output
    );
}

#[test]
fn the_review_gate_follows_the_head_and_open_findings() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    let head = |b: &ReviewBoard| b.head().map(|h| h.snapshot.tree.clone());
    assert_eq!(board.gate(None), GateState::NotConfigured);
    snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    assert_eq!(board.gate(head(&board).as_ref()), GateState::NotConfigured);

    feed.authored(
        &mut board,
        T0 + 20_000,
        Author::Human,
        review_requested(5, '1'),
    );
    assert_eq!(board.gate(head(&board).as_ref()), GateState::Pending);
    let result = AgentWhat::Result {
        text: FINDINGS.into(),
        is_error: false,
    };
    let (fact, _) = feed.agent(&mut board, &headless(run(5)), T0 + 20_001, result);
    // Only the high finding blocks; low and nit do not.
    assert_eq!(
        board.gate(head(&board).as_ref()),
        GateState::Red { open: 1 }
    );

    let high = FindingId { fact, index: 0 };
    let dismiss = |dismissed| Event::FindingDismissed {
        finding: high.clone(),
        dismissed,
    };
    feed.authored(&mut board, T0 + 20_010, Author::Human, dismiss(true));
    assert!(board.is_dismissed(&high));
    assert_eq!(board.gate(head(&board).as_ref()), GateState::Green);
    feed.authored(&mut board, T0 + 20_011, Author::Human, dismiss(false));
    assert_eq!(
        board.gate(head(&board).as_ref()),
        GateState::Red { open: 1 }
    );

    // A sent, unresolved thread blocks too; a promoted finding is counted once, as its thread.
    let promote = Event::FindingPromoted {
        finding: high.clone(),
        thread: ThreadId(Id128(9)),
        comment: CommentId(Id128(900)),
    };
    feed.authored(&mut board, T0 + 20_012, Author::Human, promote);
    let (_, t) = board.threads().next().unwrap();
    assert_eq!(t.origin, ThreadOrigin::Promoted(high.clone()));
    assert_eq!(
        t.comments[0].body,
        "step failures are masked\n\nset -e is off inside the loop"
    );
    // Still a draft: the agent has not been told, so it does not block yet.
    assert_eq!(board.gate(head(&board).as_ref()), GateState::Green);
    decide(&mut board, &mut feed, 1, '1', Decision::RequestChanges);
    assert_eq!(
        board.gate(head(&board).as_ref()),
        GateState::Red { open: 1 }
    );
    let resolve = Event::ThreadResolved {
        thread: ThreadId(Id128(9)),
        resolved: true,
    };
    feed.authored(&mut board, T0 + 60_000, Author::Human, resolve);
    assert_eq!(board.gate(head(&board).as_ref()), GateState::Green);

    // A new head makes the old review stale.
    snapshot(&mut board, &mut feed, 2, oid('2'), Some(oid('1')), vec![]);
    assert_eq!(board.gate(head(&board).as_ref()), GateState::Stale);
}

#[test]
fn request_changes_sends_exactly_the_drafts_it_previewed() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    let code = Anchor::Code {
        tree: oid('1'),
        path: path("src/a.rs"),
        lines: lines(10, 12),
    };
    thread(&mut board, &mut feed, 1, code, "check the bounds");
    let reply = Event::Replied {
        comment: CommentId(Id128(101)),
        thread: ThreadId(Id128(1)),
        body: "and the error path".into(),
    };
    feed.authored(&mut board, T0 + 30_010, Author::Human, reply);
    thread(
        &mut board,
        &mut feed,
        2,
        Anchor::Task(argus_core::ids::TaskKey(Id128(3))),
        "split this",
    );
    thread(
        &mut board,
        &mut feed,
        3,
        Anchor::Task(argus_core::ids::TaskKey(Id128(4))),
        "done already",
    );
    let resolve = Event::ThreadResolved {
        thread: ThreadId(Id128(3)),
        resolved: true,
    };
    feed.authored(&mut board, T0 + 30_020, Author::Human, resolve);

    let summarize = |items: Vec<argus_core::views::packet::FeedbackItem<'_>>| {
        items
            .into_iter()
            .map(|i| {
                (
                    i.at.map(|(p, l)| (p.clone(), l)),
                    i.body.to_owned(),
                    i.from_finding,
                )
            })
            .collect::<Vec<_>>()
    };
    let preview = summarize(board.feedback(None));
    assert_eq!(
        preview,
        vec![
            (
                Some((path("src/a.rs"), lines(10, 12))),
                "check the bounds".to_owned(),
                false
            ),
            (
                Some((path("src/a.rs"), lines(10, 12))),
                "and the error path".to_owned(),
                false
            ),
            (None, "split this".to_owned(), false),
        ]
    );
    decide(&mut board, &mut feed, 7, '1', Decision::RequestChanges);
    assert_eq!(
        summarize(board.feedback(Some(DecisionId(Id128(7))))),
        preview
    );
    // Nothing is left to send twice.
    assert!(board.feedback(None).is_empty());
}

#[test]
fn a_thread_follows_its_lines_through_later_snapshots_until_they_change() {
    let mut board = ReviewBoard::default();
    let mut feed = Feed::default();
    snapshot(&mut board, &mut feed, 1, oid('1'), None, vec![]);
    let code = Anchor::Code {
        tree: oid('1'),
        path: path("src/a.rs"),
        lines: lines(10, 12),
    };
    thread(&mut board, &mut feed, 1, code, "check the bounds");
    let at = |b: &ReviewBoard| b.feedback(None)[0].at.map(|(_, l)| l);

    // Three lines inserted at the top, a line replaced below, and another file edited.
    snapshot(
        &mut board,
        &mut feed,
        2,
        oid('2'),
        Some(oid('1')),
        vec![
            ("src/a.rs", vec![hunk(0, 0, 1, 3), hunk(40, 1, 43, 1)]),
            ("src/b.rs", vec![hunk(1, 20, 1, 0)]),
        ],
    );
    assert_eq!(at(&board), Some(lines(13, 15)));

    // A deletion of two lines above, then an edit next to (not inside) the range.
    snapshot(
        &mut board,
        &mut feed,
        3,
        oid('3'),
        Some(oid('2')),
        vec![("src/a.rs", vec![hunk(2, 2, 1, 0), hunk(16, 1, 14, 1)])],
    );
    assert_eq!(at(&board), Some(lines(11, 13)));

    // An edit of the range's last line outdates the thread.
    snapshot(
        &mut board,
        &mut feed,
        4,
        oid('4'),
        Some(oid('3')),
        vec![("src/a.rs", vec![hunk(13, 1, 13, 2)])],
    );
    assert_eq!(at(&board), None);
}
