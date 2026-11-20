//! LLM-backed runs (review, summaries, insights) through headless agent CLIs.
//! Model choice = which CLI + `--model`. No API keys or HTTP client in argus.

use crate::event::{LlmCall, ReviewLens, ReviewSpec};
use argus_json::Json;

use crate::ids::{CommitOid, LineRange, RepoPath, UnixMillis};
use crate::models::{AgentCli, Effort};
use crate::plan::{CommandSpec, PlanEnv, Workdir};
use crate::source::SourceKind;
use crate::views::timeline::{Item, ItemKind, Lane, Window};

#[derive(Clone, Copy, Debug)]
pub(crate) enum ToolAccess {
    None,
    /// Read, Grep, Glob, `git diff`/`git log` only: a reviewer explores, never edits.
    ReadOnly,
}

/// Claude: `claude -p --model <m> [--effort e] --output-format stream-json --verbose
///          --no-session-persistence --tools <set> [--allowedTools ...]`, prompt on
///          stdin, stdout `ClaudeStreamJson`. The stdout stream is the run's raw log, so
///          the session is not also saved under `~/.claude/projects`.
///          `ToolAccess::None` is `--tools ""`; `ReadOnly` is the read tools plus Bash
///          limited to `git diff`/`git log`/`git show`. Flags from Claude Code 2.1.303
///          `--help`; unverified: that print mode denies a Bash call outside the allowed
///          patterns instead of waiting for an answer.
/// Codex:  `codex exec --model <m> --json -`, stdout `CodexExec` (not written yet).
pub(crate) fn headless(
    call: &LlmCall,
    access: ToolAccess,
    workdir: Workdir,
    env: &PlanEnv,
) -> CommandSpec {
    match call.model.cli {
        AgentCli::Claude => {
            let mut args: Vec<String> =
                vec!["-p".into(), "--model".into(), call.model.model.0.clone()];
            if let Some(effort) = call.model.effort {
                let effort = match effort {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                    Effort::Max => "max",
                };
                args.extend(["--effort".into(), effort.into()]);
            }
            args.extend(
                [
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--no-session-persistence",
                ]
                .map(String::from),
            );
            match access {
                ToolAccess::None => args.extend(["--tools".into(), String::new()]),
                ToolAccess::ReadOnly => args.extend(
                    [
                        "--tools",
                        "Read,Grep,Glob,Bash",
                        "--allowedTools",
                        "Bash(git diff:*),Bash(git log:*),Bash(git show:*)",
                    ]
                    .map(String::from),
                ),
            }
            CommandSpec {
                program: env.tools.claude.clone(),
                args,
                env: Vec::new(),
                workdir,
                cwd: None,
                stdin: Some(call.prompt.clone().into_bytes()),
                stdout: SourceKind::ClaudeStreamJson,
                stderr: SourceKind::PlainText,
                terminal: false,
                watch: Vec::new(),
                artifacts: false,
                timeout: None,
            }
        }
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

/// The prompt of a `Summarize` run: the window's items, one line each, oldest first.
pub(crate) fn summary_prompt<'a>(
    window: &Window,
    items: impl Iterator<Item = (UnixMillis, &'a Item)>,
) -> String {
    let clock = |t: UnixMillis| {
        let s = t.0.div_euclid(1000).rem_euclid(86_400);
        format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    };
    let mut out = format!(
        "Summarize this part of a coding session, {} to {} UTC, in three to five short \
         sentences for someone who was away: what was done, what failed, and what is \
         waiting. Use only the events below.\n\nEvents:\n",
        clock(window.start),
        clock(window.end()),
    );
    for (at, item) in items {
        let lane = match &item.lane {
            Lane::Agent(run) => format!("agent {}", short(&run.0)),
            Lane::Run(run) => format!("run {}", short(&run.0)),
            Lane::Human => "human".into(),
            Lane::Host => "host".into(),
        };
        let kind = match &item.kind {
            ItemKind::ToolCall { name, ok } => match ok {
                Some(false) => format!("tool {} failed", name.0),
                _ => format!("tool {}", name.0),
            },
            other => format!("{other:?}").to_lowercase(),
        };
        out.push_str(&format!("{} [{lane}] {kind}: {}\n", clock(at), item.title));
    }
    out
}

/// The first 8 hex digits of an id: enough to tell lanes apart in a prompt.
fn short(id: &crate::ids::Id128) -> String {
    id.to_string()[..8].to_owned()
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

    #[test]
    fn a_summary_prompt_says_which_tool_calls_failed() {
        use crate::ids::{RunId, ToolName};
        use crate::views::timeline::{BucketWidth, Item, ItemKind, Lane, Window};
        let call = |ok| Item {
            lane: Lane::Agent(RunId(crate::ids::Id128(1))),
            kind: ItemKind::ToolCall {
                name: ToolName("Bash".into()),
                ok,
            },
            title: "Bash".into(),
            ended_at: None,
        };
        let window = Window {
            width: BucketWidth::M15,
            start: UnixMillis(0),
        };
        let items = [call(Some(false)), call(Some(true)), call(None)];
        let prompt = summary_prompt(&window, items.iter().map(|i| (UnixMillis(0), i)));
        assert_eq!(prompt.matches("tool Bash failed").count(), 1);
        assert_eq!(prompt.matches("] tool Bash: ").count(), 2);
        assert!(prompt.contains("00:00:00 to 00:15:00 UTC"), "{prompt}");
    }
}
