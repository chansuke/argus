//! The unified-diff parser and the hunk anchor, on real `git diff` output (a modified
//! file, a new file, a deleted file, a binary file, and a file without a final newline).

use argus_core::diff::{DiffHunk, line_hash, parse_unified};
use argus_core::event::Hunk;
use argus_core::ids::LineRange;

fn head_diff() -> Vec<DiffHunk> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/git/head.diff");
    parse_unified(&std::fs::read_to_string(path).unwrap())
}

fn header(old_start: u32, old_len: u32, new_start: u32, new_len: u32) -> Hunk {
    Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    }
}

#[test]
fn every_text_hunk_is_read_and_binary_files_have_none() {
    let hunks = head_diff();
    let summary: Vec<_> = hunks
        .iter()
        .map(|h| {
            (
                h.anchor.path.as_str(),
                h.header,
                h.new_lines,
                h.removed_count,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("src/a.rs", header(1, 5, 1, 5), LineRange::new(1, 5), 1),
            ("src/a.rs", header(8, 5, 8, 6), LineRange::new(8, 13), 0),
            ("src/new.rs", header(0, 0, 1, 1), LineRange::new(1, 1), 0),
            // A deleted file is named by its old path and has no new lines.
            ("tools/old.sh", header(1, 2, 0, 0), None, 2),
            (
                "tools/tail.txt",
                header(1, 1, 1, 1),
                LineRange::new(1, 1),
                1
            ),
        ]
    );
}

#[test]
fn added_lines_are_hashed_trimmed_and_markers_are_not_lines() {
    let hunks = head_diff();
    assert_eq!(hunks[0].added, vec![line_hash("line2 changed")]);
    assert_eq!(hunks[1].added, vec![line_hash("inserted")]);
    assert_eq!(line_hash("  inserted  "), line_hash("inserted"));
    // `\ No newline at end of file` is neither added nor removed.
    assert_eq!(hunks[4].added, vec![line_hash("no newline, edited")]);
    assert!(
        hunks[0]
            .text
            .starts_with("@@ -1,5 +1,5 @@\n line1\n-line2\n+line2 changed\n")
    );
    assert!(hunks[4].text.ends_with("\\ No newline at end of file\n"));
}

#[test]
fn an_anchor_depends_on_content_not_position() {
    let at = |start: u32, minus: &str, plus: &str| {
        let text = format!(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -{start},1 +{start},1 @@\n-{minus}\n+{plus}\n"
        );
        parse_unified(&text).remove(0).anchor
    };
    assert_eq!(at(3, "x", "y"), at(40, "x", "y"));
    assert_ne!(at(3, "x", "y"), at(3, "x", "z"));
    // Which side a line is on matters.
    assert_ne!(at(3, "x", "y"), at(3, "y", "x"));
    let other = parse_unified("diff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -3,1 +3,1 @@\n-x\n+y\n");
    assert_ne!(other[0].anchor, at(3, "x", "y"));
}
