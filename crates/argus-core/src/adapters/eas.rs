//! EAS Simulators (remote iOS Simulator / Android Emulator on Expo infra).
//! CLI reference: https://docs.expo.dev/eas/simulators/reference/cli/
//!
//! Confirmed from that reference: `simulator:start` takes `--platform`, `--name`,
//! repeatable `--tag`, one app source (`--build-id | --build-fingerprint |
//! --application-archive-url | --expo-go`), `--max-duration-minutes` (default 115),
//! `--max-idle-time-minutes`, `--non-interactive`, `--json` (prints `id`, `name`,
//! `deviceRunSessionUrl`, `remoteConfig`; `remoteConfig` is secret). `simulator:exec`
//! runs a command with the session's connection env loaded. `simulator:events --follow
//! --json`, `simulator:logs --output --artifact` (iOS), `simulator:list --tag --json`,
//! `simulator:stop --id --json`.
//! Unverified: the exact JSON field names of `list`/`events`, and whether `--egress local`
//! is how a remote device reaches a local Metro for JS-only changes.

use crate::event::{CheckRequest, DevicePlatform};
use crate::plan::{CommandSpec, Invocation, PlanEnv};
use crate::source::Decoded;

pub(crate) const VERSION: u32 = 1;

/// `Leased { lease, inner }` where:
/// - lease name = `argus-<lease-id>`; lease id derived from (run, attempt), so a retried
///   plan names the same session.
/// - acquire = `eas simulator:start --platform <p> --name <name> --tag argus --tag <name>
///   <app source> --max-duration-minutes <budget> --max-idle-time-minutes <idle>
///   --non-interactive --json --out-config-type env` in `Workdir::Snapshot`.
/// - inner = `eas simulator:exec -- <driver>`; driver = repo script, or a headless LLM agent
///   instructed to verify the change with agent-device and save screenshots/recordings
///   into `$ARGUS_ARTIFACT_DIR`. `artifacts: true`.
pub(crate) fn device_check(req: &CheckRequest, env: &PlanEnv) -> Invocation {
    todo!("not implemented")
}

pub(crate) fn parse_started(stdout: &[u8]) -> Option<(String, Option<String>)> {
    todo!("not implemented")
}
pub(crate) fn follow_events(remote_id: &str, env: &PlanEnv) -> CommandSpec {
    todo!("not implemented")
}
/// iOS: `eas simulator:logs --id <id> --output $ARGUS_ARTIFACT_DIR/device.log`. Android: none.
pub(crate) fn collect_logs(
    platform: DevicePlatform,
    remote_id: &str,
    env: &PlanEnv,
) -> Option<CommandSpec> {
    todo!("not implemented")
}
pub(crate) fn stop(remote_id: &str, env: &PlanEnv) -> CommandSpec {
    todo!("not implemented")
}
pub(crate) fn find_by_tag(lease_name: &str, env: &PlanEnv) -> CommandSpec {
    todo!("not implemented")
}
pub(crate) fn parse_list(stdout: &[u8]) -> Vec<String> {
    todo!("not implemented")
}
pub(crate) fn decode_events(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}
