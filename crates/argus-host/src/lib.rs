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
mod handler;
mod lease;
mod ledger_thread;
mod pulse;
mod runner;
mod tail;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;

use argus_api::{ApiError, Caller};
use argus_core::models::ModelPresets;
use argus_core::period::UtcOffset;
use argus_core::plan::ToolPaths;
use argus_core::policy::PolicyConfig;
use argus_core::views::packet::PacketConfig;
use argus_json::Json;

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
    // store, config, ledger thread handles by JobId, pulse::Registry (behind a short
    // mutex: marking dirty never waits on a ledger), git::DiffCache, live run set.
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SubscriptionId(pub u64);

impl Host {
    /// Opens the store (taking every lock), starts ledger threads for open jobs, and
    /// reconciles: each ledger's `runs.due(&empty)` abandons interrupted attempts and
    /// releases unclosed leases before anything new starts.
    pub fn start(config: HostConfig) -> std::io::Result<Host> {
        todo!("not implemented")
    }

    /// One-shot op (query, command, or the current value of a subscription).
    pub fn call(&self, caller: &Caller, op: &str, input: &Json) -> Result<Json, ApiError> {
        // TODO: argus_api::dispatch(&mut handler::ApiHandler::new(&self.inner), caller, op, input)
        todo!("not implemented")
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
        todo!("not implemented")
    }

    pub fn unsubscribe(&self, id: SubscriptionId) {
        todo!("not implemented")
    }

    /// File behind an `Evidence.blob` id (screenshots, recordings) for the transport to
    /// serve: Tauri custom protocol `argus-blob://<id>`, or `GET /blob/<id>` for the phone.
    pub fn blob_path(&self, job: &str, blob: &str) -> Option<PathBuf> {
        todo!("not implemented")
    }

    pub fn shutdown(self) {
        todo!("not implemented")
    }
}
