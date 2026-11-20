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
        // Nothing is lost: the bytes that were cut off are kept next to the log.
        match &rec.quarantined {
            Some(kept) => assert_eq!(
                fs::read(kept).unwrap(),
                &s.bytes[good_end as usize..cut as usize],
                "cut at {cut}"
            ),
            None => assert_eq!(cut, good_end, "cut at {cut}"),
        }
        assert_eq!(dir.actors().unwrap(), vec![a.clone()], "cut at {cut}");
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
/// never returns a frame whose bytes changed, and the writer moves that frame and every
/// frame after it, intact ones included, into a quarantine file before it cuts the log.
#[test]
fn a_damaged_frame_and_everything_after_it_is_set_aside() {
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

        let (_, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
        let good_end = damaged
            .checked_sub(1)
            .map_or(s.header_end, |i| s.frame_ends[i]);
        let kept = rec.quarantined.expect("the damaged tail is kept");
        assert_eq!(
            fs::read(kept).unwrap(),
            &bytes[good_end as usize..],
            "byte {pos}"
        );
        assert_eq!(fs::metadata(log_file(&dir, &a)).unwrap().len(), good_end);
        fs::remove_dir_all(dir.root()).unwrap();
    }
}

/// Two crashes that leave a torn tail at the same offset keep both tails.
#[test]
fn a_second_quarantine_does_not_overwrite_the_first() {
    let dir = scratch("twice");
    let a = actor("agent-1");
    let (mut w, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    w.append(Clock::starting_after(None).tick(), b"kept")
        .unwrap();
    drop(w);
    let path = log_file(&dir, &a);
    let mut kept = Vec::new();
    for tail in [&b"first torn tail"[..], b"second"] {
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(tail);
        fs::write(&path, &bytes).unwrap();
        let (_, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
        kept.push(rec.quarantined.unwrap());
    }
    assert_ne!(kept[0], kept[1]);
    assert_eq!(fs::read(&kept[0]).unwrap(), b"first torn tail");
    assert_eq!(fs::read(&kept[1]).unwrap(), b"second");
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
    let seqs: Vec<u64> = reader.seqs().rev().map(Seq::get).collect();
    assert_eq!(seqs, vec![4, 3, 2, 1, 0]);
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

// ---------------------------------------------------------------- found by mutation testing

/// Blob ids are content addresses: the hash must be SHA-256 exactly (FIPS 180-4 vectors).
#[test]
fn blob_ids_are_sha256_of_the_content() {
    let dir = scratch("sha");
    let blobs = dir.blobs();
    let hex = |b: &[u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let million_a = vec![b'a'; 1_000_000];
    let vectors: [(&[u8], &str); 4] = [
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
        (
            &million_a,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        ),
    ];
    for (input, want) in vectors {
        let src = dir.root().join("input");
        fs::write(&src, input).unwrap();
        let (id, len) = blobs.put_file(&src).unwrap();
        assert_eq!((hex(&id.0).as_str(), len), (want, input.len() as u64));
        assert_eq!(fs::read(blobs.path(&id)).unwrap(), input);
    }
}

/// A header cut short while the log was created is repaired; a different header is not.
#[test]
fn a_torn_header_is_repaired_only_if_it_is_a_prefix_of_ours() {
    let dir = scratch("torn-header");
    let a = actor("agent-1");
    drop(LogWriter::open(&dir, &a, HEADER).unwrap());
    let path = log_file(&dir, &a);
    let whole = fs::read(&path).unwrap();
    let torn = &whole[..whole.len() - 3];
    fs::write(&path, torn).unwrap();
    let (_, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
    assert_eq!(rec.truncated_bytes, torn.len() as u64);

    let mut other = torn.to_vec();
    let last = other.len() - 1;
    other[last] ^= 0xff;
    fs::write(&path, &other).unwrap();
    assert!(matches!(
        LogWriter::open(&dir, &a, HEADER),
        Err(LogError::HeaderMismatch(_))
    ));
}

/// A lock released a moment ago is taken; one held for good is refused after the wait.
#[test]
fn a_briefly_held_lock_is_waited_for_and_a_held_one_is_refused() {
    let dir = scratch("lock-wait");
    let a = actor("host");
    let (held, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(
        LogWriter::open(&dir, &a, HEADER),
        Err(LogError::Locked(_))
    ));
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(150),
        "{waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");

    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(held);
    });
    assert!(LogWriter::open(&dir, &a, HEADER).is_ok());
    release.join().unwrap();
}

#[test]
fn a_second_quarantine_at_the_same_offset_is_numbered() {
    let dir = scratch("numbered");
    let a = actor("agent-1");
    drop(LogWriter::open(&dir, &a, HEADER).unwrap());
    let path = log_file(&dir, &a);
    let head = fs::read(&path).unwrap();
    let mut names = Vec::new();
    for tail in [&b"one"[..], b"two"] {
        let mut bytes = head.clone();
        bytes.extend_from_slice(tail);
        fs::write(&path, &bytes).unwrap();
        let (_, rec) = LogWriter::open(&dir, &a, HEADER).unwrap();
        let kept = rec.quarantined.unwrap();
        names.push(kept.file_name().unwrap().to_string_lossy().into_owned());
    }
    let off = head.len();
    assert_eq!(
        names,
        vec![
            format!("agent-1.alog.torn-{off}"),
            format!("agent-1.alog.torn-{off}-1")
        ]
    );
}

#[test]
fn the_clock_restarts_its_counter_once_the_wall_clock_moves_on() {
    let mut clock = Clock::starting_after(Some(Stamp {
        wall_ns: 1,
        logical: 7,
    }));
    let next = clock.tick();
    assert!(next.wall_ns > 1);
    assert_eq!(next.logical, 0);
}

/// A reader checks every frame it hands out, also after the file changed under it.
#[test]
fn a_frame_damaged_after_the_reader_indexed_it_is_corrupt() {
    let dir = scratch("late-damage");
    let a = actor("agent-1");
    let (mut w, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    let mut clock = Clock::starting_after(None);
    w.append(clock.tick(), b"first frame").unwrap();
    w.append(clock.tick(), b"second").unwrap();
    w.sync().unwrap();
    let reader = LogReader::open(&dir, &a).unwrap();
    assert!(!reader.is_empty());

    let path = log_file(&dir, &a);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(&path, &bytes).unwrap();
    let second = Seq::FIRST.next();
    assert!(matches!(reader.get(second), Err(LogError::Corrupt { .. })));
    let iterated: Vec<_> = reader.iter_from(Seq::FIRST).unwrap().collect();
    assert!(iterated[0].is_ok());
    assert!(
        matches!(iterated[1], Err(LogError::Corrupt { .. })),
        "{:?}",
        iterated[1]
    );
}

#[test]
fn small_accessors_say_what_they_hold() {
    let dir = scratch("accessors");
    assert_eq!(
        LogDir::new(dir.root().join("missing")).actors().unwrap(),
        vec![]
    );
    let a = actor("agent-1");
    let (mut w, _) = LogWriter::open(&dir, &a, HEADER).unwrap();
    assert!(!w.is_dirty());
    assert!(LogReader::open(&dir, &a).unwrap().is_empty());
    w.append(Clock::starting_after(None).tick(), b"x").unwrap();
    assert!(w.is_dirty());
    w.sync().unwrap();
    assert!(!w.is_dirty());

    let mut cursor = Cursor::default();
    let id = |seq: u64| EventId::parse(&format!("agent-1:{seq}")).unwrap();
    assert_eq!(cursor.admit(&id(0)), Admit::Fresh);
    assert_eq!(
        cursor.iter().collect::<Vec<_>>(),
        vec![(&a, Seq::FIRST.next())]
    );
}

/// Appends frames one by one, returning each frame's bytes as stored.
fn frames_of(dir: &LogDir, key: &str, stamps: &[(u64, &[u8])]) -> Vec<Vec<u8>> {
    let a = actor(key);
    let (mut w, _) = LogWriter::open(dir, &a, HEADER).unwrap();
    let path = log_file(dir, &a);
    let mut out = Vec::new();
    for (wall_ns, payload) in stamps {
        let before = fs::metadata(&path).unwrap().len() as usize;
        w.append(
            Stamp {
                wall_ns: *wall_ns,
                logical: 0,
            },
            payload,
        )
        .unwrap();
        w.sync().unwrap();
        out.push(fs::read(&path).unwrap()[before..].to_vec());
    }
    out
}

/// A frame with a valid checksum still ends the log if it repeats a stamp or a seq: a
/// spliced or concatenated file is not trusted past that point.
#[test]
fn a_valid_frame_with_a_repeated_stamp_or_seq_ends_the_log() {
    for (name, foreign) in [("same-stamp", 1usize), ("same-seq", 0usize)] {
        let dir = scratch(name);
        drop(frames_of(&dir, "agent-1", &[(5, b"a")]));
        // Another log's frames. Same stamp: its seq 1 at stamp 5, right after our seq 0
        // at stamp 5. Same seq: its seq 0 at stamp 6, a later stamp but seq 0 again.
        let stamps: [(u64, &[u8]); 2] = match foreign {
            1 => [(4, b"x"), (5, b"b")],
            _ => [(6, b"x"), (7, b"b")],
        };
        let other = frames_of(&dir, "agent-2", &stamps);
        let path = log_file(&dir, &actor("agent-1"));
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&other[foreign]);
        fs::write(&path, &bytes).unwrap();
        let (_, rec) = LogWriter::open(&dir, &actor("agent-1"), HEADER).unwrap();
        assert_eq!(rec.truncated_bytes, other[foreign].len() as u64, "{name}");
    }
}

#[test]
fn a_header_of_exactly_the_largest_size_reads_back() {
    let dir = scratch("big-header");
    let a = actor("agent-1");
    let header = vec![b'h'; 1 << 16];
    drop(LogWriter::open(&dir, &a, &header).unwrap());
    assert!(LogWriter::open(&dir, &a, &header).is_ok());
    assert_eq!(LogReader::open(&dir, &a).unwrap().header(), &header[..]);
}

/// A log cut short under an open reader: the reader reports where the bad frame starts.
#[test]
fn a_frame_cut_short_after_indexing_is_corrupt_at_its_offset() {
    let dir = scratch("late-cut");
    let frames = frames_of(
        &dir,
        "agent-1",
        &[(1, b"first frame"), (2, b"second frame")],
    );
    let a = actor("agent-1");
    let reader = LogReader::open(&dir, &a).unwrap();
    let path = log_file(&dir, &a);
    let len = fs::metadata(&path).unwrap().len();
    let second_at = len - frames[1].len() as u64;
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len - 3)
        .unwrap();
    let iterated: Vec<_> = reader.iter_from(Seq::FIRST).unwrap().collect();
    assert!(iterated[0].is_ok());
    match &iterated[1] {
        Err(LogError::Corrupt { offset, .. }) => assert_eq!(*offset, second_at),
        other => panic!("{other:?}"),
    }
}

/// When the cut-off bytes cannot be kept, opening fails instead of dropping them.
#[test]
fn a_tail_that_cannot_be_set_aside_fails_the_open() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("readonly");
    drop(frames_of(&dir, "agent-1", &[(1, b"kept")]));
    let path = log_file(&dir, &actor("agent-1"));
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"torn");
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(dir.root(), fs::Permissions::from_mode(0o555)).unwrap();
    let started = std::time::Instant::now();
    let opened = LogWriter::open(&dir, &actor("agent-1"), HEADER);
    fs::set_permissions(dir.root(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(opened.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(fs::read(&path).unwrap(), bytes, "nothing was cut");
}

#[test]
fn a_log_directory_that_is_a_file_is_an_error() {
    let dir = scratch("file-root");
    let file = dir.root().join("not-a-dir");
    fs::write(&file, b"").unwrap();
    assert!(LogDir::new(file).actors().is_err());
}
