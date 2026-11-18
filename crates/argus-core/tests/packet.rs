//! The review packet's pure parts: where each hunk came from, which hunks were already
//! seen, and the agenda order.

use argus_core::diff::{DiffHunk, line_hash, parse_unified};
use argus_core::ids::{EventId, FactId, Id128, RepoPath, RunId, Stamp};
use argus_core::views::packet::{MIN_COVERAGE_PERMILLE, Provenance, provenance};
use argus_core::views::transcript::{EditHow, EditTrace};

fn fact(n: u64) -> FactId {
    FactId {
        event: EventId::parse(&format!("src-0f8fad5d-d9cb-469f-a165-70867728950e:{n}")).unwrap(),
        part: 0,
    }
}

fn edit(n: u64, path: &str, how: EditHow) -> EditTrace {
    EditTrace {
        fact: fact(n),
        stamp: Stamp {
            wall_ns: n,
            logical: 0,
        },
        run: RunId(Id128(1)),
        path: RepoPath::parse(path).unwrap(),
        how,
    }
}

fn inserted(lines: &[&str]) -> EditHow {
    EditHow::Structured {
        inserted: lines.iter().map(|l| line_hash(l)).collect(),
    }
}

/// One hunk of `path` adding `added` and removing one line.
fn hunk(path: &str, added: &[&str]) -> DiffHunk {
    let mut text = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,{} @@\n-old\n",
        added.len()
    );
    for line in added {
        text.push_str(&format!("+{line}\n"));
    }
    parse_unified(&text).remove(0)
}

#[test]
fn structured_edits_that_inserted_the_lines_explain_a_hunk() {
    let h = hunk("src/a.rs", &["let x = 1;", "let y = 2;"]);
    let edits = [
        edit(1, "src/a.rs", inserted(&["  let x = 1;"])),
        // Same file, but none of this hunk's lines: not a source of it.
        edit(2, "src/a.rs", inserted(&["unrelated"])),
        edit(3, "src/b.rs", inserted(&["let y = 2;"])),
        edit(4, "src/a.rs", inserted(&["let y = 2;"])),
    ];
    assert_eq!(
        provenance(&h, &edits),
        Provenance::Direct {
            facts: vec![fact(1), fact(4)],
            coverage_permille: 1000
        }
    );
}

#[test]
fn a_shell_command_explains_what_structured_edits_do_not() {
    let h = hunk("src/a.rs", &["a", "b", "c"]);
    let edits = [
        edit(1, "src/a.rs", inserted(&["a"])),
        edit(2, "src/a.rs", EditHow::Shell),
    ];
    assert!(333 < MIN_COVERAGE_PERMILLE);
    assert_eq!(
        provenance(&h, &edits),
        Provenance::ViaShell {
            facts: vec![fact(2)]
        }
    );
    // Without the shell command, the partial explanation is still shown, with its coverage.
    assert_eq!(
        provenance(&h, &edits[..1]),
        Provenance::Direct {
            facts: vec![fact(1)],
            coverage_permille: 333
        }
    );
}

#[test]
fn a_hunk_no_edit_touched_is_unexplained() {
    let h = hunk("src/a.rs", &["generated"]);
    let edits = [edit(1, "src/b.rs", inserted(&["generated"]))];
    assert_eq!(provenance(&h, &edits), Provenance::Unexplained);
    assert_eq!(provenance(&h, &[]), Provenance::Unexplained);
}

#[test]
fn a_pure_deletion_is_explained_by_any_edit_of_its_file() {
    let h = hunk("src/a.rs", &[]);
    assert!(h.added.is_empty());
    let structured = [edit(5, "src/a.rs", inserted(&[]))];
    assert_eq!(
        provenance(&h, &structured),
        Provenance::Direct {
            facts: vec![fact(5)],
            coverage_permille: 1000
        }
    );
    let shell = [edit(6, "src/a.rs", EditHow::Shell)];
    assert_eq!(
        provenance(&h, &shell),
        Provenance::ViaShell {
            facts: vec![fact(6)]
        }
    );
}

mod common;

#[test]
fn hunks_of_decided_snapshots_count_as_seen() {
    use argus_core::diff::HunkAnchor;
    use argus_core::event::{Author, Decision, Event, FileState, RunOutcome, RunPurpose, Snapshot};
    use argus_core::ids::{Attempt, DecisionId, GitOid};
    use argus_core::views::packet::seen_anchors;
    use argus_core::views::review::ReviewBoard;

    let anchor = |f: u64| HunkAnchor {
        path: RepoPath::parse("src/a.rs").unwrap(),
        fingerprint: f,
    };
    let oid = |c: char| GitOid::parse(&c.to_string().repeat(40)).unwrap();
    let mut board = ReviewBoard::default();
    let mut feed = common::Feed::default();
    let snapshot =
        |board: &mut ReviewBoard, feed: &mut common::Feed, n: u128, tree: char, anchors: &[u64]| {
            let run = RunId(Id128(n));
            let requested = Event::RunRequested {
                run,
                purpose: RunPurpose::Snapshot { after: None },
            };
            feed.authored(board, 1, Author::Policy, requested);
            let started = Event::RunStarted {
                run,
                attempt: Attempt(0),
                pid: None,
            };
            feed.authored(board, 2, Author::Host, started);
            let snapshot = Snapshot {
                tree: oid(tree),
                base: oid('b'),
                previous: None,
                delta: Vec::new(),
                files: vec![FileState {
                    path: RepoPath::parse("src/a.rs").unwrap(),
                    blob: None,
                    hunks: anchors.iter().map(|f| anchor(*f)).collect(),
                }],
                withheld: Vec::new(),
            };
            let finished = Event::RunFinished {
                run,
                attempt: Attempt(0),
                outcome: RunOutcome::Snapshot(snapshot),
            };
            feed.authored(board, 3, Author::Host, finished);
        };
    let decided = |d: u128, tree: Option<char>, verdict| Event::Decided {
        decision: DecisionId(Id128(d)),
        tree: tree.map(oid),
        verdict,
        note: String::new(),
    };

    snapshot(&mut board, &mut feed, 1, '1', &[1, 2]);
    assert!(seen_anchors(&board).is_empty());
    feed.authored(
        &mut board,
        4,
        Author::Human,
        decided(1, Some('1'), Decision::RequestChanges),
    );
    snapshot(&mut board, &mut feed, 2, '2', &[2, 3]);
    let seen: Vec<u64> = seen_anchors(&board)
        .into_iter()
        .map(|a| a.fingerprint)
        .collect();
    assert_eq!(seen, vec![1, 2]);
    feed.authored(
        &mut board,
        5,
        Author::Human,
        decided(2, Some('2'), Decision::Approve),
    );
    let seen: Vec<u64> = seen_anchors(&board)
        .into_iter()
        .map(|a| a.fingerprint)
        .collect();
    assert_eq!(seen, vec![1, 2, 3]);
}
