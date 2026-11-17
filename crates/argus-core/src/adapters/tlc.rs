//! TLA+ model checking through java TLC or tlars. Both speak `-tool` mode, so the
//! command differs and the decoder does not.

use crate::event::{CheckRequest, CheckSpec, TlaEngine};
use crate::plan::{CommandSpec, PlanEnv};
use crate::source::{Decoded, TlcMessage};

pub(crate) const VERSION: u32 = 1;

/// JavaTlc: `java -XX:+UseParallelGC -cp <tla2tools.jar> tlc2.TLC -tool [-workers N]
///           [-config C.cfg] Spec.tla`
/// Tlars:   `tlars -tool [-workers N] [-config C.cfg] Spec.tla`
/// Workdir: `Snapshot { tree, base }`. stdout: `TlcTool`. stderr: `PlainText`.
pub(crate) fn command(req: &CheckRequest, env: &PlanEnv) -> CommandSpec {
    let CheckSpec::Tla { engine, .. } = &req.spec else {
        unreachable!("plan() dispatches Tla only")
    };
    match engine {
        TlaEngine::JavaTlc => todo!("not implemented"),
        TlaEngine::Tlars => todo!("not implemented"),
    }
}

/// One `@!@!@STARTMSG code:class @!@!@ ... @!@!@ENDMSG code @!@!@` block (the framer
/// cut it), or a stray line -> `Line`.
pub(crate) fn decode(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}

/// Verdict from the message stream and exit code. Pure; used by `views::checks`.
///
/// TODO: success (finished, no error codes) -> Proved; invariant/property violation,
/// deadlock -> Refuted with the error-trace messages' fact ids; parse/semantic errors
/// -> ToolError. Cross-check against the exit code; disagreement -> ToolError
/// (this also catches tlars/TLC divergence).
pub(crate) fn verdict(
    messages: &[TlcMessage],
    exit_code: i32,
) -> crate::views::checks::CheckVerdict {
    todo!("not implemented")
}
