//! Codex CLI: `codex exec --json` lines (headless runs and agent runs), and the session
//! rollout files.
//! This module is the only code that knows either format.
//!
//! `exec --json` events follow `codex-rs/exec/src/exec_events.rs` (main branch):
//! <https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs>. The help
//! of codex-cli 0.171.0 confirms the flags used here. One real agent run (0.171.0,
//! 2026-11-26) confirmed the snake_case item types, and that `turn.completed` counts
//! cached input inside `input_tokens`: its uncached part equals the sum over the turn's
//! rollout `token_usage_record`s.

use argus_json::Json;

use super::touch::{inserted, shell_paths};
use crate::command::{CommandSpec, PermissionAnswer, PlanEnv, Workdir};
use crate::event::AgentLaunch;
use crate::ids::{RecordKey, ToolCallId, ToolName, UnixMillis};
use crate::source::{
    AgentEvent, AgentWhat, Decoded, FileTouch, Observation, PlanItem, PlanOp, PlanStatus,
    SourceKind, TokenUsage, TouchHow,
};

pub(crate) const VERSION: u32 = 4;

/// `codex exec --sandbox workspace-write --json [--model m] --skip-git-repo-check -` in
/// the job worktree, prompt on stdin, stdout `CodexAgentExec`: the agent works on its own
/// (no terminal), may write only inside the worktree, and its stdout is the run's session
/// log. Codex cannot be given a session id at start, so its rollout file is not known in
/// advance; the stdout carries the same turn (user decision, 2026-11-26).
///
/// Feedback (`launch.resume`) is `codex exec --sandbox workspace-write resume --last ...`:
/// the newest session recorded in that working directory, which is the job's previous
/// agent run because each job has its own worktree and its agent runs are sequential. A
/// Codex session someone starts by hand in the worktree would be picked instead.
///
/// Env: `ARGUS_RUN`, `ARGUS_SOCKET`, as for Claude. Flags from codex-cli 0.171.0
/// `exec --help` and `exec resume --help`; `--last` filters by the working directory
/// (its help says `--all` disables that filtering), and with one positional the
/// positional is the prompt (`codex-rs/exec`), so `-` reads stdin. Exec never waits for
/// an approval: its policy is `never`, so an action that needs one fails as a tool error
/// (seen in a real run for an MCP tool call).
pub(crate) fn launch(launch: &AgentLaunch, env: &PlanEnv) -> CommandSpec {
    let mut args: Vec<String> = ["exec", "--sandbox", "workspace-write"]
        .map(String::from)
        .into();
    if launch.resume.is_some() {
        args.extend(["resume".into(), "--last".into()]);
    }
    args.push("--json".into());
    for setting in mcp_settings(env) {
        args.extend(["-c".into(), setting]);
    }
    if let Some(model) = &launch.model {
        args.extend(["--model".into(), model.0.clone()]);
    }
    args.extend(["--skip-git-repo-check".into(), "-".into()]);
    CommandSpec {
        program: env.tools.codex.clone(),
        args,
        env: vec![
            ("ARGUS_RUN".to_owned(), env.run.0.to_string()),
            (
                "ARGUS_SOCKET".to_owned(),
                env.socket.to_string_lossy().into_owned(),
            ),
        ],
        workdir: Workdir::JobWorktree,
        cwd: None,
        stdin: Some(launch.prompt.clone().into_bytes()),
        stdout: SourceKind::CodexAgentExec,
        stderr: SourceKind::PlainText,
        terminal: false,
        watch: Vec::new(),
        artifacts: false,
        timeout: None,
    }
}

/// The argus MCP server (`adapters::MCP_SERVER`) as `-c` overrides of `[mcp_servers]`.
/// Codex gives a server only an allowlist of the agent's environment plus `env_vars`, so
/// those name the run's identity; and exec cannot ask for an approval, so the server's
/// tools are `approve`d (the host decides what each call may do). Keys and values checked
/// with codex-cli 0.171.0 `codex -c ... mcp get argus --json`, which rejects an unknown
/// approval mode. A real exec run (0.171.0, 2026-11-27) called the server's tool, and the
/// server saw the agent's `ARGUS_RUN` and `ARGUS_TOKEN`.
fn mcp_settings(env: &PlanEnv) -> Vec<String> {
    let key = format!("mcp_servers.{}", super::MCP_SERVER);
    let list = |items: &[&str]| -> String {
        let quoted: Vec<String> = items.iter().map(|i| toml_str(i)).collect();
        format!("[{}]", quoted.join(","))
    };
    vec![
        format!(
            "{key}.command={}",
            toml_str(&env.tools.argus.to_string_lossy())
        ),
        format!("{key}.args={}", list(super::MCP_ARGS)),
        format!("{key}.env_vars={}", list(super::MCP_ENV)),
        format!("{key}.default_tools_approval_mode={}", toml_str("approve")),
    ]
}

/// A TOML basic string: `"` and `\\` escaped, and every control character TOML forbids
/// in one (U+0000..U+001F and U+007F) as `\uXXXX`.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < '\u{20}' || c == '\u{7f}' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An agent run has no terminal to answer a prompt in.
pub(crate) fn permission_keys(_answer: PermissionAnswer) -> Option<Vec<u8>> {
    None
}

/// Record types seen in codex-cli 0.171.0 rollouts that carry nothing argus models. The
/// `response_item` records are the model's own messages and calls: the completed items
/// (`event_msg` / `item_completed`) say the same, so only those are read.
const ROLLOUT_BOOKKEEPING: &[&str] = &[
    "compacted",
    "inter_agent_communication_metadata",
    "response_item",
    "session_meta",
    "turn_context",
    "world_state",
];

/// `event_msg` types seen in those rollouts that carry nothing argus models.
const EVENT_BOOKKEEPING: &[&str] = &["task_started", "thread_settings_applied", "token_count"];

/// One rollout line, `{ timestamp, ordinal, type, payload }`, in the shapes of codex-cli
/// 0.171.0 (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`; the format is undocumented).
///
/// - `event_msg` / `item_completed`: the item by its `type`. `UserMessage` -> Prompt,
///   `AgentMessage` -> Reply, `Reasoning` -> Thinking (or `Other` when only encrypted),
///   `CommandExecution`, `FileChange` (its `unified_diff` gives the inserted lines) and
///   `McpToolCall` -> ToolCall + ToolResult.
/// - `token_usage_record` -> Usage keyed by `response_id`; `token_count` repeats running
///   totals and is not read.
/// - `event_msg` / `task_complete` or `turn_aborted` -> TurnEnded.
/// - Known bookkeeping -> `Other`; any other type -> `Unrecognized` (bump VERSION once
///   modeled). Rollouts of older versions, which lack `item_completed`, decode to `Other`
///   and stay readable in the raw view.
pub(crate) fn decode_rollout(bytes: &[u8]) -> Decoded {
    let v = match argus_json::parse(bytes) {
        Ok(v) => v,
        Err(e) => return unrecognized(format!("not JSON at byte {}: {}", e.at, e.what)),
    };
    let Some(kind) = str_at(&v, "type") else {
        return unrecognized("record without a type".into());
    };
    let at = str_at(&v, "timestamp").and_then(UnixMillis::parse_rfc3339);
    let payload = v.get("payload").cloned().unwrap_or(Json::Null);
    let (record, whats) = match kind {
        "event_msg" => {
            let event = str_at(&payload, "type").unwrap_or_default();
            match event {
                "item_completed" => {
                    let item = payload.get("item").cloned().unwrap_or(Json::Null);
                    let record = str_at(&item, "id").map(|id| RecordKey(id.to_owned()));
                    (record, rollout_item(&item))
                }
                "task_complete" | "turn_aborted" => (
                    None,
                    vec![AgentWhat::TurnEnded {
                        stop_reason: Some(event.to_owned()),
                    }],
                ),
                _ if EVENT_BOOKKEEPING.contains(&event) => {
                    (None, vec![other(&format!("event_msg:{event}"))])
                }
                _ => return unrecognized(format!("event_msg type `{event}`")),
            }
        }
        "token_usage_record" => (
            None,
            vec![AgentWhat::Usage {
                message: str_at(&payload, "response_id").map(str::to_owned),
                usage: usage_of(payload.get("usage").unwrap_or(&Json::Null)),
            }],
        ),
        _ if ROLLOUT_BOOKKEEPING.contains(&kind) => {
            let tag = match str_at(&payload, "type") {
                Some(sub) => format!("{kind}:{sub}"),
                None => kind.to_owned(),
            };
            (None, vec![other(&tag)])
        }
        _ => return unrecognized(format!("record type `{kind}`")),
    };
    events(record, at, whats)
}

/// The text blocks of a message's `content`, joined.
fn texts(content: Option<&Json>) -> String {
    items(content)
        .iter()
        .filter_map(|b| str_at(b, "text"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn rollout_item(item: &Json) -> Vec<AgentWhat> {
    let id = ToolCallId(str_at(item, "id").unwrap_or_default().to_owned());
    let status = str_at(item, "status");
    let kind = str_at(item, "type").unwrap_or_default();
    match kind {
        "UserMessage" => vec![AgentWhat::Prompt {
            text: texts(item.get("content")),
        }],
        "AgentMessage" => vec![AgentWhat::Reply {
            text: texts(item.get("content")),
            model: None,
        }],
        "Reasoning" => {
            let parts: Vec<&str> = items(item.get("summary_text"))
                .iter()
                .chain(items(item.get("raw_content")))
                .filter_map(Json::as_str)
                .collect();
            match parts.is_empty() {
                true => vec![other("item:Reasoning")],
                false => vec![AgentWhat::Thinking {
                    text: parts.join("\n"),
                }],
            }
        }
        "CommandExecution" => {
            let words: Vec<&str> = items(item.get("command"))
                .iter()
                .filter_map(Json::as_str)
                .collect();
            let exit_code = match item.get("exit_code") {
                Some(Json::Num(n)) => n.as_i64(),
                _ => None,
            };
            let input = Json::Obj(vec![
                (
                    "command".into(),
                    item.get("command").cloned().unwrap_or(Json::Null),
                ),
                ("cwd".into(), item.get("cwd").cloned().unwrap_or(Json::Null)),
            ]);
            vec![
                AgentWhat::ToolCall {
                    call: id.clone(),
                    name: ToolName(kind.to_owned()),
                    input_json: argus_json::write(&input),
                    touches: shell_touches(&words.join(" ")),
                },
                AgentWhat::ToolResult {
                    call: id,
                    is_error: status != Some("completed") || exit_code != Some(0),
                    output: str_at(item, "aggregated_output")
                        .unwrap_or_default()
                        .to_owned(),
                },
            ]
        }
        "FileChange" => {
            let changes = item
                .get("changes")
                .cloned()
                .unwrap_or(Json::Obj(Vec::new()));
            let touches = match &changes {
                Json::Obj(files) => files
                    .iter()
                    .flat_map(|(path, change)| {
                        let diff = str_at(change, "unified_diff").unwrap_or_default();
                        let added: Vec<&str> = diff
                            .lines()
                            .filter(|l| !l.starts_with("+++"))
                            .filter_map(|l| l.strip_prefix('+'))
                            .collect();
                        inserted(Some(path), &added)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let output = [str_at(item, "stdout"), str_at(item, "stderr")]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            vec![
                AgentWhat::ToolCall {
                    call: id.clone(),
                    name: ToolName(kind.to_owned()),
                    input_json: argus_json::write(&changes),
                    touches,
                },
                AgentWhat::ToolResult {
                    call: id,
                    is_error: status != Some("completed"),
                    output,
                },
            ]
        }
        "McpToolCall" => mcp_call(item, id, status),
        _ => vec![other(&format!("item:{kind}"))],
    }
}

/// An MCP tool call, as both formats write it: `server`, `tool`, `arguments`, and a
/// `result` or an `error { message }`.
fn mcp_call(item: &Json, id: ToolCallId, status: Option<&str>) -> Vec<AgentWhat> {
    let name = format!(
        "mcp:{}/{}",
        str_at(item, "server").unwrap_or_default(),
        str_at(item, "tool").unwrap_or_default()
    );
    let arguments = item
        .get("arguments")
        .cloned()
        .unwrap_or(Json::Obj(Vec::new()));
    let error = item.get("error").and_then(|e| str_at(e, "message"));
    let output = match (error, item.get("result")) {
        (Some(message), _) => message.to_owned(),
        (None, Some(result)) => argus_json::write(result),
        (None, None) => String::new(),
    };
    vec![
        AgentWhat::ToolCall {
            call: id.clone(),
            name: ToolName(name),
            input_json: argus_json::write(&arguments),
            touches: Vec::new(),
        },
        AgentWhat::ToolResult {
            call: id,
            is_error: status == Some("failed") || error.is_some(),
            output,
        },
    ]
}

/// One `codex exec --json` line.
///
/// - `item.completed`: the item, read by its `type` (below). `item.started` and
///   `item.updated` repeat an item still in progress -> `Other`, so nothing counts twice.
/// - `agent_message` -> `Result`: exec prints no final record, and the run's answer is its
///   last message, so each message is a `Result` and the last one stands (the review and
///   insights views keep the last).
/// - `command_execution` -> `ToolCall` + `ToolResult`; `file_change` -> one `ToolCall`
///   whose touches name the paths (exec gives no diff, so the content is unknown);
///   `mcp_tool_call` -> `ToolCall` + `ToolResult`; `todo_list` -> `Plan(Replace)`.
/// - `turn.completed` -> `Usage` (no message id: one per turn, added) + `TurnEnded`.
/// - `turn.failed` and `error` -> `Result { is_error }`, so a failed review says why.
pub(crate) fn decode_exec(bytes: &[u8]) -> Decoded {
    exec_line(bytes, Purpose::Answer)
}

/// One `codex exec --json` line of an agent run: as `decode_exec`, except that a message
/// is a `Reply` (the agent talking while it works), not the answer of a headless call.
pub(crate) fn decode_agent_exec(bytes: &[u8]) -> Decoded {
    exec_line(bytes, Purpose::Work)
}

/// What an exec run's messages are.
#[derive(Clone, Copy)]
enum Purpose {
    /// A headless call: the last message is its answer (`Result`).
    Answer,
    /// An agent run: messages are replies.
    Work,
}

fn exec_line(bytes: &[u8], purpose: Purpose) -> Decoded {
    let v = match argus_json::parse(bytes) {
        Ok(v) => v,
        Err(e) => return unrecognized(format!("not JSON at byte {}: {}", e.at, e.what)),
    };
    let Some(kind) = str_at(&v, "type") else {
        return unrecognized("event without a type".into());
    };
    let failed = |message: Option<&str>| AgentWhat::Result {
        text: message.unwrap_or_default().to_owned(),
        is_error: true,
    };
    let (record, whats) = match kind {
        "thread.started" | "turn.started" | "item.started" | "item.updated" => {
            (None, vec![other(kind)])
        }
        "item.completed" => {
            let Some(item) = v.get("item") else {
                return unrecognized("item.completed without an item".into());
            };
            let record = str_at(item, "id").map(|id| RecordKey(id.to_owned()));
            (record, exec_item(item, purpose))
        }
        "turn.completed" => {
            let mut whats = Vec::new();
            if let Some(usage) = v.get("usage") {
                whats.push(AgentWhat::Usage {
                    message: None,
                    usage: usage_of(usage),
                });
            }
            whats.push(AgentWhat::TurnEnded {
                stop_reason: Some("completed".into()),
            });
            (None, whats)
        }
        "turn.failed" => (
            None,
            vec![
                failed(v.get("error").and_then(|e| str_at(e, "message"))),
                AgentWhat::TurnEnded {
                    stop_reason: Some("failed".into()),
                },
            ],
        ),
        "error" => (None, vec![failed(str_at(&v, "message"))]),
        _ => return unrecognized(format!("event type `{kind}`")),
    };
    events(record, None, whats)
}

fn exec_item(item: &Json, purpose: Purpose) -> Vec<AgentWhat> {
    let id = ToolCallId(str_at(item, "id").unwrap_or_default().to_owned());
    let status = str_at(item, "status");
    match str_at(item, "type").unwrap_or_default() {
        "agent_message" => {
            let text = str_at(item, "text").unwrap_or_default().to_owned();
            vec![match purpose {
                Purpose::Answer => AgentWhat::Result {
                    text,
                    is_error: false,
                },
                Purpose::Work => AgentWhat::Reply { text, model: None },
            }]
        }
        "reasoning" => vec![AgentWhat::Thinking {
            text: str_at(item, "text").unwrap_or_default().to_owned(),
        }],
        "command_execution" => {
            let command = str_at(item, "command").unwrap_or_default();
            let exit_code = match item.get("exit_code") {
                Some(Json::Num(n)) => n.as_i64(),
                _ => None,
            };
            vec![
                AgentWhat::ToolCall {
                    call: id.clone(),
                    name: ToolName("command_execution".into()),
                    input_json: argus_json::write(&Json::Obj(vec![(
                        "command".into(),
                        Json::str(command),
                    )])),
                    touches: shell_touches(command),
                },
                AgentWhat::ToolResult {
                    call: id,
                    is_error: status != Some("completed") || exit_code != Some(0),
                    output: str_at(item, "aggregated_output")
                        .unwrap_or_default()
                        .to_owned(),
                },
            ]
        }
        "file_change" => {
            let changes = item
                .get("changes")
                .cloned()
                .unwrap_or(Json::Arr(Vec::new()));
            let touches = items(Some(&changes))
                .iter()
                .filter_map(|c| str_at(c, "path"))
                .map(|path| FileTouch {
                    path: path.to_owned(),
                    how: TouchHow::Shell,
                })
                .collect();
            vec![
                AgentWhat::ToolCall {
                    call: id.clone(),
                    name: ToolName("file_change".into()),
                    input_json: argus_json::write(&changes),
                    touches,
                },
                AgentWhat::ToolResult {
                    call: id,
                    is_error: status != Some("completed"),
                    output: String::new(),
                },
            ]
        }
        "mcp_tool_call" => mcp_call(item, id, status),
        "todo_list" => vec![AgentWhat::Plan(PlanOp::Replace(
            items(item.get("items"))
                .iter()
                .map(|t| PlanItem {
                    content: str_at(t, "text").unwrap_or_default().to_owned(),
                    status: match t.get("completed") {
                        Some(Json::Bool(true)) => PlanStatus::Completed,
                        _ => PlanStatus::Pending,
                    },
                })
                .collect(),
        ))],
        other_kind => vec![other(&format!("item:{other_kind}"))],
    }
}

fn shell_touches(command: &str) -> Vec<FileTouch> {
    shell_paths(command)
        .into_iter()
        .map(|path| FileTouch {
            path,
            how: TouchHow::Shell,
        })
        .collect()
}

/// OpenAI usage counts cached input inside `input_tokens`; argus counts it apart, as
/// Claude reports it, so `input` is the uncached part.
fn usage_of(usage: &Json) -> TokenUsage {
    let cached = u64_at(usage, "cached_input_tokens");
    TokenUsage {
        input: u64_at(usage, "input_tokens").saturating_sub(cached),
        output: u64_at(usage, "output_tokens"),
        cache_read: cached,
        cache_write: u64_at(usage, "cache_write_input_tokens"),
    }
}

fn events(record: Option<RecordKey>, at: Option<UnixMillis>, whats: Vec<AgentWhat>) -> Decoded {
    let mut obs = whats.into_iter().map(|what| {
        Observation::Agent(AgentEvent {
            record: record.clone(),
            parent: None,
            at,
            sidechain: false,
            what,
        })
    });
    match obs.next() {
        Some(first) => Decoded::many(first, obs.collect()),
        None => unrecognized("a record with nothing in it".into()),
    }
}

fn other(tag: &str) -> AgentWhat {
    AgentWhat::Other {
        tag: tag.to_owned(),
    }
}

fn unrecognized(reason: String) -> Decoded {
    Decoded::one(Observation::Unrecognized { reason })
}

fn str_at<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn items(v: Option<&Json>) -> &[Json] {
    match v {
        Some(Json::Arr(items)) => items,
        _ => &[],
    }
}

fn u64_at(v: &Json, key: &str) -> u64 {
    match v.get(key) {
        Some(Json::Num(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_toml_string_escapes_what_toml_forbids_raw() {
        assert_eq!(toml_str("a b"), r#""a b""#);
        assert_eq!(toml_str("q\"b\\"), r#""q\"b\\""#);
        assert_eq!(toml_str("n\nd\u{7f}é"), r#""n\u000Ad\u007Fé""#);
    }

    use super::*;

    fn whats(decoded: &Decoded) -> Vec<&AgentWhat> {
        decoded
            .iter()
            .map(|o| match o {
                Observation::Agent(ev) => &ev.what,
                other => panic!("not an agent event: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn an_exec_message_is_the_runs_answer_and_a_failure_says_why() {
        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Looks fine."}}"#,
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Result {
                text: "Looks fine.".into(),
                is_error: false
            }]
        );
        let Observation::Agent(ev) = d.iter().next().unwrap() else {
            unreachable!()
        };
        assert_eq!(ev.record, Some(RecordKey("item_3".into())));

        let d = decode_exec(br#"{"type":"turn.failed","error":{"message":"rate limited"}}"#);
        assert_eq!(
            whats(&d)[0],
            &AgentWhat::Result {
                text: "rate limited".into(),
                is_error: true
            }
        );
        let d = decode_exec(br#"{"type":"error","message":"stream closed"}"#);
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Result {
                text: "stream closed".into(),
                is_error: true
            }]
        );
    }

    #[test]
    fn an_exec_command_is_a_call_and_its_result() {
        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc 'sed -i s/a/b/ src/lib.rs'","aggregated_output":"","exit_code":0,"status":"completed"}}"#,
        );
        let w = whats(&d);
        let AgentWhat::ToolCall { call, touches, .. } = w[0] else {
            panic!("{w:?}")
        };
        assert_eq!(call, &ToolCallId("item_1".into()));
        assert_eq!(
            touches,
            &vec![FileTouch {
                path: "src/lib.rs".into(),
                how: TouchHow::Shell
            }]
        );
        assert_eq!(
            w[1],
            &AgentWhat::ToolResult {
                call: ToolCallId("item_1".into()),
                is_error: false,
                output: String::new()
            }
        );
        // A non-zero exit, or a command the sandbox declined, is an error.
        for item in [
            r#"{"id":"i","type":"command_execution","command":"false","aggregated_output":"","exit_code":1,"status":"failed"}"#,
            r#"{"id":"i","type":"command_execution","command":"rm x","aggregated_output":"","exit_code":null,"status":"declined"}"#,
        ] {
            let line = format!(r#"{{"type":"item.completed","item":{item}}}"#);
            let d = decode_exec(line.as_bytes());
            assert!(
                matches!(whats(&d)[1], AgentWhat::ToolResult { is_error: true, .. }),
                "{item}"
            );
        }
    }

    #[test]
    fn exec_file_changes_plans_and_mcp_calls_are_read() {
        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"f","type":"file_change","changes":[{"path":"src/a.rs","kind":"update"},{"path":"b.md","kind":"add"}],"status":"completed"}}"#,
        );
        let AgentWhat::ToolCall { touches, .. } = whats(&d)[0] else {
            panic!()
        };
        let paths: Vec<&str> = touches.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "b.md"]);
        assert_eq!(
            whats(&d)[1],
            &AgentWhat::ToolResult {
                call: ToolCallId("f".into()),
                is_error: false,
                output: String::new()
            }
        );
        // A patch the sandbox refused (seen from codex-cli 0.171.0) is an error.
        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"f","type":"file_change","changes":[{"path":"/w/notes.txt","kind":"add"}],"status":"failed"}}"#,
        );
        assert!(matches!(
            whats(&d)[1],
            AgentWhat::ToolResult { is_error: true, .. }
        ));

        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"t","type":"todo_list","items":[{"text":"parse","completed":true},{"text":"test","completed":false}]}}"#,
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Plan(PlanOp::Replace(vec![
                PlanItem {
                    content: "parse".into(),
                    status: PlanStatus::Completed
                },
                PlanItem {
                    content: "test".into(),
                    status: PlanStatus::Pending
                },
            ]))]
        );

        let d = decode_exec(
            br#"{"type":"item.completed","item":{"id":"m","type":"mcp_tool_call","server":"argus","tool":"set_task","arguments":{"job":"j"},"result":null,"error":{"message":"Forbidden"},"status":"failed"}}"#,
        );
        let w = whats(&d);
        assert!(matches!(w[0], AgentWhat::ToolCall { name, .. } if name.0 == "mcp:argus/set_task"));
        assert_eq!(
            w[1],
            &AgentWhat::ToolResult {
                call: ToolCallId("m".into()),
                is_error: true,
                output: "Forbidden".into()
            }
        );
    }

    #[test]
    fn exec_usage_counts_cached_input_apart_once_per_turn() {
        let d = decode_exec(
            br#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"reasoning_output_tokens":20}}"#,
        );
        assert_eq!(
            whats(&d),
            vec![
                &AgentWhat::Usage {
                    message: None,
                    usage: TokenUsage {
                        input: 200,
                        output: 50,
                        cache_read: 800,
                        cache_write: 0
                    }
                },
                &AgentWhat::TurnEnded {
                    stop_reason: Some("completed".into())
                },
            ]
        );
    }

    #[test]
    fn progress_events_count_nothing_twice_and_unknown_ones_are_shown_raw() {
        for line in [
            r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"i","type":"command_execution","command":"ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
        ] {
            assert!(
                matches!(
                    whats(&decode_exec(line.as_bytes()))[..],
                    [AgentWhat::Other { .. }]
                ),
                "{line}"
            );
        }
        for line in [r#"{"type":"turn.paused"}"#, "not json", r#"{"no":"type"}"#] {
            let d = decode_exec(line.as_bytes());
            assert!(
                matches!(d.iter().next(), Some(Observation::Unrecognized { .. })),
                "{line}"
            );
        }
    }

    /// A rollout line as codex-cli 0.171.0 writes one (shape only; the values are made up).
    fn line(kind: &str, payload: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-11-25T14:16:49.120Z","ordinal":7,"type":"{kind}","payload":{payload}}}"#
        )
    }

    fn item(item: &str) -> String {
        line(
            "event_msg",
            &format!(
                r#"{{"type":"item_completed","thread_id":"th","turn_id":"tu","item":{item},"started_at_ms":1,"completed_at_ms":2}}"#
            ),
        )
    }

    #[test]
    fn rollout_messages_are_prompts_and_replies_with_their_time() {
        let d = decode_rollout(
            item(r#"{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"fix the gate","text_elements":[]}]}"#)
                .as_bytes(),
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Prompt {
                text: "fix the gate".into()
            }]
        );
        let Observation::Agent(ev) = d.iter().next().unwrap() else {
            unreachable!()
        };
        assert_eq!(ev.record, Some(RecordKey("u1".into())));
        assert_eq!(
            ev.at,
            crate::ids::UnixMillis::parse_rfc3339("2026-11-25T14:16:49.120Z")
        );
        let d = decode_rollout(
            item(r#"{"type":"AgentMessage","id":"a1","content":[{"type":"Text","text":"Done."}],"phase":"final_answer"}"#)
                .as_bytes(),
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Reply {
                text: "Done.".into(),
                model: None
            }]
        );
        // Reasoning the rollout keeps only encrypted shows as such, not as empty thinking.
        let d = decode_rollout(
            item(r#"{"type":"Reasoning","id":"r1","summary_text":[],"raw_content":[]}"#).as_bytes(),
        );
        assert!(matches!(whats(&d)[..], [AgentWhat::Other { .. }]));
        let d = decode_rollout(
            item(r#"{"type":"Reasoning","id":"r2","summary_text":["Check the lock first."],"raw_content":[]}"#)
                .as_bytes(),
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Thinking {
                text: "Check the lock first.".into()
            }]
        );
    }

    #[test]
    fn rollout_commands_and_patches_are_calls_with_what_they_touch() {
        let d = decode_rollout(
            item(r#"{"type":"CommandExecution","id":"c1","process_id":"1","command":["sed","-i","s/a/b/","src/lib.rs"],"cwd":"/w","parsed_cmd":[],"source":"agent","status":"completed","aggregated_output":"","exit_code":2,"duration":{"secs":0,"nanos":1}}"#)
                .as_bytes(),
        );
        let w = whats(&d);
        let AgentWhat::ToolCall { name, touches, .. } = w[0] else {
            panic!("{w:?}")
        };
        assert_eq!(name, &ToolName("CommandExecution".into()));
        assert_eq!(touches[0].path, "src/lib.rs");
        assert!(matches!(w[1], AgentWhat::ToolResult { is_error: true, .. }));

        // Escaped as the rollout's JSON escapes it.
        let diff = r"@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn b() -> u8 { 1 }\n+fn c() {}\n";
        let d = decode_rollout(
            item(&format!(
                r#"{{"type":"FileChange","id":"f1","changes":{{"/w/src/a.rs":{{"type":"update","unified_diff":"{diff}","move_path":null}}}},"status":"completed","stdout":"Success.","stderr":""}}"#
            ))
            .as_bytes(),
        );
        let w = whats(&d);
        let AgentWhat::ToolCall { touches, .. } = w[0] else {
            panic!("{w:?}")
        };
        assert_eq!(
            touches,
            &vec![FileTouch {
                path: "/w/src/a.rs".into(),
                how: TouchHow::Inserted(vec![
                    crate::diff::line_hash("fn b() -> u8 { 1 }"),
                    crate::diff::line_hash("fn c() {}"),
                ]),
            }]
        );
        assert_eq!(
            w[1],
            &AgentWhat::ToolResult {
                call: ToolCallId("f1".into()),
                is_error: false,
                output: "Success.".into()
            }
        );
    }

    #[test]
    fn rollout_usage_is_per_response_and_a_finished_task_ends_the_turn() {
        let d = decode_rollout(
            line(
                "token_usage_record",
                r#"{"thread_id":"th","turn_id":"tu","session_id":"s","root_turn_id":"tu","response_id":"resp_1","usage":{"input_tokens":900,"cached_input_tokens":600,"cache_write_input_tokens":0,"output_tokens":40,"reasoning_output_tokens":10,"total_tokens":940}}"#,
            )
            .as_bytes(),
        );
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::Usage {
                message: Some("resp_1".into()),
                usage: TokenUsage {
                    input: 300,
                    output: 40,
                    cache_read: 600,
                    cache_write: 0
                }
            }]
        );
        let d = decode_rollout(
            line("event_msg", r#"{"type":"task_complete","turn_id":"tu","last_agent_message":"Done.","started_at":1,"completed_at":2,"duration_ms":1}"#)
                .as_bytes(),
        );
        assert!(matches!(whats(&d)[..], [AgentWhat::TurnEnded { .. }]));
    }

    #[test]
    fn rollout_records_said_twice_count_once_and_unknown_ones_are_shown_raw() {
        // The model's own records repeat what the completed items say.
        for known in [
            line(
                "response_item",
                r#"{"type":"message","role":"user","content":[]}"#,
            ),
            line(
                "response_item",
                r#"{"type":"function_call","name":"wait","arguments":"{}","call_id":"c"}"#,
            ),
            line("event_msg", r#"{"type":"token_count","info":null}"#),
            line("session_meta", r#"{"id":"s","cwd":"/w"}"#),
            line("turn_context", r#"{"model":"m","effort":"high"}"#),
            item(r#"{"type":"ImageView","id":"i","path":"/w/a.png"}"#),
        ] {
            assert!(
                matches!(
                    whats(&decode_rollout(known.as_bytes()))[..],
                    [AgentWhat::Other { .. }]
                ),
                "{known}"
            );
        }
        for unknown in [
            line("hologram", "{}"),
            line("event_msg", r#"{"type":"turn_teleported"}"#),
            "not json".to_owned(),
        ] {
            let d = decode_rollout(unknown.as_bytes());
            assert!(
                matches!(d.iter().next(), Some(Observation::Unrecognized { .. })),
                "{unknown}"
            );
        }
    }

    #[test]
    fn an_agent_runs_messages_are_replies_not_answers() {
        let line = br#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Fixed the gate."}}"#;
        assert_eq!(
            whats(&decode_agent_exec(line)),
            vec![&AgentWhat::Reply {
                text: "Fixed the gate.".into(),
                model: None
            }]
        );
        // Everything else reads as in a headless run.
        let line = br#"{"type":"turn.completed","usage":{"input_tokens":3,"cached_input_tokens":0,"output_tokens":1}}"#;
        assert_eq!(whats(&decode_agent_exec(line)), whats(&decode_exec(line)));
    }
}
