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
    let base = base.as_str();
    let lens = match lens {
        ReviewLens::General => "correctness, design, readability, and tests",
        ReviewLens::Correctness => {
            "correctness: wrong results, missed edge cases, error handling, and concurrency"
        }
        ReviewLens::Security => {
            "security: injection, leaked secrets, missing authorization, and unchecked input"
        }
        ReviewLens::Tests => "tests: missing cases, weak assertions, and tests that cannot fail",
    };
    let mut out = format!(
        "Review the change in this checkout against commit {base}. \
         Run `git diff {base}` to see it and read any file you need. Do not edit files.\n\n\
         Look at {lens}.\n"
    );
    if let Some(focus) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        out.push_str(&format!(
            "The author asks you to look closely at: {focus}\n"
        ));
    }
    out.push_str(
        "\nWrite your review as prose. End it with exactly one fenced json block that lists \
         your findings, like this:\n\n\
         ```json\n\
         [{\"path\": \"src/lib.rs\", \"start\": 10, \"end\": 12, \"severity\": \"high\", \
         \"title\": \"one line\", \"body\": \"why it is wrong and what to do\"}]\n\
         ```\n\n\
         `severity` is one of high, medium, low, nit. `path` is relative to the repository \
         root, and lines are in the new version of the file. Use null for path, start, and \
         end when a finding has no location. Use [] when you find nothing.\n",
    );
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::review::Severity;

    #[test]
    fn the_example_in_the_review_prompt_is_what_the_parser_reads() {
        let base = CommitOid::parse(&"b".repeat(40)).unwrap();
        let prompt = review_prompt(&base, ReviewLens::General, None);
        let findings = parse_findings(&prompt).expect("the example block parses");
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.path.as_ref().map(|p| p.as_str()), Some("src/lib.rs"));
        assert_eq!(f.lines, LineRange::new(10, 12));
        assert_eq!(f.severity, Severity::High);
    }
}
