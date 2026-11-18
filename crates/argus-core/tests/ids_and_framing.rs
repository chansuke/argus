//! Ids that must be stable across restarts, and the framer that cuts foreign byte streams
//! into the frames a source log stores.

use std::path::PathBuf;

use argus_core::ids::{Attempt, Id128, RunId};
use argus_core::source::{RawFrame, SourceKind, SourceMeta, SourceOrigin};

#[test]
fn minted_ids_are_distinct_v4_uuids() {
    let ids: Vec<Id128> = (0..1000).map(|_| Id128::mint()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
    for id in &ids {
        let text = id.to_string();
        assert_eq!(text.len(), 36, "{text}");
        assert_eq!(&text[14..15], "4", "version nibble of {text}");
        assert!(
            matches!(&text[19..20], "8" | "9" | "a" | "b"),
            "variant of {text}"
        );
        assert_eq!(Id128::parse(&text), Some(*id));
    }
}

#[test]
fn derived_ids_depend_only_on_their_input() {
    let a = Id128::derive("review", b"request-1/opus");
    assert_eq!(a, Id128::derive("review", b"request-1/opus"));
    assert_ne!(a, Id128::derive("review", b"request-1/sonnet"));
    assert_ne!(a, Id128::derive("snapshot", b"request-1/opus"));
    // The namespace and the key are separated, so moving a byte across them changes the id.
    assert_ne!(Id128::derive("ab", b"c"), Id128::derive("a", b"bc"));
    // Derived ids are UUIDs too (version 8, "custom"), so they work as `--session-id`.
    let text = a.to_string();
    assert_eq!(&text[14..15], "8");
    assert_eq!(Id128::parse(&text), Some(a));
}

#[test]
fn uuid_text_is_strict() {
    let id = Id128::parse("0f8fad5d-d9cb-469f-a165-70867728950e").unwrap();
    assert_eq!(id.to_string(), "0f8fad5d-d9cb-469f-a165-70867728950e");
    assert_eq!(
        Id128::parse("0F8FAD5D-D9CB-469F-A165-70867728950E"),
        Some(id)
    );
    for bad in [
        "",
        "0f8fad5dd9cb469fa16570867728950e",
        "0f8fad5d-d9cb-469f-a165-70867728950",
        "0f8fad5d-d9cb-469f-a165-70867728950ea",
        "0f8fad5d_d9cb-469f-a165-70867728950e",
        "0f8fad5d-d9cb-469f-a165-70867728950g",
    ] {
        assert_eq!(Id128::parse(bad), None, "{bad}");
    }
}

#[test]
fn a_source_id_is_the_same_after_a_restart() {
    let run = RunId(Id128::derive("t", b"run"));
    let transcript = SourceOrigin::File(PathBuf::from("/home/u/.claude/projects/x/abc.jsonl"));
    let id = SourceMeta::derive_id(run, Attempt(0), &transcript);
    assert_eq!(id, SourceMeta::derive_id(run, Attempt(0), &transcript));
    assert_ne!(id, SourceMeta::derive_id(run, Attempt(1), &transcript));
    assert_ne!(
        id,
        SourceMeta::derive_id(run, Attempt(0), &SourceOrigin::Stdout)
    );
    assert_ne!(
        SourceMeta::derive_id(run, Attempt(0), &SourceOrigin::Stdout),
        SourceMeta::derive_id(run, Attempt(0), &SourceOrigin::Stderr)
    );
    let other = SourceOrigin::File(PathBuf::from("/home/u/.claude/projects/x/abd.jsonl"));
    assert_ne!(id, SourceMeta::derive_id(run, Attempt(0), &other));
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Frames as `(offset, bytes)`; the framer never emits a discontinuity.
fn frame_all(kind: SourceKind, resume_at: u64, chunks: &[&[u8]]) -> Vec<(u64, Vec<u8>)> {
    let mut framer = kind.framer(resume_at);
    let mut frames: Vec<RawFrame> = chunks.iter().flat_map(|c| framer.push(c)).collect();
    frames.extend(framer.finish());
    frames
        .into_iter()
        .map(|f| match f {
            RawFrame::Bytes { offset, bytes } => (offset, bytes),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

fn split_randomly<'a>(rng: &mut Rng, stream: &'a [u8]) -> Vec<&'a [u8]> {
    let mut chunks = Vec::new();
    let mut rest = stream;
    while !rest.is_empty() {
        let n = 1 + rng.below(rest.len().min(9) as u64) as usize;
        let (head, tail) = rest.split_at(n);
        chunks.push(head);
        rest = tail;
    }
    chunks
}

/// However the bytes arrive, the frames are the same: one per line, newline included,
/// covering the stream exactly with contiguous offsets from the resume point.
#[test]
fn lines_do_not_depend_on_how_the_bytes_arrive() {
    let mut rng = Rng(11);
    for _ in 0..300 {
        let stream: Vec<u8> = (0..rng.below(60))
            .map(|_| [b'a', b'{', b'"', b'\n', b'\r', 0xE3][rng.below(6) as usize])
            .collect();
        let whole = frame_all(SourceKind::ClaudeTranscript, 100, &[&stream]);
        let chunks = split_randomly(&mut rng, &stream);
        assert_eq!(frame_all(SourceKind::ClaudeTranscript, 100, &chunks), whole);

        let mut next = 100;
        for (i, (offset, bytes)) in whole.iter().enumerate() {
            assert_eq!(*offset, next);
            assert!(!bytes.is_empty());
            let last = i == whole.len() - 1;
            let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
            if last && !stream.ends_with(b"\n") {
                assert_eq!(newlines, 0);
            } else {
                assert_eq!(newlines, 1);
                assert!(bytes.ends_with(b"\n"));
            }
            next += bytes.len() as u64;
        }
        let joined: Vec<u8> = whole.iter().flat_map(|(_, b)| b.clone()).collect();
        assert_eq!(joined, stream);
    }
}

#[test]
fn a_line_still_being_written_is_held_back() {
    let mut framer = SourceKind::ClaudeTranscript.framer(0);
    assert_eq!(
        framer.push(b"{\"a\":1}\n{\"b\""),
        vec![RawFrame::Bytes {
            offset: 0,
            bytes: b"{\"a\":1}\n".to_vec()
        }]
    );
    assert_eq!(framer.push(b":2}"), vec![]);
    assert_eq!(
        framer.push(b"\n"),
        vec![RawFrame::Bytes {
            offset: 8,
            bytes: b"{\"b\":2}\n".to_vec()
        }]
    );
    assert_eq!(framer.finish(), None);
}

#[test]
fn terminal_bytes_are_kept_as_they_arrive() {
    let frames = frame_all(SourceKind::Terminal, 0, &[b"\x1b[31mred", b"", b"\n$ "]);
    assert_eq!(
        frames,
        vec![(0, b"\x1b[31mred".to_vec()), (8, b"\n$ ".to_vec())]
    );
}

/// TLC `-tool` output: a STARTMSG..ENDMSG block is one frame, other lines stand alone.
#[test]
fn tlc_messages_are_framed_as_blocks() {
    let out: &[u8] = b"stray\n@!@!@STARTMSG 2185:0 @!@!@\nStarting...\n@!@!@ENDMSG 2185 @!@!@\n@!@!@STARTMSG 2110:1 @!@!@\nInvariant TypeOK is violated.\nmore\n@!@!@ENDMSG 2110 @!@!@\ntail";
    let expected: Vec<&[u8]> = vec![
        b"stray\n",
        b"@!@!@STARTMSG 2185:0 @!@!@\nStarting...\n@!@!@ENDMSG 2185 @!@!@\n",
        b"@!@!@STARTMSG 2110:1 @!@!@\nInvariant TypeOK is violated.\nmore\n@!@!@ENDMSG 2110 @!@!@\n",
        b"tail",
    ];
    let mut rng = Rng(12);
    for _ in 0..50 {
        let chunks = split_randomly(&mut rng, out);
        let frames = frame_all(SourceKind::TlcTool, 0, &chunks);
        let bytes: Vec<&[u8]> = frames.iter().map(|(_, b)| b.as_slice()).collect();
        assert_eq!(bytes, expected);
    }
}

/// `lake build` text: a diagnostic and the goal lines under it are one frame; lake's own
/// progress lines stand alone. Observed with lake 4.35.
#[test]
fn lean_diagnostics_are_framed_with_their_continuation_lines() {
    let out: &[u8] = "✖ [4/6] Building P.Broken (390ms)\nerror: P/Broken.lean:1:40: unsolved goals\nn : Nat\n⊢ False\nwarning: P/Holey.lean:1:8: declaration uses `sorry`\nSome required targets logged failures:\n- P.Broken\n".as_bytes();
    let expected: Vec<&[u8]> = vec![
        "✖ [4/6] Building P.Broken (390ms)\n".as_bytes(),
        "error: P/Broken.lean:1:40: unsolved goals\nn : Nat\n⊢ False\n".as_bytes(),
        b"warning: P/Holey.lean:1:8: declaration uses `sorry`\n",
        b"Some required targets logged failures:\n- P.Broken\n",
    ];
    let mut rng = Rng(13);
    for _ in 0..50 {
        let chunks = split_randomly(&mut rng, out);
        let frames = frame_all(SourceKind::LeanText, 7, &chunks);
        let bytes: Vec<&[u8]> = frames.iter().map(|(_, b)| b.as_slice()).collect();
        assert_eq!(bytes, expected);
        assert_eq!(frames[0].0, 7);
    }
}

#[test]
fn rfc3339_timestamps_parse_to_unix_millis() {
    use argus_core::ids::UnixMillis;
    let p = |s| UnixMillis::parse_rfc3339(s).map(|m| m.0);
    assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(p("2026-10-24T03:16:30.255Z"), Some(1_792_811_790_255));
    assert_eq!(p("2026-10-24T12:16:30.255+09:00"), Some(1_792_811_790_255));
    assert_eq!(p("2026-10-24t03:16:30.255123z"), Some(1_792_811_790_255));
    assert_eq!(p("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
    assert_eq!(p("1969-12-31T23:59:59.999Z"), Some(-1));
    for bad in [
        "",
        "2026-10-24",
        "2026-13-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-10-24T03:16:30",
        "2026-10-24T25:00:00Z",
        "2026-10-24T03:16:30.Z",
    ] {
        assert_eq!(p(bad), None, "{bad}");
    }
}
