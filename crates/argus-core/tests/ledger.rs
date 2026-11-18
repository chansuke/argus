//! The ledger seen from outside: authored events and observed agent records reach the
//! views once each, and reopening the store (replay) shows exactly what live did.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use argus_core::event::{Author, Event, LedgerKind, Rejected, RunOutcome, RunPurpose};
use argus_core::fact::{Fact, FactBody};
use argus_core::ids::{Attempt, Id128, RunId, SourceId};
use argus_core::models::AgentCli;
use argus_core::source::{
    Observation, RawFrame, SourceFamily, SourceKind, SourceMeta, SourceOrigin,
};
use argus_core::views::{Projection, ViewMask, ViewSet, ViewStamp};
use argus_core::{Ledger, LedgerError, Reading, Submitted, replay};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argus-ledger-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// One line per fact a view received, in order. Two ledgers that applied the same facts
/// in the same order have equal logs.
#[derive(Default, Debug, Clone, PartialEq)]
struct Log(Vec<String>, u64);

fn describe(fact: &Fact<'_>) -> String {
    let what = match &fact.body {
        FactBody::Authored { author, event } => format!("{author:?} {event:?}"),
        FactBody::Observed { obs, raw, .. } => format!("{obs:?} raw={}", raw.len()),
    };
    format!("{} at={} {what}", fact.id, fact.at.0)
}

impl Projection for Log {
    const NAME: &'static str = "log";
    const VERSION: u32 = 1;
    const READS: &'static [SourceFamily] = &[SourceFamily::AgentSession];
    fn apply(&mut self, fact: &Fact<'_>) -> bool {
        if let FactBody::Observed {
            obs: Observation::Unrecognized { .. },
            ..
        } = fact.body
        {
            self.1 += 1;
        }
        self.0.push(describe(fact));
        true
    }
}

#[derive(Default)]
struct TestViews {
    log: Log,
}

impl ViewSet for TestViews {
    const KIND: LedgerKind = LedgerKind::Job;
    fn wants(source: &SourceMeta) -> bool {
        Log::READS.contains(&source.kind.family())
    }
    fn apply(&mut self, fact: &Fact<'_>) {
        self.log.apply(fact);
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

type TestLedger = Ledger<TestViews>;

fn run(n: u128) -> RunId {
    RunId(Id128(n))
}

fn transcript(run_id: RunId, file: &str) -> SourceMeta {
    let origin = SourceOrigin::File(PathBuf::from(file));
    SourceMeta {
        id: SourceMeta::derive_id(run_id, Attempt(0), &origin),
        run: run_id,
        attempt: Attempt(0),
        kind: SourceKind::ClaudeTranscript,
        origin,
    }
}

const LINES: &[&str] = &[
    r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"fix the gate"},"uuid":"u-1","timestamp":"2026-10-24T03:16:30.255Z"}"#,
    r#"{"parentUuid":"u-1","type":"assistant","uuid":"a-1","timestamp":"2026-10-24T03:16:31.000Z","message":{"id":"msg_1","model":"m","content":[{"type":"thinking","thinking":"","signature":"s"}],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":2}}}"#,
    r#"{"parentUuid":"a-1","type":"assistant","uuid":"a-2","timestamp":"2026-10-24T03:16:32.000Z","message":{"id":"msg_1","model":"m","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":9}}}"#,
];

fn frames(lines: &[&str], start: u64) -> Vec<RawFrame> {
    let mut framer = SourceKind::ClaudeTranscript.framer(start);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    framer.push(text.as_bytes())
}

fn agent_request(n: u128) -> Event {
    Event::RunRequested {
        run: run(n),
        purpose: RunPurpose::Agent(argus_core::event::AgentLaunch {
            cli: AgentCli::Claude,
            model: None,
            prompt: "go".into(),
            resume: None,
        }),
    }
}

#[test]
fn a_retried_submission_is_stored_once() {
    let dir = scratch("dedup");
    let mut ledger = TestLedger::open(&dir).unwrap();
    let first = ledger.submit(Author::Human, agent_request(1)).unwrap();
    let Submitted::Appended(id) = first else {
        panic!("{first:?}")
    };
    assert_eq!(
        ledger.submit(Author::Human, agent_request(1)).unwrap(),
        Submitted::Duplicate(id.clone())
    );
    assert_eq!(ledger.views().log.0.len(), 1);

    // The dedup index is rebuilt on open, so a retry after a restart is still a duplicate.
    drop(ledger);
    let mut ledger = TestLedger::open(&dir).unwrap();
    assert_eq!(
        ledger.submit(Author::Human, agent_request(1)).unwrap(),
        Submitted::Duplicate(id)
    );
}

#[test]
fn authorship_is_checked_before_anything_is_written() {
    let dir = scratch("author");
    let mut ledger = TestLedger::open(&dir).unwrap();
    let started = Event::RunStarted {
        run: run(1),
        attempt: Attempt(0),
        pid: None,
    };
    assert!(matches!(
        ledger.submit(Author::Human, started.clone()),
        Err(LedgerError::Rejected(Rejected::WrongAuthor))
    ));
    assert!(ledger.views().log.0.is_empty());
    assert!(matches!(
        ledger.submit(Author::Host, started),
        Ok(Submitted::Appended(_))
    ));
}

#[test]
fn observed_records_reach_the_views_with_their_own_time() {
    let dir = scratch("observe");
    let mut ledger = TestLedger::open(&dir).unwrap();
    let meta = transcript(run(1), "/h/.claude/projects/w/s.jsonl");
    let cursor = ledger.open_source(meta.clone()).unwrap();
    assert_eq!(cursor.resume_at, 0);
    assert_eq!(cursor.last, None);
    let ids = ledger.observe(meta.id, frames(LINES, 0)).unwrap();
    assert_eq!(ids.len(), 3);

    let log = &ledger.views().log.0;
    // user prompt, thinking + usage, reply + usage + turn end
    assert_eq!(log.len(), 6, "{log:#?}");
    assert!(log[0].contains("at=1792811790255") && log[0].contains("fix the gate"));
    assert!(log[0].starts_with(&format!("{}.0 ", ids[0])));
    assert!(log[5].starts_with(&format!("{}.2 ", ids[2])) && log[5].contains("TurnEnded"));

    let raw = ledger.raw(&ids[2]).unwrap();
    assert_eq!(raw.bytes, format!("{}\n", LINES[2]).into_bytes());
    match raw.reading {
        Reading::Observed {
            source,
            decoded,
            repeat,
        } => {
            assert_eq!(source, meta);
            assert!(!repeat);
            assert_eq!(decoded.iter().count(), 3);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn reopening_replays_exactly_what_live_applied() {
    let dir = scratch("replay");
    let live = {
        let mut ledger = TestLedger::open(&dir).unwrap();
        ledger.submit(Author::Human, agent_request(1)).unwrap();
        let meta = transcript(run(1), "/h/a.jsonl");
        ledger.open_source(meta.clone()).unwrap();
        ledger.observe(meta.id, frames(&LINES[..2], 0)).unwrap();
        ledger
            .submit(
                Author::Host,
                Event::RunFinished {
                    run: run(1),
                    attempt: Attempt(0),
                    outcome: RunOutcome::Exited { code: 0 },
                },
            )
            .unwrap();
        let start = frames(&LINES[..2], 0).iter().map(byte_len).sum();
        ledger.observe(meta.id, frames(&LINES[2..], start)).unwrap();
        // A terminal log is stored but not read by this view set.
        let pty = SourceMeta {
            id: SourceMeta::derive_id(run(1), Attempt(0), &SourceOrigin::Terminal),
            run: run(1),
            attempt: Attempt(0),
            kind: SourceKind::Terminal,
            origin: SourceOrigin::Terminal,
        };
        ledger.open_source(pty.clone()).unwrap();
        ledger
            .observe(
                pty.id,
                vec![RawFrame::Bytes {
                    offset: 0,
                    bytes: b"\x1b[1m$ ".to_vec(),
                }],
            )
            .unwrap();
        ledger.flush().unwrap();
        ledger.views().log.clone()
    };
    assert_eq!(live.0.len(), 8, "{:#?}", live.0);

    let reopened = TestLedger::open(&dir).unwrap();
    assert_eq!(reopened.views().log, live);
    drop(reopened);

    // The lock-free fold used by analytics sees the same, and resumes from a checkpoint.
    let (all, applied) = replay::<Log>(&dir, None).unwrap();
    assert_eq!(all, live);
    let (again, _) = replay::<Log>(&dir, Some((all.clone(), applied))).unwrap();
    assert_eq!(again, all);
}

fn byte_len(f: &RawFrame) -> u64 {
    match f {
        RawFrame::Bytes { bytes, .. } => bytes.len() as u64,
        RawFrame::Discontinuity { .. } => 0,
    }
}

/// `claude --resume` copies earlier records into a new file. The copies are stored and
/// visible as raw frames, but no view counts them twice, live or on replay.
#[test]
fn a_record_seen_in_two_files_is_applied_once() {
    let dir = scratch("repeat");
    let live = {
        let mut ledger = TestLedger::open(&dir).unwrap();
        let first = transcript(run(1), "/h/first.jsonl");
        let resumed = transcript(run(2), "/h/resumed.jsonl");
        ledger.open_source(first.clone()).unwrap();
        ledger.open_source(resumed.clone()).unwrap();
        ledger.observe(first.id, frames(&LINES[..2], 0)).unwrap();
        let copies = ledger.observe(resumed.id, frames(LINES, 0)).unwrap();
        // Two records repeated: the prompt, and a thinking block with its usage.
        assert_eq!(ledger.applied().repeats(), 3);
        let Reading::Observed { repeat, .. } = ledger.raw(&copies[0]).unwrap().reading else {
            panic!()
        };
        assert!(repeat);
        let Reading::Observed { repeat, .. } = ledger.raw(&copies[2]).unwrap().reading else {
            panic!()
        };
        assert!(!repeat);
        ledger.flush().unwrap();
        ledger.views().log.clone()
    };
    let prompts = live.0.iter().filter(|l| l.contains("fix the gate")).count();
    assert_eq!(prompts, 1, "{:#?}", live.0);
    let reopened = TestLedger::open(&dir).unwrap();
    assert_eq!(reopened.views().log, live);
    assert_eq!(reopened.applied().repeats(), 3);
}

#[test]
fn ingestion_resumes_after_the_last_stored_bytes() {
    let dir = scratch("resume");
    let meta = transcript(run(1), "/h/a.jsonl");
    let stored = frames(&LINES[..2], 0);
    let end: u64 = stored.iter().map(byte_len).sum();
    {
        let mut ledger = TestLedger::open(&dir).unwrap();
        ledger.open_source(meta.clone()).unwrap();
        ledger.observe(meta.id, stored).unwrap();
        ledger.flush().unwrap();
    }
    let mut ledger = TestLedger::open(&dir).unwrap();
    let cursor = ledger.open_source(meta.clone()).unwrap();
    assert_eq!(cursor.resume_at, end);
    let last = format!("{}\n", LINES[1]).into_bytes();
    assert_eq!(cursor.last, Some((end - last.len() as u64, last)));

    // A rewritten file starts over at 0, after a marker the timeline shows.
    ledger
        .observe(
            meta.id,
            vec![RawFrame::Discontinuity {
                stored_end: end,
                file_len: 10,
            }],
        )
        .unwrap();
    let cursor = ledger.open_source(meta.clone()).unwrap();
    assert_eq!((cursor.resume_at, cursor.last), (0, None));
    assert!(
        ledger
            .views()
            .log
            .0
            .last()
            .unwrap()
            .contains("Discontinuity")
    );
}

#[test]
fn a_source_must_be_opened_before_it_is_observed() {
    let dir = scratch("unknown");
    let mut ledger = TestLedger::open(&dir).unwrap();
    let id = SourceId(Id128(5));
    assert!(matches!(
        ledger.observe(id, frames(LINES, 0)),
        Err(LedgerError::UnknownSource(missing)) if missing == id
    ));
    let mut changed = transcript(run(1), "/h/a.jsonl");
    ledger.open_source(changed.clone()).unwrap();
    changed.kind = SourceKind::CodexRollout;
    assert!(ledger.open_source(changed).is_err());
}

#[test]
fn a_second_host_on_the_same_store_is_refused() {
    let dir = scratch("lock");
    let mut first = TestLedger::open(&dir).unwrap();
    first.submit(Author::Human, agent_request(1)).unwrap();
    assert!(matches!(
        TestLedger::open(&dir),
        Err(LedgerError::Log(argus_log::LogError::Locked(_)))
    ));
}

/// The crash test of argus-log, one level up: cutting a source log anywhere leaves a
/// ledger that opens and shows a prefix of what live showed.
#[test]
fn a_torn_source_log_reopens_to_a_prefix() {
    let dir = scratch("torn");
    let live = {
        let mut ledger = TestLedger::open(&dir).unwrap();
        let meta = transcript(run(1), "/h/a.jsonl");
        ledger.open_source(meta.clone()).unwrap();
        ledger.observe(meta.id, frames(LINES, 0)).unwrap();
        ledger.flush().unwrap();
        ledger.views().log.clone()
    };
    let log = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("src-"))
        .unwrap();
    let len = fs::metadata(&log).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&log)
        .unwrap()
        .set_len(len - 5)
        .unwrap();
    let reopened = TestLedger::open(&dir).unwrap();
    let shown = &reopened.views().log.0;
    assert!(shown.len() < live.0.len());
    assert_eq!(shown[..], live.0[..shown.len()]);
}

/// Imports every real transcript into a scratch ledger and checks that reopening it
/// replays what live applied. Reads files outside the repo, so it runs only on request:
/// `ARGUS_TRANSCRIPTS=~/.claude/projects cargo nextest run -p argus-core --run-ignored all real_`
#[test]
#[ignore = "reads real transcripts named by ARGUS_TRANSCRIPTS"]
fn real_transcripts_replay_equal_to_live() {
    let root = std::env::var_os("ARGUS_TRANSCRIPTS").expect("set ARGUS_TRANSCRIPTS");
    let dir = scratch("real");
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files);
    files.sort();
    let live = {
        let mut ledger = TestLedger::open(&dir).unwrap();
        for (i, file) in files.iter().enumerate() {
            let meta = transcript(run(i as u128 + 1), file.to_str().unwrap());
            ledger.open_source(meta.clone()).unwrap();
            let mut framer = SourceKind::ClaudeTranscript.framer(0);
            let mut frames = framer.push(&fs::read(file).unwrap());
            frames.extend(framer.finish());
            ledger.observe(meta.id, frames).unwrap();
        }
        ledger.flush().unwrap();
        eprintln!(
            "{} files, {} facts applied, {} repeats dropped",
            files.len(),
            ledger.views().log.0.len(),
            ledger.applied().repeats()
        );
        assert_eq!(ledger.views().log.1, 0, "unrecognized records");
        ledger.views().log.clone()
    };
    let reopened = TestLedger::open(&dir).unwrap();
    assert_eq!(reopened.views().log.0.len(), live.0.len());
    assert!(reopened.views().log == live);
    fs::remove_dir_all(&dir).unwrap();
}

/// Timeline and progress over every real transcript: nothing panics, every closed call
/// matches a result, and each agent's plan share stays in range.
#[derive(Default)]
struct ScreenViews {
    timeline: argus_core::views::timeline::Timeline,
    progress: argus_core::views::progress::Progress,
}

impl ViewSet for ScreenViews {
    const KIND: LedgerKind = LedgerKind::Job;
    fn wants(source: &SourceMeta) -> bool {
        source.kind.family() == SourceFamily::AgentSession
    }
    fn apply(&mut self, fact: &Fact<'_>) {
        self.timeline.apply(fact);
        self.progress.apply(fact);
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

#[test]
#[ignore = "reads real transcripts named by ARGUS_TRANSCRIPTS"]
fn real_transcripts_fill_the_timeline_and_progress() {
    use argus_core::ids::UnixMillis;
    use argus_core::views::progress::PlanNode;
    use argus_core::views::timeline::{BucketWidth, ItemKind};

    let root = std::env::var_os("ARGUS_TRANSCRIPTS").expect("set ARGUS_TRANSCRIPTS");
    let dir = scratch("real-screens");
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files);
    files.sort();
    let mut ledger = Ledger::<ScreenViews>::open(&dir).unwrap();
    for (i, file) in files.iter().enumerate() {
        let r = run(i as u128 + 1);
        ledger
            .submit(Author::Human, agent_request(i as u128 + 1))
            .unwrap();
        let meta = transcript(r, file.to_str().unwrap());
        ledger.open_source(meta.clone()).unwrap();
        let mut framer = SourceKind::ClaudeTranscript.framer(0);
        let mut frames = framer.push(&fs::read(file).unwrap());
        frames.extend(framer.finish());
        ledger.observe(meta.id, frames).unwrap();
    }
    let views = ledger.views();
    let all: Vec<_> = views
        .timeline
        .range(UnixMillis(i64::MIN), UnixMillis(i64::MAX))
        .collect();
    let calls = all
        .iter()
        .filter(|(_, _, i)| matches!(i.kind, ItemKind::ToolCall { .. }));
    let (closed, open): (Vec<_>, Vec<_>) = calls.partition(|(_, _, i)| i.ended_at.is_some());
    for (at, _, item) in &closed {
        assert!(
            item.ended_at.unwrap() >= *at,
            "a result before its call: {item:?}"
        );
        assert!(matches!(item.kind, ItemKind::ToolCall { ok: Some(_), .. }));
    }
    let hours = views.timeline.buckets(
        BucketWidth::H1,
        UnixMillis(i64::MIN / 2),
        UnixMillis(i64::MAX / 2),
    );
    let plans = views
        .progress
        .tree()
        .roots
        .iter()
        .filter(|n| matches!(n, PlanNode::Observed { .. }))
        .count();
    let share = views.progress.share().map(|s| s.get());
    eprintln!(
        "{} items, {} calls closed, {} open, {} active hours, {} observed plans, share {share:?}",
        all.len(),
        closed.len(),
        open.len(),
        hours.len(),
        plans
    );
    assert!(share.is_none_or(|s| (0.0..=1.0).contains(&s)));
    fs::remove_dir_all(&dir).unwrap();
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}
