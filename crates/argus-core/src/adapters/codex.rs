//! Codex CLI. Sketch only: launches in a PTY; its rollout and `exec --json` lines decode
//! to `Unrecognized` until the adapter is written, so the raw view works from day one.

use crate::command::{CommandSpec, PermissionAnswer, PlanEnv};
use crate::event::AgentLaunch;
use crate::source::{Decoded, Observation};

pub(crate) const VERSION: u32 = 0;

/// `codex [--model m] [resume <session>] <prompt>` under a PTY; watch the rollout file.
pub(crate) fn launch(launch: &AgentLaunch, env: &PlanEnv) -> CommandSpec {
    todo!("not implemented")
}

/// Not verified against a real prompt yet, so no answer has keys.
pub(crate) fn permission_keys(_answer: PermissionAnswer) -> Option<Vec<u8>> {
    None
}

pub(crate) fn decode_rollout(bytes: &[u8]) -> Decoded {
    Decoded::one(Observation::Unrecognized {
        reason: "codex rollout adapter not written".into(),
    })
}

pub(crate) fn decode_exec(bytes: &[u8]) -> Decoded {
    Decoded::one(Observation::Unrecognized {
        reason: "codex exec adapter not written".into(),
    })
}
