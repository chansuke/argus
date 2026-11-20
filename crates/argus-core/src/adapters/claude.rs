//! Claude Code: interactive launch, transcript JSONL, headless stream-json, hook payloads.
//! The transcript format is undocumented and changes between versions; this module is
//! the only code that knows it.

use argus_json::Json;

use crate::diff::line_hash;
use crate::event::AgentLaunch;
use crate::ids::{ModelId, RecordKey, ToolCallId, ToolName, UnixMillis};
use crate::plan::{CommandSpec, PermissionAnswer, PlanEnv, WatchSpec, Workdir};
use crate::source::{
    AgentEvent, AgentWhat, Decoded, FileTouch, Observation, PlanItem, PlanOp, PlanStatus,
    SourceKind, TokenUsage, TouchHow,
};

pub(crate) const VERSION: u32 = 1;

/// `claude --session-id <run-uuid> [--model m] <prompt>` under a PTY in the job worktree,
/// or, to deliver feedback (`launch.resume`), `claude --resume <earlier-run-uuid>
/// --fork-session --session-id <run-uuid> ...`: a copy of the earlier session continues
/// under this run's id, so each run has its own transcript file.
///
/// The run id is the session id, so the transcript path is known before the process
/// starts: watch `.claude/projects/*/<uuid>.jsonl` and
/// `.claude/projects/*/<uuid>/subagents/*.jsonl` (glob on the slug instead of re-deriving
/// Claude's cwd-to-slug rule).
/// Env: `ARGUS_RUN`, `ARGUS_SOCKET`, so `argus task ...` and `argus hook` inside the agent
/// know who they speak for.
///
/// Flags from Claude Code 2.1.303 `--help`. Unverified: that `--resume` with
/// `--fork-session` accepts `--session-id`, and that a v8 UUID (derived run ids) is
/// accepted as a session id. No MCP config is passed yet: the argus MCP server does not
/// exist.
pub(crate) fn launch(launch: &AgentLaunch, env: &PlanEnv) -> CommandSpec {
    let run = env.run.0.to_string();
    let mut args = Vec::new();
    if let Some(prev) = launch.resume {
        args.extend([
            "--resume".to_owned(),
            prev.0.to_string(),
            "--fork-session".to_owned(),
        ]);
    }
    args.extend(["--session-id".to_owned(), run.clone()]);
    if let Some(model) = &launch.model {
        args.extend(["--model".to_owned(), model.0.clone()]);
    }
    args.push(launch.prompt.clone());
    let watch = |pattern: String| WatchSpec {
        pattern,
        kind: SourceKind::ClaudeTranscript,
    };
    CommandSpec {
        program: env.tools.claude.clone(),
        args,
        env: vec![
            ("ARGUS_RUN".to_owned(), run.clone()),
            (
                "ARGUS_SOCKET".to_owned(),
                env.socket.to_string_lossy().into_owned(),
            ),
        ],
        workdir: Workdir::JobWorktree,
        cwd: None,
        stdin: None,
        stdout: SourceKind::PlainText,
        stderr: SourceKind::PlainText,
        terminal: true,
        watch: vec![
            watch(format!(".claude/projects/*/{run}.jsonl")),
            watch(format!(".claude/projects/*/{run}/subagents/*.jsonl")),
        ],
        artifacts: false,
        timeout: None,
    }
}

/// Unverified: the prompt is a numbered menu (1 yes, 2 yes and don't ask again, 3 no)
/// in current versions; confirm per version before relying on it.
pub(crate) fn permission_keys(answer: PermissionAnswer) -> Vec<u8> {
    todo!("not implemented")
}

/// One transcript line.
///
/// - Common fields: `uuid` -> record, `parentUuid` -> parent, `timestamp` -> at,
///   `isSidechain` -> sidechain.
/// - `assistant`: one observation per `message.content[]` block (`thinking`, `text`,
///   `tool_use`, plus a `Plan` op after `TodoWrite`/`TaskUpdate` calls), then `Usage`
///   keyed by `message.id`, then `TurnEnded` for any stop reason but `tool_use`.
/// - `user`: string content -> Prompt; blocks -> Prompt / ToolResult / Other; a
///   `toolUseResult.task` (TaskCreate) -> Plan(Upsert{Pending}).
/// - Record types argus knows and does not model -> `Other { tag }`; any other type ->
///   `Unrecognized`, which the raw view shows (bump VERSION once modeled).
pub(crate) fn decode_transcript(bytes: &[u8]) -> Decoded {
    decode_line(bytes, |_| None)
}

/// `claude -p --output-format stream-json --verbose` line. Same record shapes as the
/// transcript plus `system`/`init` and the final `result` (-> `Result { text, is_error }`).
pub(crate) fn decode_stream_json(bytes: &[u8]) -> Decoded {
    decode_line(bytes, |v| match v.get("type")?.as_str()? {
        "result" => Some(vec![AgentWhat::Result {
            text: str_at(v, "result").unwrap_or_default().to_owned(),
            is_error: v.get("is_error") == Some(&Json::Bool(true)),
        }]),
        _ => None,
    })
}

/// Hook payload (`Notification`, `Stop`, `PreToolUse`, ...) forwarded by `argus hook`.
///
/// Unverified: `Notification` also fires for "waiting for your input", so only a message
/// that mentions permission is read as `PermissionWait`. Confirm per version.
pub(crate) fn decode_hook(bytes: &[u8]) -> Decoded {
    let v = match argus_json::parse(bytes) {
        Ok(v) => v,
        Err(e) => return unrecognized(format!("hook payload is not JSON: {}", e.what)),
    };
    let Some(name) = str_at(&v, "hook_event_name") else {
        return unrecognized("hook payload without hook_event_name".into());
    };
    let message = str_at(&v, "message").unwrap_or_default();
    let what = if name == "Notification" && message.contains("permission") {
        AgentWhat::PermissionWait {
            message: message.to_owned(),
        }
    } else {
        AgentWhat::Other {
            tag: format!("hook:{name}"),
        }
    };
    Decoded::one(Observation::Agent(AgentEvent {
        record: None,
        parent: None,
        at: None,
        sidechain: false,
        what,
    }))
}

/// Record types seen in real transcripts that carry nothing argus models yet.
const BOOKKEEPING: &[&str] = &[
    "agent-name",
    "ai-title",
    "api-request",
    "api-request-blob",
    "api-request-shape",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "atis-latch",
    "attachment",
    "bridge-session",
    "continued-in",
    "cost-state",
    "custom-title",
    "file-history-delta",
    "file-history-snapshot",
    "frame-link",
    "last-prompt",
    "mode",
    "permission-mode",
    "pr-link",
    "queue-operation",
    "relocated",
    "summary",
    "system",
    "worktree-state",
];

fn decode_line(bytes: &[u8], extra: impl Fn(&Json) -> Option<Vec<AgentWhat>>) -> Decoded {
    let v = match argus_json::parse(bytes) {
        Ok(v) => v,
        Err(e) => return unrecognized(format!("not JSON at byte {}: {}", e.at, e.what)),
    };
    let Some(kind) = str_at(&v, "type") else {
        return unrecognized("record without a type".into());
    };
    let whats = match kind {
        "assistant" => assistant(&v),
        "user" => user(&v),
        _ => match extra(&v) {
            Some(whats) => whats,
            None if BOOKKEEPING.contains(&kind) => vec![AgentWhat::Other {
                tag: kind.to_owned(),
            }],
            None => return unrecognized(format!("record type `{kind}`")),
        },
    };
    let record = str_at(&v, "uuid").map(|s| RecordKey(s.to_owned()));
    let parent = str_at(&v, "parentUuid").map(|s| RecordKey(s.to_owned()));
    let at = str_at(&v, "timestamp").and_then(UnixMillis::parse_rfc3339);
    let sidechain = v.get("isSidechain") == Some(&Json::Bool(true));
    let mut obs = whats.into_iter().map(|what| {
        Observation::Agent(AgentEvent {
            record: record.clone(),
            parent: parent.clone(),
            at,
            sidechain,
            what,
        })
    });
    match obs.next() {
        Some(first) => Decoded::many(first, obs.collect()),
        None => unrecognized(format!("`{kind}` record with nothing in it")),
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

fn assistant(v: &Json) -> Vec<AgentWhat> {
    let Some(message) = v.get("message") else {
        return Vec::new();
    };
    let model = str_at(message, "model").map(|m| ModelId(m.to_owned()));
    let mut out = Vec::new();
    for block in items(message.get("content")) {
        match str_at(block, "type") {
            Some("thinking") => out.push(AgentWhat::Thinking {
                text: str_at(block, "thinking").unwrap_or_default().to_owned(),
            }),
            Some("text") => out.push(AgentWhat::Reply {
                text: str_at(block, "text").unwrap_or_default().to_owned(),
                model: model.clone(),
            }),
            Some("tool_use") => {
                let name = str_at(block, "name").unwrap_or_default();
                let input = block.get("input").cloned().unwrap_or(Json::Obj(Vec::new()));
                out.push(AgentWhat::ToolCall {
                    call: ToolCallId(str_at(block, "id").unwrap_or_default().to_owned()),
                    name: ToolName(name.to_owned()),
                    input_json: argus_json::write(&input),
                    touches: touches(name, &input),
                });
                out.extend(plan_op(name, &input).map(AgentWhat::Plan));
            }
            other => out.push(AgentWhat::Other {
                tag: other.unwrap_or("block").to_owned(),
            }),
        }
    }
    if let Some(usage) = message.get("usage") {
        out.push(AgentWhat::Usage {
            message: str_at(message, "id").map(str::to_owned),
            usage: TokenUsage {
                input: u64_at(usage, "input_tokens"),
                output: u64_at(usage, "output_tokens"),
                cache_read: u64_at(usage, "cache_read_input_tokens"),
                cache_write: u64_at(usage, "cache_creation_input_tokens"),
            },
        });
    }
    if let Some(reason) = message.get("stop_reason").and_then(Json::as_str)
        && reason != "tool_use"
    {
        out.push(AgentWhat::TurnEnded {
            stop_reason: Some(reason.to_owned()),
        });
    }
    out
}

fn user(v: &Json) -> Vec<AgentWhat> {
    let content = v.get("message").and_then(|m| m.get("content"));
    let mut out = Vec::new();
    match content {
        Some(Json::Str(text)) => out.push(AgentWhat::Prompt { text: text.clone() }),
        _ => {
            for block in items(content) {
                match str_at(block, "type") {
                    Some("text") => out.push(AgentWhat::Prompt {
                        text: str_at(block, "text").unwrap_or_default().to_owned(),
                    }),
                    Some("tool_result") => out.push(AgentWhat::ToolResult {
                        call: ToolCallId(
                            str_at(block, "tool_use_id").unwrap_or_default().to_owned(),
                        ),
                        is_error: block.get("is_error") == Some(&Json::Bool(true)),
                        output: result_text(block.get("content")),
                    }),
                    other => out.push(AgentWhat::Other {
                        tag: other.unwrap_or("block").to_owned(),
                    }),
                }
            }
        }
    }
    if let Some(task) = v.get("toolUseResult").and_then(|r| r.get("task"))
        && let Some(id) = str_at(task, "id")
    {
        out.push(AgentWhat::Plan(PlanOp::Upsert {
            task: id.to_owned(),
            subject: str_at(task, "subject").map(str::to_owned),
            status: Some(PlanStatus::Pending),
        }));
    }
    out
}

/// A tool result's content: a string, or blocks whose text is kept and whose images
/// (and anything else) are named in brackets.
fn result_text(content: Option<&Json>) -> String {
    match content {
        Some(Json::Str(s)) => s.clone(),
        Some(Json::Arr(blocks)) => blocks
            .iter()
            .map(|b| match str_at(b, "type") {
                Some("text") => str_at(b, "text").unwrap_or_default().to_owned(),
                other => format!("[{}]", other.unwrap_or("block")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn inserted(path: Option<&str>, texts: &[&str]) -> Vec<FileTouch> {
    let Some(path) = path else { return Vec::new() };
    let hashes = texts
        .iter()
        .flat_map(|t| t.lines())
        .map(line_hash)
        .collect();
    vec![FileTouch {
        path: path.to_owned(),
        how: TouchHow::Inserted(hashes),
    }]
}

fn touches(tool: &str, input: &Json) -> Vec<FileTouch> {
    let path = str_at(input, "file_path");
    match tool {
        "Write" => inserted(path, &[str_at(input, "content").unwrap_or_default()]),
        "Edit" => inserted(path, &[str_at(input, "new_string").unwrap_or_default()]),
        "MultiEdit" => {
            let texts: Vec<&str> = items(input.get("edits"))
                .iter()
                .filter_map(|e| str_at(e, "new_string"))
                .collect();
            inserted(path, &texts)
        }
        "Bash" => shell_paths(str_at(input, "command").unwrap_or_default())
            .into_iter()
            .map(|path| FileTouch {
                path,
                how: TouchHow::Shell,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Words of a shell command that look like file paths. Coarse on purpose: a hunk this
/// names is "via shell" rather than unexplained; it never claims what was written.
fn shell_paths(command: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in command.split(|c: char| c.is_whitespace() || ";|&<>()`".contains(c)) {
        let word = word.trim_matches(|c| c == '\'' || c == '"');
        let path_chars = word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/~+-@".contains(c));
        let has_ext = word
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && !stem.ends_with('/'));
        let looks_like_path = !word.is_empty()
            && path_chars
            && !word.starts_with('-')
            && !word.ends_with('/')
            && (word.contains('/') || has_ext);
        if looks_like_path && !out.iter().any(|p| p == word) {
            out.push(word.to_owned());
        }
    }
    out
}

fn plan_status(s: Option<&str>) -> Option<PlanStatus> {
    Some(match s? {
        "pending" => PlanStatus::Pending,
        "in_progress" => PlanStatus::InProgress,
        "completed" => PlanStatus::Completed,
        "deleted" => PlanStatus::Deleted,
        _ => return None,
    })
}

fn plan_op(tool: &str, input: &Json) -> Option<PlanOp> {
    match tool {
        "TodoWrite" => Some(PlanOp::Replace(
            items(input.get("todos"))
                .iter()
                .map(|t| PlanItem {
                    content: str_at(t, "content").unwrap_or_default().to_owned(),
                    status: plan_status(str_at(t, "status")).unwrap_or(PlanStatus::Pending),
                })
                .collect(),
        )),
        "TaskUpdate" => Some(PlanOp::Upsert {
            task: str_at(input, "taskId")?.to_owned(),
            subject: str_at(input, "subject").map(str::to_owned),
            status: plan_status(str_at(input, "status")),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(decoded: &Decoded) -> Vec<&AgentEvent> {
        decoded
            .iter()
            .map(|o| match o {
                Observation::Agent(ev) => ev,
                other => panic!("not an agent event: {other:?}"),
            })
            .collect()
    }

    fn whats(decoded: &Decoded) -> Vec<&AgentWhat> {
        agent(decoded).into_iter().map(|ev| &ev.what).collect()
    }

    const PROMPT: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"fix the gate"},"uuid":"u-1","timestamp":"2026-10-24T03:16:30.255Z","sessionId":"s","version":"2.9.0","cwd":"/w"}"#;

    #[test]
    fn a_prompt_carries_the_record_identity() {
        let d = decode_transcript(PROMPT.as_bytes());
        let evs = agent(&d);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].record, Some(RecordKey("u-1".into())));
        assert_eq!(evs[0].parent, None);
        assert_eq!(
            evs[0].at,
            UnixMillis::parse_rfc3339("2026-10-24T03:16:30.255Z")
        );
        assert!(!evs[0].sidechain);
        assert_eq!(
            evs[0].what,
            AgentWhat::Prompt {
                text: "fix the gate".into()
            }
        );
    }

    #[test]
    fn a_trailing_newline_is_part_of_the_frame_not_the_record() {
        let line = format!("{PROMPT}\n");
        assert_eq!(
            decode_transcript(line.as_bytes()),
            decode_transcript(PROMPT.as_bytes())
        );
    }

    #[test]
    fn assistant_blocks_become_one_observation_each_plus_usage() {
        let line = r#"{"parentUuid":"u-1","isSidechain":true,"type":"assistant","uuid":"a-1","timestamp":"2026-10-24T03:16:31.000Z","message":{"id":"msg_1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"thinking","thinking":"look at tools/gate","signature":"sig"},{"type":"text","text":"The step masks failures."}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40,"service_tier":"standard"}}}"#;
        let d = decode_transcript(line.as_bytes());
        let evs = agent(&d);
        assert!(evs.iter().all(|e| e.record == Some(RecordKey("a-1".into()))
            && e.parent == Some(RecordKey("u-1".into()))
            && e.sidechain));
        assert_eq!(
            whats(&d),
            vec![
                &AgentWhat::Thinking {
                    text: "look at tools/gate".into()
                },
                &AgentWhat::Reply {
                    text: "The step masks failures.".into(),
                    model: Some(ModelId("claude-opus-5-5".into())),
                },
                &AgentWhat::Usage {
                    message: Some("msg_1".into()),
                    usage: TokenUsage {
                        input: 10,
                        output: 20,
                        cache_read: 30,
                        cache_write: 40
                    },
                },
                &AgentWhat::TurnEnded {
                    stop_reason: Some("end_turn".into())
                },
            ]
        );
    }

    /// Claude Code stores most thinking blocks with only a signature. The block is still
    /// an observation, so the raw view can show that the model thought and nothing was kept.
    #[test]
    fn a_thinking_block_without_text_is_still_seen() {
        let line = r#"{"type":"assistant","uuid":"a-2","message":{"id":"msg_2","content":[{"type":"thinking","thinking":"","signature":"sig"}],"stop_reason":null}}"#;
        assert_eq!(
            whats(&decode_transcript(line.as_bytes())),
            vec![&AgentWhat::Thinking {
                text: String::new()
            }]
        );
    }

    #[test]
    fn a_tool_call_keeps_its_input_verbatim_and_names_what_it_edits() {
        let line = r#"{"type":"assistant","uuid":"a-3","message":{"id":"msg_3","content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/w/tools/gate","old_string":"if \"step_$s\"","new_string":"set +e\n  ( set -e; \"step_$s\" )\n","replace_all":false}}],"stop_reason":"tool_use"}}"#;
        let d = decode_transcript(line.as_bytes());
        assert_eq!(
            whats(&d),
            vec![&AgentWhat::ToolCall {
                call: ToolCallId("toolu_1".into()),
                name: ToolName("Edit".into()),
                input_json: r#"{"file_path":"/w/tools/gate","old_string":"if \"step_$s\"","new_string":"set +e\n  ( set -e; \"step_$s\" )\n","replace_all":false}"#.into(),
                touches: vec![FileTouch {
                    path: "/w/tools/gate".into(),
                    how: TouchHow::Inserted(vec![line_hash("set +e"), line_hash("  ( set -e; \"step_$s\" )")]),
                }],
            }]
        );
    }

    fn touches(name: &str, input: &str) -> Vec<FileTouch> {
        let line = format!(
            r#"{{"type":"assistant","uuid":"a","message":{{"id":"m","content":[{{"type":"tool_use","id":"t","name":"{name}","input":{input}}}]}}}}"#
        );
        match whats(&decode_transcript(line.as_bytes()))[..] {
            [AgentWhat::ToolCall { touches, .. }] => touches.clone(),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn edits_are_read_from_each_editing_tool() {
        assert_eq!(
            touches(
                "Write",
                r#"{"file_path":"/w/a.rs","content":"fn a() {}\n\nfn b() {}"}"#
            ),
            vec![FileTouch {
                path: "/w/a.rs".into(),
                how: TouchHow::Inserted(vec![
                    line_hash("fn a() {}"),
                    line_hash(""),
                    line_hash("fn b() {}")
                ]),
            }]
        );
        assert_eq!(
            touches(
                "MultiEdit",
                r#"{"file_path":"/w/b.rs","edits":[{"old_string":"x","new_string":"y1\ny2"},{"old_string":"z","new_string":"w"}]}"#
            ),
            vec![FileTouch {
                path: "/w/b.rs".into(),
                how: TouchHow::Inserted(vec![line_hash("y1"), line_hash("y2"), line_hash("w")]),
            }]
        );
        assert_eq!(
            touches(
                "Bash",
                r#"{"command":"sed -i 's/a/b/' crates/x.rs && cargo fmt --all > /dev/null; curl https://e.com/a.b","description":"d"}"#
            ),
            vec![
                FileTouch {
                    path: "crates/x.rs".into(),
                    how: TouchHow::Shell
                },
                FileTouch {
                    path: "/dev/null".into(),
                    how: TouchHow::Shell
                },
            ]
        );
        assert_eq!(touches("Read", r#"{"file_path":"/w/a.rs"}"#), vec![]);
        assert_eq!(touches("Grep", r#"{"pattern":"x","path":"/w"}"#), vec![]);
    }

    #[test]
    fn plan_tools_become_plan_ops() {
        let todo = r#"{"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"TodoWrite","input":{"todos":[{"content":"Ground","status":"completed","activeForm":"Grounding"},{"content":"Sketch","status":"in_progress","activeForm":"Sketching"},{"content":"Ship","status":"pending","activeForm":"Shipping"}]}}]}}"#;
        let d = decode_transcript(todo.as_bytes());
        assert_eq!(
            whats(&d)[1],
            &AgentWhat::Plan(PlanOp::Replace(vec![
                PlanItem {
                    content: "Ground".into(),
                    status: PlanStatus::Completed
                },
                PlanItem {
                    content: "Sketch".into(),
                    status: PlanStatus::InProgress
                },
                PlanItem {
                    content: "Ship".into(),
                    status: PlanStatus::Pending
                },
            ]))
        );

        let update = r#"{"type":"assistant","uuid":"a","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"TaskUpdate","input":{"taskId":"2","status":"completed"}}]}}"#;
        assert_eq!(
            whats(&decode_transcript(update.as_bytes()))[1],
            &AgentWhat::Plan(PlanOp::Upsert {
                task: "2".into(),
                subject: None,
                status: Some(PlanStatus::Completed)
            })
        );

        let created = r#"{"type":"user","uuid":"u","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"Task #2 created"}]},"toolUseResult":{"task":{"id":"2","subject":"R02 approval diff scroll"}}}"#;
        assert_eq!(
            whats(&decode_transcript(created.as_bytes())),
            vec![
                &AgentWhat::ToolResult {
                    call: ToolCallId("t".into()),
                    is_error: false,
                    output: "Task #2 created".into()
                },
                &AgentWhat::Plan(PlanOp::Upsert {
                    task: "2".into(),
                    subject: Some("R02 approval diff scroll".into()),
                    status: Some(PlanStatus::Pending),
                }),
            ]
        );
    }

    #[test]
    fn tool_results_keep_their_output_and_error_flag() {
        let line = r#"{"type":"user","uuid":"u","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"exit 1","is_error":true},{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"a"},{"type":"image","source":{}},{"type":"text","text":"b"}]}]}}"#;
        assert_eq!(
            whats(&decode_transcript(line.as_bytes())),
            vec![
                &AgentWhat::ToolResult {
                    call: ToolCallId("t1".into()),
                    is_error: true,
                    output: "exit 1".into()
                },
                &AgentWhat::ToolResult {
                    call: ToolCallId("t2".into()),
                    is_error: false,
                    output: "a\n[image]\nb".into()
                },
            ]
        );
    }

    #[test]
    fn known_bookkeeping_records_are_tagged_and_unknown_ones_are_not_hidden() {
        for tag in [
            "attachment",
            "api-request",
            "ai-title",
            "file-history-snapshot",
            "system",
            "queue-operation",
        ] {
            let line = format!(r#"{{"type":"{tag}","uuid":"x"}}"#);
            assert_eq!(
                whats(&decode_transcript(line.as_bytes())),
                vec![&AgentWhat::Other { tag: tag.into() }]
            );
        }
        let unknown = decode_transcript(br#"{"type":"hologram","uuid":"x"}"#);
        assert!(
            matches!(unknown.iter().next(), Some(Observation::Unrecognized { reason }) if reason.contains("hologram"))
        );
        let broken = decode_transcript(b"{\"type\":\"user\"");
        assert!(matches!(
            broken.iter().next(),
            Some(Observation::Unrecognized { .. })
        ));
        let empty =
            decode_transcript(br#"{"type":"assistant","uuid":"a","message":{"content":[]}}"#);
        assert!(matches!(
            empty.iter().next(),
            Some(Observation::Unrecognized { .. })
        ));
    }

    #[test]
    fn a_headless_run_ends_with_its_result() {
        let init = decode_stream_json(
            br#"{"type":"system","subtype":"init","session_id":"s","model":"claude-opus-5-5"}"#,
        );
        assert_eq!(
            whats(&init),
            vec![&AgentWhat::Other {
                tag: "system".into()
            }]
        );
        let result = decode_stream_json(br#"{"type":"result","subtype":"success","is_error":false,"result":"LGTM with one nit","session_id":"s"}"#);
        assert_eq!(
            whats(&result),
            vec![&AgentWhat::Result {
                text: "LGTM with one nit".into(),
                is_error: false
            }]
        );
        let reply = decode_stream_json(br#"{"type":"assistant","message":{"id":"m","model":"claude-opus-5-5","content":[{"type":"text","text":"hi"}]},"session_id":"s"}"#);
        assert_eq!(
            whats(&reply)[0],
            &AgentWhat::Reply {
                text: "hi".into(),
                model: Some(ModelId("claude-opus-5-5".into()))
            }
        );
    }

    #[test]
    fn a_permission_notification_is_a_permission_wait() {
        let hook = decode_hook(br#"{"session_id":"s","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#);
        assert_eq!(
            whats(&hook),
            vec![&AgentWhat::PermissionWait {
                message: "Claude needs your permission to use Bash".into()
            }]
        );
        let stop =
            decode_hook(br#"{"session_id":"s","hook_event_name":"Stop","stop_hook_active":false}"#);
        assert_eq!(
            whats(&stop),
            vec![&AgentWhat::Other {
                tag: "hook:Stop".into()
            }]
        );
    }

    /// Every line of real transcripts decodes to something argus can name. Reads files
    /// outside the repo, so it runs only on request:
    /// `ARGUS_TRANSCRIPTS=~/.claude/projects cargo nextest run -p argus-core --run-ignored all real_`
    #[test]
    #[ignore = "reads real transcripts named by ARGUS_TRANSCRIPTS"]
    fn real_transcripts_have_no_unrecognized_records() {
        let root = std::env::var_os("ARGUS_TRANSCRIPTS").expect("set ARGUS_TRANSCRIPTS");
        let mut dirs = vec![std::path::PathBuf::from(root)];
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        let mut unrecognized = Vec::new();
        while let Some(path) = dirs.pop() {
            if path.is_dir() {
                dirs.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
                if line.is_empty() {
                    continue;
                }
                for obs in decode_transcript(line).iter() {
                    let name = match obs {
                        Observation::Agent(ev) => match &ev.what {
                            AgentWhat::Other { tag } => format!("Other({tag})"),
                            what => format!("{what:?}")
                                .split([' ', '(', '{'])
                                .next()
                                .unwrap()
                                .to_owned(),
                        },
                        Observation::Unrecognized { reason } => {
                            unrecognized.push(format!("{}:{}: {reason}", path.display(), n + 1));
                            "Unrecognized".to_owned()
                        }
                        other => format!("{other:?}"),
                    };
                    *counts.entry(name).or_default() += 1;
                }
            }
        }
        for (name, n) in &counts {
            eprintln!("{n:>8} {name}");
        }
        assert!(
            unrecognized.is_empty(),
            "{:#?}",
            &unrecognized[..unrecognized.len().min(10)]
        );
    }

    #[test]
    fn only_a_notification_about_permission_is_a_permission_wait() {
        let hook = |name: &str, message: &str| {
            let line = format!(r#"{{"hook_event_name":"{name}","message":"{message}"}}"#);
            whats(&decode_hook(line.as_bytes()))[0].clone()
        };
        assert!(matches!(
            hook("Notification", "Claude needs your permission to use Bash"),
            AgentWhat::PermissionWait { .. }
        ));
        assert_eq!(
            hook("Notification", "Claude is waiting for your input"),
            AgentWhat::Other {
                tag: "hook:Notification".into()
            }
        );
        assert_eq!(
            hook("PreToolUse", "asks for permission"),
            AgentWhat::Other {
                tag: "hook:PreToolUse".into()
            }
        );
    }

    /// Coarse on purpose, but each rule is pinned: a hunk a shell command names is "via
    /// shell" instead of unexplained.
    #[test]
    fn shell_commands_name_the_paths_they_touch() {
        let cases: [(&str, &[&str]); 9] = [
            ("sed -i s/a/b/ src/lib.rs", &["src/lib.rs"]),
            ("cargo fmt -- --check", &[]),
            ("rm -rf target/ && touch out.txt", &["out.txt"]),
            ("cat 'docs/a.md'|wc -l", &["docs/a.md"]),
            ("./run.sh;make", &["./run.sh"]),
            ("echo hello > .env", &[]),
            ("cp a.rs a.rs", &["a.rs"]),
            ("python -c x.y(1)", &["x.y"]),
            ("ls ~/notes/todo.md -o -f.txt", &["~/notes/todo.md"]),
        ];
        for (command, want) in cases {
            assert_eq!(shell_paths(command), want, "{command}");
        }
    }

    #[test]
    fn task_updates_read_every_status_the_agent_writes() {
        let update = |status: &str| {
            let line = format!(
                r#"{{"type":"assistant","uuid":"a","message":{{"id":"m","content":[{{"type":"tool_use","id":"t","name":"TaskUpdate","input":{{"taskId":"2","status":"{status}"}}}}]}}}}"#
            );
            whats(&decode_transcript(line.as_bytes()))
                .into_iter()
                .find_map(|w| match w {
                    AgentWhat::Plan(PlanOp::Upsert { status, .. }) => Some(*status),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(update("pending"), Some(PlanStatus::Pending));
        assert_eq!(update("in_progress"), Some(PlanStatus::InProgress));
        assert_eq!(update("completed"), Some(PlanStatus::Completed));
        assert_eq!(update("deleted"), Some(PlanStatus::Deleted));
        assert_eq!(update("someday"), None);
    }
}
