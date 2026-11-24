//! The host: the single process that owns a store (its ledger locks make a second one
//! fail at open). Thin shell around `argus-core`: it executes what the core plans and
//! appends what happened. Embedded in the Tauri app, or run headless as `argusd`.
//!
//! ```text
//!  transports ─▶ Host::call / subscribe ─▶ argus_api::dispatch ─▶ ApiHandler (vm <-> core)
//!                                                │ submit / read (short, on the ledger thread)
//!                                                │ git diff for review packets (on the API thread)
//!                         ledger threads (one per open ledger, owns Ledger<V>) ◀─┘
//!                           │ after each batch:
//!                           ├─ policy::decide ─▶ submit (Author::Policy)
//!                           ├─ runs.due(live) ─▶ runner (processes, leases, in-process effects)
//!                           └─ pulse::Registry::changed ─▶ dirty subscriptions only
//!  runner / tailers ─▶ observe (raw frames) / submit (Author::Host) ─▶ ledger threads
//! ```

#![allow(dead_code, unused_variables)]

mod git;
mod glob;
mod handler;
mod lease;
mod ledger_thread;
mod pty;
mod pulse;
mod runner;
mod tail;
mod tokens;
mod transport;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use argus_api::{ApiError, Caller};
use argus_core::Store;
use argus_core::ids::{JobId, LedgerRef};
use argus_core::models::ModelPresets;
use argus_core::period::UtcOffset;
use argus_core::plan::{Invocation, PlanEnv, SNAPSHOT_DENY, ToolPaths};
use argus_core::policy::PolicyConfig;
use argus_core::views::ViewSet;
use argus_core::views::packet::PacketConfig;
use argus_json::Json;

use argus_core::views::MetaViews;
use ledger_thread::{Cmd, JobLoop, Launcher, LedgerHandle, LoopConfig, MetaLoop};

pub use transport::{PairedDevice, PhoneAccess, PhoneListen, serve_socket, serve_websocket};

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub data_root: PathBuf,
    /// Unix socket for the CLI and agents (`$XDG_RUNTIME_DIR/argus.sock`).
    pub socket: PathBuf,
    /// Network listener for the phone. Off by default.
    pub phone: Option<PhoneAccess>,
    pub tools: ToolPaths,
    pub policy: PolicyConfig,
    /// Review model presets; `request_review { reviewers: Default }` resolves here.
    pub presets: ModelPresets,
    pub packet: PacketConfig,
    /// Added to `plan::SNAPSHOT_DENY` (never removes from it).
    pub snapshot_deny: Vec<String>,
    pub retention: Retention,
    pub tz: UtcOffset,
    /// Directories whose subdirectories are skills (`<dir>/<name>/SKILL.md`). Their names
    /// go into insight prompts, so a model improves a skill instead of copying it.
    pub skill_dirs: Vec<PathBuf>,
}

/// Terminal (PTY) logs dominate disk and no projection reads them, so they alone may be
/// pruned: a whole `src-*` log of the Terminal family is deleted `terminal_days` after its
/// run finished, after an `Event::SourcePruned`. Everything else is kept forever.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// `None`: keep forever. Default: 30.
    pub terminal_days: Option<u16>,
}

/// Cheap to clone; shared by every transport thread.
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

pub(crate) struct HostInner {
    pub(crate) config: HostConfig,
    pub(crate) store: Store,
    /// One loop per open job: the handle to reach it and the thread that runs it.
    pub(crate) jobs: Mutex<BTreeMap<JobId, (LedgerHandle, JoinHandle<()>)>>,
    /// The meta ledger's loop (insights, skill candidates, skill writes).
    meta: Mutex<Option<(LedgerHandle<MetaViews>, JoinHandle<()>)>>,
    /// Behind a short mutex: marking dirty never waits on a ledger.
    pub(crate) registry: Arc<Mutex<pulse::Registry>>,
    pub(crate) diffs: git::DiffCache,
    /// Re-evaluates dirty subscriptions (`pulse::flush`), coalesced to ~100 ms.
    pulse: Mutex<Option<JoinHandle<()>>>,
    /// The agents' socket tokens, issued by the runner and checked by the transport.
    pub(crate) tokens: tokens::RunTokens,
}

impl HostInner {
    /// Opens a job's ledger and starts its loop. Its first pass reconciles: attempts that
    /// were running when the last host died are abandoned, and safe ones retried.
    pub(crate) fn open_job(&self, job: JobId) -> std::io::Result<()> {
        let ledger = self.store.open_job(job).map_err(io_error)?;
        let Some(info) = ledger.views().runs().job().cloned() else {
            return Ok(());
        };
        let config = &self.config;
        let mut snapshot_deny: Vec<String> = SNAPSHOT_DENY.iter().map(|s| s.to_string()).collect();
        snapshot_deny.extend(config.snapshot_deny.iter().cloned());
        let cfg = LoopConfig {
            info: Some(info),
            home: home(),
            policy: config.policy.clone(),
            tools: config.tools.clone(),
            socket: config.socket.clone(),
            snapshot_deny,
            tokens: self.tokens.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let registry = self.registry.clone();
        let at = LedgerRef::Job(job);
        let job_loop =
            JobLoop::new(ledger, tx.clone(), ProcessLauncher, cfg).on_change(move |ledger| {
                let views = ledger.views();
                registry
                    .lock()
                    .unwrap()
                    .changed(at, &|mask| views.stamp(mask));
            });
        let thread = std::thread::spawn(move || job_loop.run(rx));
        let handle = LedgerHandle::new(tx, LedgerRef::Job(job));
        self.jobs.lock().unwrap().insert(job, (handle, thread));
        Ok(())
    }

    /// Opens the meta ledger and starts its loop. Nothing there runs unasked; its first
    /// pass abandons or retries what was running when the last host died.
    fn open_meta(&self) -> std::io::Result<()> {
        let ledger = self.store.open_meta().map_err(io_error)?;
        let cfg = LoopConfig {
            info: None,
            home: home(),
            policy: PolicyConfig::default(),
            tools: self.config.tools.clone(),
            socket: self.config.socket.clone(),
            snapshot_deny: Vec::new(),
            tokens: self.tokens.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let registry = self.registry.clone();
        let meta_loop =
            MetaLoop::new(ledger, tx.clone(), ProcessLauncher, cfg).on_change(move |ledger| {
                let views = ledger.views();
                registry
                    .lock()
                    .unwrap()
                    .changed(LedgerRef::Meta, &|mask| views.stamp(mask));
            });
        let thread = std::thread::spawn(move || meta_loop.run(rx));
        *self.meta.lock().unwrap() = Some((LedgerHandle::new(tx, LedgerRef::Meta), thread));
        Ok(())
    }

    pub(crate) fn meta(&self) -> Option<LedgerHandle<MetaViews>> {
        self.meta.lock().unwrap().as_ref().map(|(h, _)| h.clone())
    }

    /// Records a new job (`JobOpened`, by the host) and starts its loop. A job that is
    /// already open (a retried start) is left as it is.
    pub(crate) fn create_job(&self, info: argus_core::event::JobInfo) -> std::io::Result<()> {
        let job = info.job;
        if self.jobs.lock().unwrap().contains_key(&job) {
            return Ok(());
        }
        let mut ledger = self.store.open_job(job).map_err(io_error)?;
        ledger
            .submit(
                argus_core::event::Author::Host,
                argus_core::event::Event::JobOpened(info),
            )
            .map_err(io_error)?;
        drop(ledger);
        self.open_job(job)?;
        self.registry.lock().unwrap().job_set_changed();
        Ok(())
    }

    pub(crate) fn job(&self, job: JobId) -> Option<LedgerHandle> {
        self.jobs.lock().unwrap().get(&job).map(|(h, _)| h.clone())
    }

    pub(crate) fn job_ids(&self) -> Vec<JobId> {
        self.jobs.lock().unwrap().keys().copied().collect()
    }

    fn stop_all(&self) {
        let jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
        for (handle, thread) in jobs.into_values() {
            let _ = handle.sender().send(Cmd::Stop);
            let _ = thread.join();
        }
        if let Some((handle, thread)) = self.meta.lock().unwrap().take() {
            let _ = handle.sender().send(Cmd::Stop);
            let _ = thread.join();
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn io_error(e: argus_core::LedgerError) -> std::io::Error {
    std::io::Error::other(format!("{e:?}"))
}

/// Starts runs for real: processes, PTYs, in-process effects.
pub(crate) struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn start(
        &mut self,
        inv: Invocation,
        env: PlanEnv,
        ws: runner::Workspace,
        ledger: Sender<Cmd>,
    ) -> runner::Running {
        runner::start(inv, env, ws, ledger)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SubscriptionId(pub u64);

/// How long the pulse waits after a change before re-evaluating, so a burst of batches
/// costs one evaluation.
const COALESCE: std::time::Duration = std::time::Duration::from_millis(100);

impl Host {
    /// Opens the store (taking every lock), starts ledger threads for open jobs, and
    /// reconciles: each ledger's `runs.due(&empty)` abandons interrupted attempts and
    /// releases unclosed leases before anything new starts.
    ///
    /// A job directory without a `JobOpened` is skipped. If any job cannot be opened (another
    /// host holds it), the loops started so far are stopped and the error returned.
    pub fn start(config: HostConfig) -> std::io::Result<Host> {
        let store = Store::open(config.data_root.clone()).map_err(io_error)?;
        let (wake, woken) = std::sync::mpsc::channel();
        let inner = Arc::new(HostInner {
            config,
            store,
            jobs: Mutex::new(BTreeMap::new()),
            meta: Mutex::new(None),
            registry: Arc::new(Mutex::new(pulse::Registry::new(wake))),
            diffs: git::DiffCache::default(),
            pulse: Mutex::new(None),
            tokens: tokens::RunTokens::default(),
        });
        inner.open_meta()?;
        for job in inner.store.jobs().map_err(io_error)? {
            if let Err(e) = inner.open_job(job) {
                inner.stop_all();
                return Err(e);
            }
        }
        let weak = Arc::downgrade(&inner);
        let pulse = std::thread::spawn(move || {
            // Ends when the registry closes (shutdown) and drops the sender.
            while woken.recv().is_ok() {
                std::thread::sleep(COALESCE);
                while woken.try_recv().is_ok() {}
                let Some(inner) = weak.upgrade() else { return };
                pulse::flush(&inner.registry, &mut |caller, op, input| {
                    let mut h = handler::ApiHandler::new(&inner);
                    let value = argus_api::dispatch(&mut h, caller, op, input)?;
                    Ok((value, h.deps))
                });
            }
        });
        *inner.pulse.lock().unwrap() = Some(pulse);
        Ok(Host { inner })
    }

    /// One-shot op (query, command, or the current value of a subscription).
    pub fn call(&self, caller: &Caller, op: &str, input: &Json) -> Result<Json, ApiError> {
        argus_api::dispatch(
            &mut handler::ApiHandler::new(&self.inner),
            caller,
            op,
            input,
        )
    }

    /// Pushes the op's value now and again whenever an input it read changes
    /// (`pulse.rs`: tracked view reads, coalesced to ~100 ms, pushed only when the JSON
    /// differs). No per-screen push code exists, so desktop and phone cannot see
    /// different state.
    pub fn subscribe(
        &self,
        caller: &Caller,
        op: &str,
        input: Json,
        sink: Box<dyn FnMut(Json) + Send>,
    ) -> Result<SubscriptionId, ApiError> {
        let info = argus_api::OPS
            .iter()
            .find(|o| o.name == op)
            .ok_or_else(|| ApiError::UnknownOp { op: op.into() })?;
        if info.kind != argus_api::OpKind::Subscribe {
            return Err(ApiError::BadInput {
                path: "op".into(),
                what: format!("{op} is not a subscription"),
            });
        }
        let mut h = handler::ApiHandler::new(&self.inner);
        let value = argus_api::dispatch(&mut h, caller, op, &input)?;
        let sink: pulse::Sink = Arc::new(Mutex::new(sink));
        (sink.lock().unwrap())(value.clone());
        let sub = pulse::Subscription {
            caller: caller.clone(),
            op: op.into(),
            input,
            deps: h.deps,
            last: Some(value),
            dirty: true,
            sink,
        };
        Ok(self.inner.registry.lock().unwrap().insert(sub))
    }

    /// No push starts after this returns; one already being delivered may still land.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.inner.registry.lock().unwrap().remove(id);
    }

    /// File behind an `Evidence.blob` id (screenshots, recordings) for the transport to
    /// serve: Tauri custom protocol `argus-blob://<id>`, or `GET /blob/<id>` for the phone.
    pub fn blob_path(&self, job: &str, blob: &str) -> Option<PathBuf> {
        let job = argus_core::ids::Id128::parse(job)?;
        if blob.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(blob.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        let dir = self.inner.store.root().join("jobs").join(job.to_string());
        Some(
            argus_log::LogDir::new(dir)
                .blobs()
                .path(&argus_log::BlobId(bytes)),
        )
    }

    /// Stops the pulse first (its evaluations read the ledgers), then every ledger's loop.
    /// Runs still executing are stopped with them (a dropped `Running` is a cancel), and
    /// the next start abandons or retries them.
    pub fn shutdown(self) {
        self.inner.registry.lock().unwrap().close();
        let pulse = self.inner.pulse.lock().unwrap().take();
        if let Some(pulse) = pulse {
            let _ = pulse.join();
        }
        self.inner.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use argus_core::event::{
        AgentLaunch, Author, Event, JobInfo, JobOrigin, RunOutcome, RunPurpose,
    };
    use argus_core::ids::{Attempt, GitOid, Id128, RunId};
    use argus_core::models::{AgentCli, ModelChoice, ModelPresets, ModelSet, PresetName};
    use argus_core::plan::ToolPaths;

    fn scratch() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "argus-hostlib-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    pub(crate) fn config(data_root: PathBuf) -> HostConfig {
        let opus = ModelChoice {
            cli: AgentCli::Claude,
            model: argus_core::ids::ModelId("opus".into()),
            effort: None,
        };
        let name = PresetName::parse("default").unwrap();
        let presets = ModelPresets::new(
            BTreeMap::from([(name.clone(), ModelSet::new([opus]).unwrap())]),
            name,
        )
        .unwrap();
        HostConfig {
            socket: data_root.join("argus.sock"),
            data_root,
            phone: None,
            // No test may start a real tool: agent CLIs and EAS cost money and write
            // outside the test, checkers are slow. A run that reaches one fails to spawn.
            tools: ToolPaths {
                claude: "/nonexistent/argus-test/claude".into(),
                codex: "/nonexistent/argus-test/codex".into(),
                java: "/nonexistent/argus-test/java".into(),
                tla2tools_jar: "/nonexistent/argus-test/tla2tools.jar".into(),
                tlars: "/nonexistent/argus-test/tlars".into(),
                lake: "/nonexistent/argus-test/lake".into(),
                eas: "/nonexistent/argus-test/eas".into(),
                script: "script".into(),
            },
            policy: PolicyConfig::default(),
            presets,
            packet: PacketConfig {
                sensitive: Vec::new(),
            },
            snapshot_deny: Vec::new(),
            retention: Retention {
                terminal_days: Some(30),
            },
            tz: UtcOffset { minutes: 540 },
            skill_dirs: Vec::new(),
        }
    }

    pub(crate) fn job_info(job: JobId, root: &std::path::Path) -> JobInfo {
        JobInfo {
            job,
            title: "fix the gate".into(),
            repo: root.join("repo"),
            base: GitOid::parse(&"b".repeat(40)).unwrap(),
            target: "main".into(),
            worktree: root.join("worktree"),
            group: None,
            origin: JobOrigin::Launched,
        }
    }

    /// A job whose agent was running when the previous host died.
    fn interrupted_job(root: &std::path::Path, job: JobId, run: RunId) {
        let store = Store::open(root.to_path_buf()).unwrap();
        let mut ledger = store.open_job(job).unwrap();
        ledger
            .submit(Author::Host, Event::JobOpened(job_info(job, root)))
            .unwrap();
        let launch = AgentLaunch {
            cli: AgentCli::Claude,
            model: None,
            prompt: "go".into(),
            resume: None,
        };
        ledger
            .submit(
                Author::Human,
                Event::RunRequested {
                    run,
                    purpose: RunPurpose::Agent(launch),
                },
            )
            .unwrap();
        ledger
            .submit(
                Author::Host,
                Event::RunStarted {
                    run,
                    attempt: Attempt(0),
                    pid: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn a_restarted_host_abandons_what_was_running_and_knows_its_jobs() {
        let root = scratch();
        let (job, run) = (JobId(Id128(7)), RunId(Id128(1)));
        interrupted_job(&root, job, run);
        // A directory without a `JobOpened` is skipped, not fatal.
        Store::open(root.clone())
            .unwrap()
            .open_job(JobId(Id128(8)))
            .unwrap();

        let host = Host::start(config(root.clone())).unwrap();
        assert!(host.inner.jobs.lock().unwrap().contains_key(&job));
        host.shutdown();

        let ledger = Store::open(root).unwrap().open_job(job).unwrap();
        let attempts = &ledger.views().runs().get(&run).unwrap().attempts;
        assert_eq!(
            attempts[0].finished.as_ref().map(|f| f.1.clone()),
            Some(RunOutcome::Abandoned)
        );
        // An agent is not retried: a human decides.
        assert_eq!(attempts.len(), 1);
    }

    #[test]
    fn a_second_host_on_the_same_store_is_refused() {
        let root = scratch();
        interrupted_job(&root, JobId(Id128(7)), RunId(Id128(1)));
        let host = Host::start(config(root.clone())).unwrap();
        assert!(Host::start(config(root)).is_err());
        host.shutdown();
    }

    #[test]
    fn a_blob_is_found_by_its_job_and_hex_id() {
        let root = scratch();
        let job = JobId(Id128(7));
        interrupted_job(&root, job, RunId(Id128(1)));
        let host = Host::start(config(root.clone())).unwrap();
        let hex = "ab".repeat(32);
        let path = host.blob_path(&job.0.to_string(), &hex).unwrap();
        assert!(
            path.starts_with(root.join("jobs").join(job.0.to_string())),
            "{}",
            path.display()
        );
        assert!(
            path.ends_with(format!("ab/{}", &hex[2..])),
            "{}",
            path.display()
        );
        assert_eq!(host.blob_path("not-a-job", &hex), None);
        assert_eq!(host.blob_path(&job.0.to_string(), "zz"), None);
        host.shutdown();
    }
}
