//! LLM-backed runs (review, summaries, insights) through headless agent CLIs.
//! Model choice = which CLI + `--model`. No API keys or HTTP client in argus.

use crate::event::{LlmCall, ReviewLens, ReviewSpec};
use argus_json::Json;

use crate::ids::{CommitOid, LineRange, RepoPath, SkillName, UnixMillis};
use crate::models::{AgentCli, Effort};
use crate::plan::{CommandSpec, PlanEnv, Workdir};
use crate::source::SourceKind;

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
pub(crate) fn parse_findings(result_text: &str) -> Option<Vec<crate::source::RawFinding>> {
    use crate::source::{RawFinding, Severity};
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

/// The first 8 hex digits of an id: enough to tell lanes apart in a prompt.
fn short(id: &crate::ids::Id128) -> String {
    id.to_string()[..8].to_owned()
}

/// One job's timeline inside an insight period, oldest first.
pub(crate) struct InsightSession<M> {
    pub job: crate::ids::JobId,
    pub title: String,
    pub items: Vec<(UnixMillis, crate::ids::FactId, M)>,
}

/// A moment of a session as an insight prompt shows it. The timeline implements it, so
/// this adapter only formats lines and never reads a view.
pub(crate) trait InsightMoment {
    /// What kind of moment it is (`human typed`, `tool Bash failed`), or `None` when it
    /// is not worth learning from (replies, thinking, calls that worked).
    fn lesson(&self) -> Option<String>;
    fn title(&self) -> &str;
}

/// Tagged lines in one insight prompt; past this the rest is left out, and the prompt
/// says how many.
const INSIGHT_LINES: usize = 2000;
/// Starts the tag -> fact table at the end of an insight prompt. The prompt is stored
/// verbatim in the request, so `parse_candidates` resolves tags from it alone.
const EVIDENCE_INDEX: &str = "argus evidence index (read by argus, not by you):";

/// The prompt of an `Insight` run: the moments worth learning from in each session, one
/// short tagged line each (`[E12] 14:03 tool Bash failed: cargo test`). The model cites
/// tags instead of copying long fact ids; the table at the end maps them back.
///
/// Kept: prompts, what the human typed, failed tool calls, decisions, comments and
/// bookmarks, findings. Replies, thinking, and calls that worked are left out.
pub(crate) fn insight_prompt<M: InsightMoment>(
    period: &crate::period::Period,
    existing_skills: &[SkillName],
    sessions: &[InsightSession<M>],
) -> String {
    let utc = crate::period::UtcOffset { minutes: 0 };
    let minute = |t: UnixMillis| {
        let m = t.0.div_euclid(60_000).rem_euclid(1440);
        format!("{:02}:{:02}", m / 60, m % 60)
    };
    let mut out = format!(
        "These are the logs of coding sessions between a human and coding agents, from \
         {} {} to {} {} UTC.\n\n\
         Find what went wrong more than once or cost the human time: corrections the human \
         had to type, tool calls that kept failing, review findings, notes the human \
         bookmarked. For each lesson worth keeping, propose a skill: instructions a coding \
         agent reads before it starts, so the same thing goes right next time without the \
         human stepping in.\n\n\
         Every line below is tagged like [E1]. Cite the tags of the lines that show the \
         problem. Propose a skill only when the lines support it, and prefer a few sharp \
         skills to many vague ones.\n",
        utc.date(period.from),
        minute(period.from),
        utc.date(period.to),
        minute(period.to),
    );
    if existing_skills.is_empty() {
        out.push_str("\nNo skills exist yet.\n");
    } else {
        let names: Vec<&str> = existing_skills.iter().map(SkillName::as_str).collect();
        out.push_str(&format!(
            "\nThese skills already exist: {}. To improve one, use its exact name and write \
             its whole new body; do not add a copy under another name.\n",
            names.join(", ")
        ));
    }
    let mut index = Vec::new();
    let mut left_out = 0usize;
    for session in sessions {
        let mut lines = String::new();
        let mut day = String::new();
        for (at, fact, item) in &session.items {
            let Some(label) = item.lesson() else {
                continue;
            };
            if index.len() == INSIGHT_LINES {
                left_out += 1;
                continue;
            }
            if utc.date(*at) != day {
                day = utc.date(*at);
                lines.push_str(&format!("{day}\n"));
            }
            index.push((session.job, fact.clone()));
            lines.push_str(&format!(
                "[E{}] {} {label}: {}\n",
                index.len(),
                minute(*at),
                printable(item.title())
            ));
        }
        if !lines.is_empty() {
            out.push_str(&format!(
                "\nSession {} \"{}\":\n{lines}",
                short(&session.job.0),
                printable(&session.title)
            ));
        }
    }
    if index.is_empty() {
        out.push_str("\nNothing happened in this period.\n");
    }
    if left_out > 0 {
        out.push_str(&format!("\n({left_out} more lines were left out.)\n"));
    }
    out.push_str(
        "\nEnd your answer with exactly one fenced json block that lists your proposals, \
         like this:\n\n\
         ```json\n\
         [{\"name\": \"read-the-error-first\", \"description\": \"Use when a command \
         fails.\", \"why\": \"The same command failed three times before the human pointed \
         at the error.\", \"body\": \"Read the whole error message before you run the \
         command again.\", \"evidence\": [\"E1\", \"E2\"]}]\n\
         ```\n\n\
         `name` is lowercase words joined by hyphens. `description` says when an agent \
         should use the skill. `body` is the skill itself, in Markdown. Use [] when nothing \
         is worth a skill.\n",
    );
    out.push_str(&format!("\n{EVIDENCE_INDEX}\n"));
    for (n, (job, fact)) in index.iter().enumerate() {
        out.push_str(&format!("E{} {} {fact}\n", n + 1, job.0));
    }
    out
}

/// Control characters (a typed Escape, say) shown as escapes, so a line stays one line.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Candidates from an insight run's final text; tags resolve through the index at the end
/// of `prompt`. `None` when the block is missing or any proposal lacks a name, why,
/// description, or body. A tag the prompt never gave is dropped, not trusted.
pub(crate) fn parse_candidates(
    prompt: &str,
    result_text: &str,
) -> Option<Vec<crate::source::RawCandidate>> {
    use crate::ids::{FactId, FactRef, Id128, JobId, LedgerRef};
    use crate::source::RawCandidate;
    let index: std::collections::BTreeMap<&str, FactRef> = prompt
        .rfind(EVIDENCE_INDEX)
        .map(|at| &prompt[at + EVIDENCE_INDEX.len()..])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let tag = parts.next()?;
            let job = JobId(Id128::parse(parts.next()?)?);
            let fact = FactId::parse(parts.next()?)?;
            Some((
                tag,
                FactRef {
                    ledger: LedgerRef::Job(job),
                    fact,
                },
            ))
        })
        .collect();
    let block = last_json_block(result_text)?;
    let Json::Arr(items) = argus_json::parse(block.as_bytes()).ok()? else {
        return None;
    };
    items
        .iter()
        .map(|item| {
            let text = |key| item.get(key).and_then(Json::as_str);
            let filled = |key| text(key).map(str::trim).filter(|t| !t.is_empty());
            let mut evidence: Vec<FactRef> = Vec::new();
            if let Some(Json::Arr(tags)) = item.get("evidence") {
                for fact in tags.iter().filter_map(|t| index.get(t.as_str()?)) {
                    if !evidence.contains(fact) {
                        evidence.push(fact.clone());
                    }
                }
            }
            Some(RawCandidate {
                name: filled("name")?.to_owned(),
                description: text("description")?.trim().to_owned(),
                why: text("why")?.trim().to_owned(),
                body: filled("body")?.to_owned(),
                evidence,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Severity;

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

    fn session() -> InsightSession<crate::views::timeline::Item> {
        use crate::ids::{FactId, Id128, JobId, RunId, ToolName};
        use crate::views::timeline::{Item, ItemKind, Lane};
        let item = |lane, kind, title: &str| Item {
            lane,
            kind,
            title: title.into(),
            ended_at: None,
        };
        let agent = Lane::Agent(RunId(Id128(7)));
        let tool = |ok| ItemKind::ToolCall {
            name: ToolName("Bash".into()),
            ok,
        };
        let at = |min: i64| UnixMillis(min * 60_000);
        let fact = |s: &str| FactId::parse(s).unwrap();
        InsightSession {
            job: JobId(Id128(1)),
            title: "fix the gate".into(),
            items: vec![
                (
                    at(0),
                    fact("human:0.0"),
                    item(agent.clone(), ItemKind::Prompt, "make the gate pass"),
                ),
                (
                    at(1),
                    fact("src-a:1.0"),
                    item(agent.clone(), ItemKind::Reply, "working on it"),
                ),
                (
                    at(2),
                    fact("src-a:2.0"),
                    item(agent.clone(), tool(Some(false)), "cargo test"),
                ),
                (
                    at(3),
                    fact("src-a:3.0"),
                    item(agent.clone(), tool(Some(true)), "cargo fmt"),
                ),
                (
                    at(4),
                    fact("human:1.0"),
                    item(Lane::Human, ItemKind::HumanInput, "run fmt first"),
                ),
                (
                    at(5),
                    fact("human:2.0"),
                    item(Lane::Human, ItemKind::Comment, "Pitfall: fmt before test"),
                ),
            ],
        }
    }

    fn period() -> crate::period::Period {
        crate::period::Period {
            from: UnixMillis(0),
            to: UnixMillis(86_400_000),
        }
    }

    #[test]
    fn an_insight_prompt_tags_only_the_moments_worth_learning_from() {
        let skills = [SkillName::parse("run-fmt").unwrap()];
        let prompt = insight_prompt(&period(), &skills, &[session()]);
        assert!(
            prompt.contains("1970-01-01 00:00 to 1970-01-02 00:00 UTC"),
            "{prompt}"
        );
        assert!(prompt.contains("fix the gate"), "{prompt}");
        assert!(prompt.contains("run-fmt"), "{prompt}");
        assert!(
            prompt.contains("[E1] 00:00 prompt: make the gate pass"),
            "{prompt}"
        );
        assert!(
            prompt.contains("[E2] 00:02 tool Bash failed: cargo test"),
            "{prompt}"
        );
        assert!(
            prompt.contains("[E3] 00:04 human typed: run fmt first"),
            "{prompt}"
        );
        assert!(
            prompt.contains("[E4] 00:05 comment: Pitfall: fmt before test"),
            "{prompt}"
        );
        assert!(!prompt.contains("working on it"), "{prompt}");
        assert!(!prompt.contains("cargo fmt"), "{prompt}");
        assert!(!prompt.contains("[E5]"), "{prompt}");
    }

    #[test]
    fn the_example_in_the_insight_prompt_is_what_the_parser_reads() {
        use crate::ids::{FactId, FactRef, Id128, JobId, LedgerRef};
        let prompt = insight_prompt(&period(), &[], &[session()]);
        let found = parse_candidates(&prompt, &prompt).expect("the example block parses");
        assert_eq!(found.len(), 1);
        let job = LedgerRef::Job(JobId(Id128(1)));
        let cite = |f: &str| FactRef {
            ledger: job,
            fact: FactId::parse(f).unwrap(),
        };
        assert_eq!(found[0].evidence, [cite("human:0.0"), cite("src-a:2.0")]);
        assert!(!found[0].name.is_empty() && !found[0].body.is_empty());
    }

    #[test]
    fn candidates_cite_only_tags_the_prompt_gave_and_bad_output_is_not_read() {
        let prompt = insight_prompt(&period(), &[], &[session()]);
        let answer = |json: &str| format!("Here you go.\n\n```json\n{json}\n```\n");
        let found = parse_candidates(
            &prompt,
            &answer(r#"[{"name":"a","description":"d","why":"w","body":"b","evidence":["E4","E9","x"]}]"#),
        )
        .unwrap();
        assert_eq!(found[0].evidence.len(), 1, "E9 and x were never given");
        assert_eq!(
            parse_candidates(&prompt, &answer("[]")).map(|c| c.len()),
            Some(0)
        );
        for bad in [
            "no block at all",
            &answer("{}"),
            &answer(r#"[{"name":"a","description":"d","why":"w","evidence":[]}]"#),
            &answer(r#"[{"name":"","description":"d","why":"w","body":"b","evidence":[]}]"#),
        ] {
            assert!(parse_candidates(&prompt, bad).is_none(), "{bad}");
        }
    }
}
