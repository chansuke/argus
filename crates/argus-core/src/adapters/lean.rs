//! Lean 4 via lake.

use crate::event::{CheckRequest, CheckSpec, LeanMode};
use crate::plan::{CommandSpec, PlanEnv};
use crate::source::{Decoded, LeanMessage};

pub(crate) const VERSION: u32 = 1;

/// `LakeBuild`: `lake build` in `root` (stdout `LeanText`).
/// `Files`: `lake env lean --json <file>` per file, run sequentially by a wrapper script
/// (stdout `LeanJson`).
pub(crate) fn command(req: &CheckRequest, env: &PlanEnv) -> CommandSpec {
    let CheckSpec::Lean { mode, .. } = &req.spec else {
        unreachable!("plan() dispatches Lean only")
    };
    match mode {
        LeanMode::LakeBuild => todo!("not implemented"),
        LeanMode::Files(_) => todo!("not implemented"),
    }
}

/// `path:line:col: error: text` lines.
pub(crate) fn decode_text(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}

pub(crate) fn decode_json(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}

/// Errors -> ToolError/Refuted; no errors but "declaration uses 'sorry'" warnings ->
/// `Holes { sorries }` (green build, unfinished proof: never shown as proved).
pub(crate) fn verdict(
    messages: &[LeanMessage],
    exit_code: i32,
) -> crate::views::checks::CheckVerdict {
    todo!("not implemented")
}
