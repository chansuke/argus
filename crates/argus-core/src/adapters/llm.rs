//! LLM-backed runs (review, summaries, insights) through headless agent CLIs.
//! Model choice = which CLI + `--model`. No API keys or HTTP client in argus.

use crate::event::{LlmCall, ReviewLens, ReviewSpec};
use argus_json::Json;

use crate::ids::{CommitOid, LineRange, RepoPath};
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
    use crate::views::review::{RawFinding, Severity};
    let block = last_json_block(result_text)?;
    let Json::Arr(items) = argus_json::parse(block.as_bytes()).ok()? else {
        return None;
    };
    items
        .iter()
        .map(|item| {
            let text = |key| item.get(key).and_then(Json::as_str);
            let line = |key| match item.get(key) {
                Some(Json::Num(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
                _ => None,
            };
            let severity = match text("severity")? {
                "high" => Severity::High,
                "medium" => Severity::Medium,
                "low" => Severity::Low,
                "nit" => Severity::Nit,
                _ => return None,
            };
            let start = line("start");
            Some(RawFinding {
                path: text("path").and_then(RepoPath::parse),
                lines: start.and_then(|s| LineRange::new(s, line("end").unwrap_or(s))),
                severity,
                title: text("title")?.to_owned(),
                body: text("body").unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// The body of the last fenced `json` block, if it is closed.
fn last_json_block(text: &str) -> Option<&str> {
    let open = text.rfind("```json")?;
    let body = &text[open + "```json".len()..];
    let body = body.strip_prefix('\r').unwrap_or(body);
    let body = body.strip_prefix('\n')?;
    let close = body.find("```")?;
    Some(&body[..close])
}

pub(crate) fn parse_candidates(
    result_text: &str,
) -> Option<Vec<crate::views::insights::RawCandidate>> {
    todo!("not implemented")
}
