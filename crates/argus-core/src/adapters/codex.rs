//! Codex CLI: headless `codex exec --json` lines, and the session rollout files.
//! This module is the only code that knows either format.
//!
//! `exec --json` events follow `codex-rs/exec/src/exec_events.rs` (main branch):
//! <https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs>. The help
//! of codex-cli 0.171.0 confirms the flags used here. Unverified against a real run:
//! that an item's `type` is snake_case on the wire, and how usage splits cached input.

use argus_json::Json;

use super::touch::shell_paths;
use crate::command::{CommandSpec, PermissionAnswer, PlanEnv};
use crate::event::AgentLaunch;
use crate::ids::{RecordKey, ToolCallId, ToolName};
use crate::source::{
    AgentEvent, AgentWhat, Decoded, FileTouch, Observation, PlanItem, PlanOp, PlanStatus,
    TokenUsage, TouchHow,
};

pub(crate) const VERSION: u32 = 1;

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
            (record, exec_item(item))
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
    events(record, whats)
}

fn exec_item(item: &Json) -> Vec<AgentWhat> {
    let id = ToolCallId(str_at(item, "id").unwrap_or_default().to_owned());
    let status = str_at(item, "status");
    match str_at(item, "type").unwrap_or_default() {
        "agent_message" => vec![AgentWhat::Result {
            text: str_at(item, "text").unwrap_or_default().to_owned(),
            is_error: false,
        }],
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
            vec![AgentWhat::ToolCall {
                call: id,
                name: ToolName("file_change".into()),
                input_json: argus_json::write(&changes),
                touches,
            }]
        }
        "mcp_tool_call" => {
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

fn events(record: Option<RecordKey>, whats: Vec<AgentWhat>) -> Decoded {
    let mut obs = whats.into_iter().map(|what| {
        Observation::Agent(AgentEvent {
            record: record.clone(),
            parent: None,
            at: None,
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
}
