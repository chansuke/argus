//! LLM-backed runs (review, summaries, insights) through headless agent CLIs.
//! Model choice = which CLI + `--model`. No API keys or HTTP client in argus.

use crate::event::{LlmCall, ReviewLens, ReviewSpec};
use crate::ids::CommitOid;
use crate::models::AgentCli;
use crate::plan::{CommandSpec, PlanEnv, Workdir};

#[derive(Clone, Copy, Debug)]
pub(crate) enum ToolAccess {
    None,
    /// Read, Grep, Glob, `git diff`/`git log` only: a reviewer explores, never edits.
    ReadOnly,
}

/// Claude: `claude -p --model <m> --output-format stream-json --verbose
///          [--allowedTools ...]`, prompt on stdin, stdout `ClaudeStreamJson`.
/// Codex:  `codex exec --model <m> --json -`, stdout `CodexExec`.
pub(crate) fn headless(
    call: &LlmCall,
    access: ToolAccess,
    workdir: Workdir,
    env: &PlanEnv,
) -> CommandSpec {
    match call.model.cli {
        AgentCli::Claude => todo!("not implemented"),
        AgentCli::Codex => todo!("not implemented"),
    }
}

/// A reviewer judges the recorded snapshot in a throwaway checkout, read-only.
pub(crate) fn review(spec: &ReviewSpec, env: &PlanEnv) -> CommandSpec {
    headless(
        &spec.llm,
        ToolAccess::ReadOnly,
        Workdir::Snapshot {
            tree: spec.tree.clone(),
            base: spec.base.clone(),
        },
        env,
    )
}

/// Builds the prompt stored in `ReviewSpec.llm.prompt` (called only by
/// `ReviewSpec::panel`). The reviewer runs in a checkout of the snapshot and is told the
/// base commit, so it runs `git diff` itself instead of argus pasting a diff.
///
/// Output contract: prose, then one fenced `json` block:
/// `[{"path","start","end","severity":"high|medium|low|nit","title","body"}]`.
pub(crate) fn review_prompt(base: &CommitOid, lens: ReviewLens, focus: Option<&str>) -> String {
    todo!("not implemented")
}

/// Findings from the final `Result` text. `None` when the block is missing or malformed:
/// the review view then keeps the whole answer as `ReviewOutput::Unstructured`, never
/// dropped. Findings whose location is outside the diff are kept, unlocated.
pub(crate) fn parse_findings(result_text: &str) -> Option<Vec<crate::views::review::RawFinding>> {
    todo!("not implemented")
}

pub(crate) fn parse_candidates(
    result_text: &str,
) -> Option<Vec<crate::views::insights::RawCandidate>> {
    todo!("not implemented")
}
