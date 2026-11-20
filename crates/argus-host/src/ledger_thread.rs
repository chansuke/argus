//! One thread per open ledger, owning it. All writes to a ledger are serialized here, so
//! the ledger's sequencer order is the order views see, live and on replay. Reads also
//! run here, but each is a short fold-state lookup: anything slow (git, LLMs, processes)
//! runs elsewhere and comes back as a command.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use argus_core::event::{Author, Event, JobInfo, LedgerKind, RunOutcome};
use argus_core::ids::{Attempt, RunId, SourceId, UnixMillis};
use argus_core::ledger::{JobLedger, LedgerError, SourceCursor, Submitted};
use argus_core::plan::{Invocation, PlanEnv, ToolPaths, plan};
use argus_core::policy::{self, PolicyConfig};
use argus_core::source::{RawFrame, SourceMeta};
use argus_core::views::runs::Due;

use crate::pulse::Dep;
use crate::runner::{Running, Workspace};

pub(crate) enum Cmd {
    Submit {
        author: Author,
        event: Event,
        reply: Sender<Result<Submitted, LedgerError>>,
    },
    OpenSource {
        meta: SourceMeta,
        reply: Sender<Result<SourceCursor, LedgerError>>,
    },
    Observe {
        source: SourceId,
        frames: Vec<RawFrame>,
    },
    /// Read-only access for API handlers; runs on this thread, so it sees a consistent
    /// state. The thread wraps `f` in `take_reads()` calls and replies with the `Dep`.
    Read(Box<dyn FnOnce(&JobLedger) + Send>),
    /// The runner stopped executing a run (it already submitted `RunFinished`).
    Released {
        run: RunId,
    },
    Flush,
    /// Ends the loop (host shutdown). Runs still executing keep their own threads.
    Stop,
    /// Daily: `prune_source` for Terminal-family sources older than `Retention`.
    Retain,
}

#[derive(Clone)]
pub(crate) struct LedgerHandle {
    tx: Sender<Cmd>,
}

impl LedgerHandle {
    pub(crate) fn submit(&self, author: Author, event: Event) -> Result<Submitted, LedgerError> {
        todo!("not implemented")
    }
    /// Runs `f` on the ledger thread and appends what it read to `deps` (subscriptions
    /// keep them; one-shot calls drop them).
    #[expect(clippy::ptr_arg, reason = "sketch: the body pushes the read's Dep")]
    pub(crate) fn read<T: Send + 'static>(
        &self,
        deps: &mut Vec<Dep>,
        f: impl FnOnce(&JobLedger) -> T + Send + 'static,
    ) -> T {
        todo!("not implemented")
    }
    pub(crate) fn sender(&self) -> Sender<Cmd> {
        self.tx.clone()
    }
}

/// Starts runs. The host's is `runner::start`; tests stand in for processes.
pub(crate) trait Launcher {
    fn start(
        &mut self,
        inv: Invocation,
        env: PlanEnv,
        ws: Workspace,
        ledger: Sender<Cmd>,
    ) -> Running;
}

/// What a job's loop needs besides its ledger: where its runs execute and how.
pub(crate) struct LoopConfig {
    pub info: JobInfo,
    pub home: PathBuf,
    pub policy: PolicyConfig,
    pub tools: ToolPaths,
    pub socket: PathBuf,
    pub snapshot_deny: Vec<String>,
}

/// One job's ledger and the runs the host is executing for it. Only this owns the
/// ledger, so every write is ordered here, live and on replay alike.
pub(crate) struct JobLoop<L: Launcher> {
    ledger: JobLedger,
    /// This loop's own inbox, handed to each run it starts.
    inbox: Sender<Cmd>,
    live: BTreeMap<RunId, Running>,
    launcher: L,
    cfg: LoopConfig,
}

impl<L: Launcher> JobLoop<L> {
    pub(crate) fn new(ledger: JobLedger, inbox: Sender<Cmd>, launcher: L, cfg: LoopConfig) -> Self {
        JobLoop {
            ledger,
            inbox,
            live: BTreeMap::new(),
            launcher,
            cfg,
        }
    }

    /// Applies one command. Returns whether the views may have changed.
    pub(crate) fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Submit {
                author,
                event,
                reply,
            } => {
                let typed = match &event {
                    Event::RunInput { run, bytes, .. } => Some((*run, bytes.clone())),
                    _ => None,
                };
                let result = self.ledger.submit(author, event);
                let changed = matches!(result, Ok(Submitted::Appended(_)));
                // Typed bytes reach the program only once they are on record, and a
                // retried submission (a duplicate) never types them twice.
                if changed
                    && let Some((run, bytes)) = typed
                    && let Some(running) = self.live.get(&run)
                {
                    running.send_input(bytes);
                }
                let _ = reply.send(result);
                changed
            }
            Cmd::OpenSource { meta, reply } => {
                let _ = reply.send(self.ledger.open_source(meta));
                false
            }
            // A source that is not open (or a corrupt frame) has nowhere to go.
            Cmd::Observe { source, frames } => self.ledger.observe(source, frames).is_ok(),
            Cmd::Read(read) => {
                read(&self.ledger);
                false
            }
            Cmd::Released { run } => self.live.remove(&run).is_some(),
            Cmd::Flush => {
                let _ = self.ledger.flush();
                false
            }
            Cmd::Stop | Cmd::Retain => false,
        }
    }

    /// After a batch of commands: submits what the policy decides, then does what the
    /// runs view says the host owes (`Runs::due`), until neither has anything left.
    pub(crate) fn after_batch(&mut self, now: UnixMillis) {
        // Each pass can enable the next (a decision requests a run, a finished run lets
        // the policy decide again); the bound only guards against a policy bug.
        for _ in 0..8 {
            let mut acted = false;
            for event in policy::decide(self.ledger.views(), &self.cfg.policy, now) {
                acted |= self.submit(Author::Policy, event);
            }
            let live: BTreeSet<RunId> = self.live.keys().copied().collect();
            for due in self.ledger.views().runs().due(&live) {
                match due {
                    Due::Start { run, attempt } => acted |= self.start(run, attempt),
                    Due::Abandon { run, attempt } => {
                        let outcome = RunOutcome::Abandoned;
                        acted |= self.submit(
                            Author::Host,
                            Event::RunFinished {
                                run,
                                attempt,
                                outcome,
                            },
                        );
                    }
                    Due::Kill { run } => {
                        if let Some(running) = self.live.get(&run) {
                            running.cancel();
                        }
                    }
                    // Leases exist only for device checks, which are not supported yet.
                    Due::Release { .. } => {}
                }
            }
            if !acted {
                break;
            }
        }
    }

    /// Reconciles once (a restart abandons or retries interrupted runs), then blocks on
    /// the inbox and applies commands in batches until `Cmd::Stop`.
    pub(crate) fn run(mut self, inbox: Receiver<Cmd>) {
        self.after_batch(now());
        while let Ok(first) = inbox.recv() {
            let mut changed = false;
            let mut stop = false;
            for cmd in std::iter::once(first).chain(inbox.try_iter()) {
                if let Cmd::Stop = cmd {
                    stop = true;
                    break;
                }
                changed |= self.handle(cmd);
            }
            if changed {
                self.after_batch(now());
            }
            if stop {
                return;
            }
        }
    }

    /// Whether the event was appended (not refused, not a duplicate).
    fn submit(&mut self, author: Author, event: Event) -> bool {
        matches!(
            self.ledger.submit(author, event),
            Ok(Submitted::Appended(_))
        )
    }

    /// Records the attempt as started, then hands it to the launcher.
    fn start(&mut self, run: RunId, attempt: Attempt) -> bool {
        let views = self.ledger.views();
        let Some(state) = views.runs().get(&run) else {
            return false;
        };
        // Meta-ledger purposes never belong to a job.
        if state.purpose.ledger_kind() != LedgerKind::Job {
            return false;
        }
        let purpose = state.purpose.clone();
        let previous = views.review().head().map(|h| h.snapshot.tree.clone());
        if !self.submit(
            Author::Host,
            Event::RunStarted {
                run,
                attempt,
                pid: None,
            },
        ) {
            return false;
        }
        let info = &self.cfg.info;
        let env = PlanEnv {
            job: Some(info.job),
            run,
            attempt,
            socket: self.cfg.socket.clone(),
            tools: self.cfg.tools.clone(),
            snapshot_deny: self.cfg.snapshot_deny.clone(),
        };
        // An adapter that is not written yet panics; that fails this run, not the job.
        let planned = std::panic::catch_unwind(AssertUnwindSafe(|| plan(&purpose, &env)));
        let Ok(inv) = planned else {
            let reason = "argus cannot run this purpose yet".to_owned();
            let outcome = RunOutcome::Failed { reason };
            self.submit(
                Author::Host,
                Event::RunFinished {
                    run,
                    attempt,
                    outcome,
                },
            );
            return true;
        };
        let ws = Workspace {
            repo: info.repo.clone(),
            worktree: info.worktree.clone(),
            home: self.cfg.home.clone(),
            blobs: self.ledger.blobs(),
            job: info.job,
            base: info.base.clone(),
            previous,
        };
        let running = self.launcher.start(inv, env, ws, self.inbox.clone());
        self.live.insert(run, running);
        true
    }

    pub(crate) fn ledger(&self) -> &JobLedger {
        &self.ledger
    }
}

fn now() -> UnixMillis {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    UnixMillis(since.map_or(0, |d| d.as_millis() as i64))
}

/// Appends an authored event on the ledger thread.
pub(crate) fn submit(
    ledger: &Sender<Cmd>,
    author: Author,
    event: Event,
) -> std::io::Result<Submitted> {
    let (reply, answer) = std::sync::mpsc::channel();
    let gone = || std::io::Error::other("the ledger thread stopped");
    ledger
        .send(Cmd::Submit {
            author,
            event,
            reply,
        })
        .map_err(|_| gone())?;
    answer
        .recv()
        .map_err(|_| gone())?
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

/// Opens (or reopens) a source on the ledger thread and returns where ingestion resumes.
pub(crate) fn open_source(ledger: &Sender<Cmd>, meta: SourceMeta) -> std::io::Result<SourceCursor> {
    let (reply, answer) = std::sync::mpsc::channel();
    let gone = || std::io::Error::other("the ledger thread stopped");
    ledger
        .send(Cmd::OpenSource { meta, reply })
        .map_err(|_| gone())?;
    answer
        .recv()
        .map_err(|_| gone())?
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

/// Hands frames to the ledger thread. A stopped ledger thread means the host is shutting
/// down; the frames are lost with the process, as they would be with a crash.
pub(crate) fn observe(ledger: &Sender<Cmd>, source: SourceId, frames: Vec<RawFrame>) {
    if !frames.is_empty() {
        let _ = ledger.send(Cmd::Observe { source, frames });
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{self, Sender};
    use std::thread::JoinHandle;

    use argus_core::event::{Author, Event};
    use argus_core::ids::{EventId, SourceId};
    use argus_core::ledger::{SourceCursor, Submitted};
    use argus_core::source::{RawFrame, SourceMeta};

    use super::Cmd;

    /// What a fake ledger thread received: per source, its header and stored frames; and
    /// every submitted event, in order.
    #[derive(Default)]
    pub(crate) struct Received {
        pub sources: BTreeMap<SourceId, (SourceMeta, Vec<RawFrame>)>,
        pub events: Vec<(Author, Event)>,
    }

    /// A ledger thread that opens every source fresh and keeps what it is sent.
    pub(crate) fn fake_ledger() -> (Sender<Cmd>, JoinHandle<Received>) {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut got = Received::default();
            for cmd in rx {
                match cmd {
                    Cmd::Submit {
                        author,
                        event,
                        reply,
                    } => {
                        let id = EventId::parse(&format!("host:{}", got.events.len())).unwrap();
                        got.events.push((author, event));
                        reply.send(Ok(Submitted::Appended(id))).unwrap();
                    }
                    Cmd::OpenSource { meta, reply } => {
                        got.sources.insert(meta.id, (meta.clone(), Vec::new()));
                        let cursor = SourceCursor {
                            meta,
                            resume_at: 0,
                            last: None,
                        };
                        reply.send(Ok(cursor)).unwrap();
                    }
                    Cmd::Observe { source, frames } => {
                        let stored =
                            &mut got.sources.get_mut(&source).expect("observed after open").1;
                        stored.extend(frames);
                    }
                    _ => {}
                }
            }
            got
        });
        (tx, thread)
    }

    /// The bytes of every `Bytes` frame, in order.
    pub(crate) fn bytes(frames: &[RawFrame]) -> Vec<Vec<u8>> {
        frames
            .iter()
            .filter_map(|f| match f {
                RawFrame::Bytes { bytes, .. } => Some(bytes.clone()),
                RawFrame::Discontinuity { .. } => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use argus_core::event::{AgentLaunch, JobOrigin, ReviewLens, RunOutcome, RunPurpose, Snapshot};
    use argus_core::ids::{Attempt, GitOid, Id128, JobId, ModelId};
    use argus_core::models::{AgentCli, ModelChoice, ModelSet};
    use argus_core::plan::{SNAPSHOT_DENY, Workdir};

    /// One `Launcher::start` call, and the receiver its `Running::cancel` sends to.
    struct Started {
        run: RunId,
        attempt: Attempt,
        inv: Invocation,
        ws: Workspace,
        cancelled: mpsc::Receiver<()>,
        typed: mpsc::Receiver<Vec<u8>>,
    }

    #[derive(Clone, Default)]
    struct FakeLauncher(Arc<Mutex<Vec<Started>>>);

    impl Launcher for FakeLauncher {
        fn start(
            &mut self,
            inv: Invocation,
            env: PlanEnv,
            ws: Workspace,
            _: Sender<Cmd>,
        ) -> Running {
            let (cancel, cancelled) = mpsc::channel();
            let (input, typed) = mpsc::channel();
            self.0.lock().unwrap().push(Started {
                run: env.run,
                attempt: env.attempt,
                inv,
                ws,
                cancelled,
                typed,
            });
            Running::new(env.run, cancel, input)
        }
    }

    fn oid(c: char) -> GitOid {
        GitOid::parse(&c.to_string().repeat(40)).unwrap()
    }

    fn run(n: u128) -> RunId {
        RunId(Id128(n))
    }

    fn scratch() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "argus-loop-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn cfg(policy: PolicyConfig) -> LoopConfig {
        LoopConfig {
            info: JobInfo {
                job: JobId(Id128(1)),
                title: "fix the gate".into(),
                repo: PathBuf::from("/w/argus"),
                base: oid('b'),
                target: "main".into(),
                worktree: PathBuf::from("/w/jobs/1"),
                group: None,
                origin: JobOrigin::Launched,
            },
            home: PathBuf::from("/h"),
            policy,
            tools: ToolPaths {
                claude: "claude".into(),
                codex: "codex".into(),
                java: "java".into(),
                tla2tools_jar: "tla2tools.jar".into(),
                tlars: "tlars".into(),
                lake: "lake".into(),
                eas: "eas".into(),
                script: "script".into(),
            },
            socket: PathBuf::from("/run/argus.sock"),
            snapshot_deny: SNAPSHOT_DENY.iter().map(|s| s.to_string()).collect(),
        }
    }

    const NOW: UnixMillis = UnixMillis(1_792_811_790_000);

    struct Job {
        dir: PathBuf,
        launcher: FakeLauncher,
        job: JobLoop<FakeLauncher>,
    }

    impl Job {
        fn open(dir: PathBuf, policy: PolicyConfig) -> Self {
            let launcher = FakeLauncher::default();
            let (inbox, _) = mpsc::channel();
            let ledger = JobLedger::open(&dir).unwrap();
            let job = JobLoop::new(ledger, inbox, launcher.clone(), cfg(policy));
            Job { dir, launcher, job }
        }

        /// Submits through the loop, as a transport or a run would, then lets it act.
        fn submit(&mut self, author: Author, event: Event) -> Submitted {
            let (reply, answer) = mpsc::channel();
            self.job.handle(Cmd::Submit {
                author,
                event,
                reply,
            });
            self.job.after_batch(NOW);
            answer.recv().unwrap().unwrap()
        }

        fn release(&mut self, n: u128) {
            self.job.handle(Cmd::Released { run: run(n) });
            self.job.after_batch(NOW);
        }

        fn started(&self) -> Vec<(RunId, Attempt)> {
            self.launcher
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|s| (s.run, s.attempt))
                .collect()
        }

        fn attempts(&self, n: u128) -> Vec<(Attempt, Option<RunOutcome>)> {
            let runs = self.job.ledger().views().runs();
            runs.get(&run(n))
                .map(|r| {
                    r.attempts
                        .iter()
                        .map(|a| (a.attempt, a.finished.clone().map(|f| f.1)))
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn agent(n: u128) -> Event {
        Event::RunRequested {
            run: run(n),
            purpose: RunPurpose::Agent(AgentLaunch {
                cli: AgentCli::Claude,
                model: None,
                prompt: "fix the gate".into(),
                resume: None,
            }),
        }
    }

    fn finished(n: u128, attempt: u16, outcome: RunOutcome) -> Event {
        Event::RunFinished {
            run: run(n),
            attempt: Attempt(attempt),
            outcome,
        }
    }

    #[test]
    fn a_requested_run_is_started_once_and_recorded_as_started() {
        let mut job = Job::open(scratch(), PolicyConfig::default());
        job.submit(Author::Human, agent(1));
        assert_eq!(job.started(), vec![(run(1), Attempt(0))]);
        assert_eq!(job.attempts(1), vec![(Attempt(0), None)]);
        // Planned from the purpose, in the job's own worktree.
        {
            // The guard must go before `started()` locks again.
            let launcher = job.launcher.0.lock().unwrap();
            let launched = &launcher[0];
            let Invocation::Process(spec) = &launched.inv else {
                panic!("{:?}", launched.inv)
            };
            assert_eq!(spec.program, PathBuf::from("claude"));
            assert!(matches!(spec.workdir, Workdir::JobWorktree));
            assert_eq!(launched.ws.worktree, PathBuf::from("/w/jobs/1"));
            assert_eq!(launched.ws.repo, PathBuf::from("/w/argus"));
        }

        // Nothing new happens on later batches while it runs.
        job.job.after_batch(NOW);
        assert_eq!(job.started().len(), 1);
    }

    #[test]
    fn a_run_finishes_by_its_own_report_and_is_not_restarted() {
        let mut job = Job::open(scratch(), PolicyConfig::default());
        job.submit(Author::Human, agent(1));
        job.submit(Author::Host, finished(1, 0, RunOutcome::Exited { code: 0 }));
        job.release(1);
        assert_eq!(job.started().len(), 1);
        assert_eq!(
            job.attempts(1),
            vec![(Attempt(0), Some(RunOutcome::Exited { code: 0 }))]
        );
    }

    #[test]
    fn a_cancel_reaches_the_running_run() {
        let mut job = Job::open(scratch(), PolicyConfig::default());
        job.submit(Author::Human, agent(1));
        job.submit(Author::Human, Event::RunCancelRequested { run: run(1) });
        let launcher = job.launcher.0.lock().unwrap();
        assert!(launcher[0].cancelled.try_recv().is_ok());
    }

    #[test]
    fn typed_input_goes_on_record_then_to_the_live_run_once() {
        use argus_core::ids::InputId;
        let mut job = Job::open(scratch(), PolicyConfig::default());
        job.submit(Author::Human, agent(1));
        let typed = |n: u128, input: u128| Event::RunInput {
            input: InputId(Id128(input)),
            run: run(n),
            bytes: b"y\r".to_vec(),
        };
        assert!(matches!(
            job.submit(Author::Human, typed(1, 7)),
            Submitted::Appended(_)
        ));
        // A retried submission is a duplicate: the agent must not get the keys twice.
        assert!(matches!(
            job.submit(Author::Human, typed(1, 7)),
            Submitted::Duplicate(_)
        ));
        // Input for a run that is not executing is recorded and goes nowhere.
        job.submit(Author::Human, typed(9, 8));
        let launcher = job.launcher.0.lock().unwrap();
        let received: Vec<Vec<u8>> = launcher[0].typed.try_iter().collect();
        assert_eq!(received, vec![b"y\r".to_vec()]);
    }

    #[test]
    fn after_a_crash_interrupted_runs_are_abandoned_and_safe_ones_retried() {
        let dir = scratch();
        {
            let mut job = Job::open(dir.clone(), PolicyConfig::default());
            job.submit(Author::Human, agent(1));
            job.submit(
                Author::Policy,
                Event::RunRequested {
                    run: run(2),
                    purpose: RunPurpose::Snapshot { after: None },
                },
            );
            assert_eq!(job.started().len(), 2);
            // The host dies here: neither run reports back.
        }
        let mut job = Job::open(dir, PolicyConfig::default());
        job.job.after_batch(NOW);
        assert_eq!(
            job.attempts(1),
            vec![(Attempt(0), Some(RunOutcome::Abandoned))]
        );
        // The agent waits for a human; the snapshot is retried as a new attempt.
        assert_eq!(
            job.attempts(2),
            vec![
                (Attempt(0), Some(RunOutcome::Abandoned)),
                (Attempt(1), None)
            ]
        );
        assert_eq!(job.started(), vec![(run(2), Attempt(1))]);
    }

    #[test]
    fn the_policy_s_requests_are_started_in_the_same_batch() {
        let models = ModelSet::new([ModelChoice {
            cli: AgentCli::Claude,
            model: ModelId("opus".into()),
            effort: None,
        }])
        .unwrap();
        let policy = PolicyConfig {
            review_on_snapshot: Some((models, ReviewLens::General)),
            ..PolicyConfig::default()
        };
        let mut job = Job::open(scratch(), policy);
        job.submit(
            Author::Policy,
            Event::RunRequested {
                run: run(2),
                purpose: RunPurpose::Snapshot { after: None },
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
        job.submit(Author::Host, finished(2, 0, RunOutcome::Snapshot(snapshot)));
        job.release(2);
        let launched = job.launcher.0.lock().unwrap();
        let review = launched.iter().find_map(|s| match &s.inv {
            Invocation::Process(spec) if matches!(spec.workdir, Workdir::Snapshot { .. }) => {
                Some((s.run, s.ws.previous.clone()))
            }
            _ => None,
        });
        // The reviewer was started, and new snapshots will be recorded against the head.
        let (_, previous) = review.expect("a review run was started");
        assert_eq!(previous, Some(oid('1')));
    }
}
