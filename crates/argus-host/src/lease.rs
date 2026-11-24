//! The lease protocol for billed remote resources (EAS Simulators sessions). The order of
//! steps is the whole point: intent is durable before money is spent, and release is
//! attempted on every exit path, including after a host crash.

use std::sync::mpsc::Sender;

use argus_core::command::{CommandSpec, LeaseSpec, PlanEnv};
use argus_core::ids::LeaseId;
use argus_core::views::runs::LeaseState;

use crate::ledger_thread::Cmd;

/// TODO:
/// 1. submit(Host, LeaseOpened{lease, run, attempt, provider, name}) — fsynced by submit.
/// 2. run `lease.acquire` with stdout piped to memory only (never a source: it holds
///    `remoteConfig` credentials); `provider.parse_acquired` -> (remote_id, preview_url).
///    Failure -> LeaseClosed{Gone}; RunFinished{SpawnFailed}.
/// 3. submit(Host, LeaseBound{lease, remote_id, preview_url}).
/// 4. start `provider.follow(remote_id)` as an `EasEvents` source; run `inner` via
///    runner::spawn_capture (artifacts on).
/// 5. always (success, failure, timeout, cancel): run `provider.collect`, collect
///    artifacts, run `provider.release`, submit LeaseClosed{Stopped | Gone}.
pub(crate) fn run_leased(lease: LeaseSpec, inner: CommandSpec, env: PlanEnv, ledger: Sender<Cmd>) {
    todo!("not implemented")
}

/// Recovery for `Due::Release`: bound -> `release(remote_id)`; opened but never bound ->
/// `find(name)` (sessions are tagged with the lease name) and release each match; then
/// LeaseClosed. Idempotent: stopping a stopped session is reported as Gone.
pub(crate) fn release_orphan(
    lease: LeaseId,
    state: &LeaseState,
    env: PlanEnv,
    ledger: Sender<Cmd>,
) {
    todo!("not implemented")
}
