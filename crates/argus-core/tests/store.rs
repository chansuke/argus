//! The data root: one ledger per job, one meta ledger, found again after a restart.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_core::Store;
use argus_core::ids::{Id128, JobId};

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "argus-store-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn jobs_are_found_again_in_id_order_and_open_once() {
    let root = scratch();
    let store = Store::open(root.clone()).unwrap();
    assert!(root.join("jobs").is_dir() && root.join("meta").is_dir());
    assert_eq!(store.jobs().unwrap(), vec![]);

    let (a, b) = (JobId(Id128(2)), JobId(Id128(1)));
    let job_a = store.open_job(a).unwrap();
    drop(store.open_job(b).unwrap());
    // Something that is not a job is ignored.
    std::fs::create_dir_all(root.join("jobs").join("notes")).unwrap();
    std::fs::write(root.join("jobs").join("README"), "").unwrap();
    assert_eq!(
        Store::open(root.clone()).unwrap().jobs().unwrap(),
        vec![b, a]
    );

    // Only one host may hold a job.
    assert!(store.open_job(a).is_err());
    drop(job_a);
    assert!(store.open_job(a).is_ok());
    assert!(store.open_meta().is_ok());
}

#[test]
fn analytics_read_every_job_while_its_host_holds_it() {
    use argus_core::event::{AgentLaunch, Author, Event, JobInfo, JobOrigin, RunPurpose};
    use argus_core::ids::{Attempt, GitOid, RunId};
    use argus_core::models::AgentCli;
    use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};
    let root = scratch();
    let store = Store::open(root.clone()).unwrap();
    let job = JobId(Id128(3));
    let run = RunId(Id128(4));
    let mut ledger = store.open_job(job).unwrap();
    let info = JobInfo {
        job,
        title: "fix the gate".into(),
        repo: root.join("repo"),
        base: GitOid::parse(&"b".repeat(40)).unwrap(),
        target: "main".into(),
        worktree: root.join("worktree"),
        group: None,
        origin: JobOrigin::Launched,
    };
    ledger.submit(Author::Host, Event::JobOpened(info)).unwrap();
    let launch = AgentLaunch {
        cli: AgentCli::Claude,
        model: None,
        prompt: "go".into(),
        resume: None,
    };
    let requested = Event::RunRequested {
        run,
        purpose: RunPurpose::Agent(launch),
    };
    ledger.submit(Author::Human, requested).unwrap();
    let origin = SourceOrigin::File(PathBuf::from("/h/.claude/projects/w/s.jsonl"));
    let meta = SourceMeta {
        id: SourceMeta::derive_id(run, Attempt(0), &origin),
        run,
        attempt: Attempt(0),
        kind: SourceKind::ClaudeTranscript,
        origin,
    };
    let source = ledger.open_source(meta).unwrap().meta.id;
    let lines = [
        r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"fix the gate"},"uuid":"u-1","timestamp":"2026-10-24T03:16:30.255Z"}"#,
        r#"{"parentUuid":"u-1","type":"assistant","uuid":"a-1","timestamp":"2026-10-24T03:16:31.000Z","message":{"id":"msg_1","model":"opus","content":[{"type":"text","text":"on it"}],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        r#"{"parentUuid":"a-1","type":"assistant","uuid":"a-2","timestamp":"2026-10-24T03:16:32.000Z","message":{"id":"msg_1","model":"opus","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":9}}}"#,
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let frames = SourceKind::ClaudeTranscript.framer(0).push(text.as_bytes());
    ledger.observe(source, frames).unwrap();
    ledger.flush().unwrap();
    std::fs::create_dir_all(root.join("jobs").join(Id128(9).to_string())).unwrap();

    let analytics = store.analytics().unwrap();
    assert_eq!(
        analytics.jobs.keys().copied().collect::<Vec<_>>(),
        vec![job, JobId(Id128(9))],
        "an empty job directory is a job with nothing in it"
    );
    let stats = &analytics.jobs[&job].runs[&run];
    assert_eq!(stats.cli, Some(AgentCli::Claude));
    let total = |f: fn(&argus_core::views::stats::Counters) -> u64| -> u64 {
        stats.hours.values().map(f).sum()
    };
    assert_eq!(total(|c| c.prompts), 1);
    assert_eq!(total(|c| c.replies), 2);
    assert_eq!(
        total(|c| c.output_tokens),
        9,
        "a message's usage counts once"
    );
    drop(ledger);
}
