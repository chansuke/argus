//! Behaviour of argus-log seen from outside: what survives a crash, what a reader may
//! return, and that replaying the logs reproduces the order the live host applied.
//!
//! The randomized tests use a fixed-seed generator, so a failure always reproduces.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_log::{
    ActorKey, Admit, Clock, Cursor, EventId, Frame, LogDir, LogError, LogReader, LogWriter, Seq,
    Stamp, merge_by_stamp,
};

/// splitmix64: small, deterministic, good enough to pick sizes and actors.
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
    fn bytes(&mut self, max_len: u64) -> Vec<u8> {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn scratch(name: &str) -> LogDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argus-log-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    LogDir::new(dir)
}

fn actor(key: &str) -> ActorKey {
    ActorKey::new(key).unwrap()
}

fn log_file(dir: &LogDir, actor: &ActorKey) -> PathBuf {
    dir.root().join(format!("{}.alog", actor.as_str()))
}

fn read_all(reader: &LogReader) -> Vec<Frame> {
    reader
        .iter_from(Seq::FIRST)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A log with random payloads, plus the file length after the header and after each
/// frame. The boundaries come from the file itself, not from the frame layout.
struct Written {
    bytes: Vec<u8>,
    header_end: u64,
    frame_ends: Vec<u64>,
    payloads: Vec<Vec<u8>>,
    stamps: Vec<Stamp>,
}

const HEADER: &[u8] = b"source=claude-code;v=1";

fn write_sample(rng: &mut Rng, frames: usize) -> Written {
    let dir = scratch("sample");
    let a = actor("agent-1");
    let mut clock = Clock::starting_after(None);
    let (mut w, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
    assert_eq!(rec.truncated_bytes, 0);
    assert!(rec.last.is_none());
    let path = log_file(&dir, &a);
    let header_end = fs::metadata(&path).unwrap().len();
    let mut frame_ends = Vec::new();
    let mut payloads = Vec::new();
    let mut stamps = Vec::new();
    for i in 0..frames {
        let payload = rng.bytes(40);
        let stamp = clock.tick();
        assert_eq!(w.append(stamp, &payload).unwrap().get(), i as u64);
        frame_ends.push(fs::metadata(&path).unwrap().len());
        payloads.push(payload);
        stamps.push(stamp);
    }
    w.sync().unwrap();
    drop(w);
    Written {
        bytes: fs::read(&path).unwrap(),
        header_end,
        frame_ends,
        payloads,
        stamps,
    }
}

#[test]
fn appended_frames_read_back_in_order() {
    let mut rng = Rng(1);
    let s = write_sample(&mut rng, 20);
    let dir = scratch("roundtrip");
    let a = actor("agent-1");
    fs::write(log_file(&dir, &a), &s.bytes).unwrap();

    let reader = LogReader::open(&dir, &a).unwrap();
    assert_eq!(reader.header(), HEADER);
    assert_eq!(reader.len(), 20);
    assert_eq!(reader.last_stamp(), s.stamps.last().copied());
    let frames = read_all(&reader);
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.id.actor, a);
        assert_eq!(f.id.seq.get(), i as u64);
        assert_eq!(f.payload, s.payloads[i]);
        assert_eq!(f.stamp, s.stamps[i]);
        let by_id = reader.get(f.id.seq).unwrap();
        assert_eq!(by_id.payload, s.payloads[i]);
    }
    let from_7: Vec<Frame> = reader
        .iter_from(frames[7].id.seq)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(from_7.len(), 13);
    assert_eq!(from_7[0].payload, s.payloads[7]);
}

/// A crash can stop the file at any byte. Whatever the cut, a reader returns exactly the
/// frames that were whole, the writer cuts the rest off, and appending works afterwards.
#[test]
fn crash_at_any_byte_keeps_every_whole_frame() {
    let mut rng = Rng(2);
    let s = write_sample(&mut rng, 6);
    let a = actor("agent-1");
    for cut in 0..=s.bytes.len() as u64 {
        let dir = scratch("cut");
        fs::write(log_file(&dir, &a), &s.bytes[..cut as usize]).unwrap();
        let whole = s.frame_ends.iter().filter(|&&end| end <= cut).count();

        if cut >= s.header_end {
            let reader = LogReader::open(&dir, &a).unwrap();
            let frames = read_all(&reader);
            assert_eq!(frames.len(), whole, "cut at {cut}");
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(f.payload, s.payloads[i], "cut at {cut}");
            }
        }

        let (mut w, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
        let good_end = if cut < s.header_end {
            // A header that never finished writing: the log held nothing yet.
            0
        } else if whole == 0 {
            s.header_end
        } else {
            s.frame_ends[whole - 1]
        };
        assert_eq!(rec.truncated_bytes, cut - good_end, "cut at {cut}");
        assert_eq!(
            rec.last.map(|(seq, st)| (seq.get(), st)),
            whole.checked_sub(1).map(|i| (i as u64, s.stamps[i])),
            "cut at {cut}"
        );
        assert_eq!(w.next_seq().get(), whole as u64);

        let stamp = Clock::starting_after(rec.last.map(|(_, st)| st)).tick();
        w.append(stamp, b"after").unwrap();
        w.sync().unwrap();
        drop(w);
        let frames = read_all(&LogReader::open(&dir, &a).unwrap());
        assert_eq!(frames.len(), whole + 1, "cut at {cut}");
        assert_eq!(frames[whole].payload, b"after");
        fs::remove_dir_all(dir.root()).unwrap();
    }
}

/// A damaged byte anywhere in a frame ends the trusted prefix at that frame. A reader
/// never returns a frame whose bytes changed.
#[test]
fn a_damaged_frame_and_everything_after_it_is_not_read() {
    let mut rng = Rng(3);
    let s = write_sample(&mut rng, 5);
    let a = actor("agent-1");
    for pos in s.header_end..s.bytes.len() as u64 {
        let dir = scratch("flip");
        let mut bytes = s.bytes.clone();
        bytes[pos as usize] ^= 0xFF;
        fs::write(log_file(&dir, &a), &bytes).unwrap();
        let damaged = s.frame_ends.iter().filter(|&&end| end <= pos).count();

        let frames = read_all(&LogReader::open(&dir, &a).unwrap());
        assert_eq!(frames.len(), damaged, "byte {pos}");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.payload, s.payloads[i], "byte {pos}");
        }
        fs::remove_dir_all(dir.root()).unwrap();
    }
}

/// One clock stamps every frame of a ledger, so merging the per-actor logs by stamp gives
/// back the order in which the live host appended (and applied) them.
#[test]
fn replay_equals_live_order() {
    let mut rng = Rng(4);
    let dir = scratch("replay");
    let actors = [actor("agent-1"), actor("host"), actor("review-2")];
    let mut writers: Vec<LogWriter> = actors
        .iter()
        .map(|a| LogWriter::open(&dir, a, HEADER).unwrap().0)
        .collect();
    let mut clock = Clock::starting_after(None);
    let mut live = Vec::new();
    for _ in 0..300 {
        let i = rng.below(actors.len() as u64) as usize;
        let payload = rng.bytes(24);
        let seq = writers[i].append(clock.tick(), &payload).unwrap();
        live.push((
            EventId {
                actor: actors[i].clone(),
                seq,
            },
            payload,
        ));
    }
    for w in &mut writers {
        w.sync().unwrap();
    }

    // Readers work while the writers still hold their locks.
    assert_eq!(dir.actors().unwrap(), actors.to_vec());
    let iters = actors
        .iter()
        .map(|a| {
            LogReader::open(&dir, a)
                .unwrap()
                .iter_from(Seq::FIRST)
                .unwrap()
        })
        .collect();
    let replayed: Vec<(EventId, Vec<u8>)> = merge_by_stamp(iters)
        .map(|f| f.map(|f| (f.id, f.payload)))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(replayed, live);
}

/// The Rust side of `replay_duplicated` in proofs/ArgusProofs/Replay.lean: delivering
/// the stream again, or from an earlier checkpoint, applies nothing twice.
#[test]
fn cursor_applies_each_frame_once() {
    let mut rng = Rng(5);
    let actors = [actor("a"), actor("b")];
    let mut next = [0u64; 2];
    let stream: Vec<EventId> = (0..100)
        .map(|_| {
            let i = rng.below(2) as usize;
            next[i] += 1;
            EventId::parse(&format!("{}:{}", actors[i].as_str(), next[i] - 1)).unwrap()
        })
        .collect();

    let mut live = Cursor::default();
    let applied: Vec<&EventId> = stream
        .iter()
        .filter(|id| live.admit(id) == Admit::Fresh)
        .collect();
    assert_eq!(applied.len(), stream.len());

    let checkpoint_at = 40;
    let mut cursor = Cursor::default();
    for id in &stream[..checkpoint_at] {
        assert_eq!(cursor.admit(id), Admit::Fresh);
    }
    let mut again = Vec::new();
    for id in stream.iter().chain(&stream) {
        match cursor.admit(id) {
            Admit::Fresh => again.push(id),
            Admit::Seen => {}
            Admit::Gap { expected } => panic!("gap at {id}, expected {expected:?}"),
        }
    }
    assert_eq!(again, stream[checkpoint_at..].iter().collect::<Vec<_>>());
    assert_eq!(cursor, live);
}

#[test]
fn cursor_reports_a_skipped_frame() {
    let mut cursor = Cursor::default();
    let a = actor("a");
    let second = EventId {
        actor: a.clone(),
        seq: Seq::FIRST.next(),
    };
    assert_eq!(
        cursor.admit(&second),
        Admit::Gap {
            expected: Seq::FIRST
        }
    );
    assert_eq!(cursor.next_for(&a), Seq::FIRST);
}

#[test]
fn a_reader_sees_frames_appended_after_it_opened() {
    let dir = scratch("refresh");
    let a = actor("agent-1");
    let mut clock = Clock::starting_after(None);
    let (mut w, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    w.append(clock.tick(), b"one").unwrap();
    w.append(clock.tick(), b"two").unwrap();
    let mut reader = LogReader::open(&dir, &a).unwrap();
    assert_eq!(reader.len(), 2);
    assert_eq!(reader.refresh().unwrap(), 0);

    for p in [&b"three"[..], b"four", b"five"] {
        w.append(clock.tick(), p).unwrap();
    }
    assert_eq!(reader.refresh().unwrap(), 3);
    assert_eq!(reader.len(), 5);
    let fifth = EventId::parse("agent-1:4").unwrap();
    assert_eq!(reader.get(fifth.seq).unwrap().payload, b"five");
}

#[test]
fn a_second_writer_is_refused() {
    let dir = scratch("lock");
    let a = actor("agent-1");
    let (first, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    assert!(matches!(
        LogWriter::open(&dir, &a, HEADER),
        Err(LogError::Locked(_))
    ));
    drop(first);
    LogWriter::open(&dir, &a, HEADER).unwrap();
}

#[test]
fn reopening_with_another_header_is_refused() {
    let dir = scratch("header");
    let a = actor("agent-1");
    drop(LogWriter::open(&dir, &a, HEADER).unwrap());
    assert!(matches!(
        LogWriter::open(&dir, &a, b"source=codex;v=1"),
        Err(LogError::HeaderMismatch(_))
    ));
}

#[test]
fn a_file_that_is_not_a_log_is_refused() {
    let dir = scratch("notalog");
    let a = actor("agent-1");
    fs::write(
        log_file(&dir, &a),
        b"{\"type\":\"user\",\"message\":\"hello\"}\n",
    )
    .unwrap();
    assert!(matches!(
        LogReader::open(&dir, &a),
        Err(LogError::NotALog(_))
    ));
    assert!(matches!(
        LogWriter::open(&dir, &a, HEADER),
        Err(LogError::NotALog(_))
    ));
}

#[test]
fn stamps_must_increase() {
    let dir = scratch("stamps");
    let (mut w, _) = LogWriter::open(&dir, &actor("agent-1"), HEADER).unwrap();
    let stamp = Clock::starting_after(None).tick();
    w.append(stamp, b"x").unwrap();
    assert!(matches!(
        w.append(stamp, b"y"),
        Err(LogError::NonMonotonicStamp { .. })
    ));
    assert_eq!(w.next_seq().get(), 1);
}

#[test]
fn clock_never_repeats_a_stamp_across_restarts() {
    let far_future = Stamp {
        wall_ns: u64::MAX - 1,
        logical: 0,
    };
    let mut clock = Clock::starting_after(Some(far_future));
    let a = clock.tick();
    let b = clock.tick();
    assert!(far_future < a && a < b);
}

#[test]
fn a_reference_past_the_end_is_not_found() {
    let dir = scratch("notfound");
    let a = actor("agent-1");
    let (mut w, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    w.append(Clock::starting_after(None).tick(), b"x").unwrap();
    let reader = LogReader::open(&dir, &a).unwrap();
    let id = EventId::parse("agent-1:1").unwrap();
    assert!(matches!(reader.get(id.seq), Err(LogError::NotFound(missing)) if missing == id));
}

#[test]
fn event_ids_round_trip_through_text() {
    let id = EventId::parse("review-2:42").unwrap();
    assert_eq!(id.actor.as_str(), "review-2");
    assert_eq!(id.seq.get(), 42);
    assert_eq!(id.to_string(), "review-2:42");
    assert!(EventId::parse("Review:1").is_none());
    assert!(EventId::parse("review").is_none());
}

#[test]
fn actors_lists_only_logs() {
    let dir = scratch("actors");
    drop(LogWriter::open(&dir, &actor("b"), HEADER).unwrap());
    drop(LogWriter::open(&dir, &actor("a"), HEADER).unwrap());
    fs::write(dir.root().join("notes.txt"), b"").unwrap();
    fs::write(dir.root().join("Not-A-Key.alog"), b"").unwrap();
    assert_eq!(dir.actors().unwrap(), vec![actor("a"), actor("b")]);
}

#[test]
fn blobs_are_named_by_their_sha256() {
    let dir = scratch("blobs");
    let store = dir.blobs();
    let cases: [(&[u8], &str); 3] = [
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ];
    for (i, (bytes, hex)) in cases.iter().enumerate() {
        let src = dir.root().join(format!("src-{i}"));
        fs::write(&src, bytes).unwrap();
        let (id, len) = store.put_file(&src).unwrap();
        let got: String = id.0.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, *hex);
        assert_eq!(len, bytes.len() as u64);
        assert_eq!(fs::read(store.path(&id)).unwrap(), *bytes);
        assert_eq!(store.put_file(&src).unwrap(), (id, len));
    }

    // Longer than one copy buffer, so hashing has to carry state across reads.
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7) as u8).collect();
    let src = dir.root().join("big");
    fs::write(&src, &big).unwrap();
    let (id, _) = store.put_file(&src).unwrap();
    assert_eq!(fs::read(store.path(&id)).unwrap(), big);
}
