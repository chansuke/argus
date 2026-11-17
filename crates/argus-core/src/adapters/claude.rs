//! Claude Code: interactive launch, transcript JSONL, headless stream-json, hook payloads.
//! The transcript format is undocumented and changes between versions; this module is
//! the only code that knows it.

use crate::event::AgentLaunch;
use crate::plan::{CommandSpec, PermissionAnswer, PlanEnv};
use crate::source::Decoded;

pub(crate) const VERSION: u32 = 1;

/// `claude --session-id <run-uuid> [--model m] --mcp-config <argus-mcp.json> <prompt>`, or
/// `claude --resume <earlier-run-uuid> ... <prompt>` when `launch.resume` is set (feedback
/// delivery), under a PTY in the job worktree.
///
/// The run id is the session id, so the transcript path is known before the process
/// starts: watch `.claude/projects/*/<uuid>.jsonl` and `.claude/projects/*/<uuid>/subagents/*.jsonl`
/// (glob on the slug instead of re-deriving Claude's cwd-to-slug rule).
/// Env: `ARGUS_RUN`, `ARGUS_SOCKET`, so `argus task ...` and `argus hook` inside the agent
/// know who they speak for.
pub(crate) fn launch(launch: &AgentLaunch, env: &PlanEnv) -> CommandSpec {
    todo!("not implemented")
}

/// Unverified: the prompt is a numbered menu (1 yes, 2 yes and don't ask again, 3 no)
/// in current versions; confirm per version before relying on it.
pub(crate) fn permission_keys(answer: PermissionAnswer) -> Vec<u8> {
    todo!("not implemented")
}

/// One transcript line.
///
/// TODO:
/// - parse JSON; on failure -> `Unrecognized { reason }`.
/// - common: `uuid` -> record, `parentUuid` -> parent, `timestamp` (RFC 3339) -> at,
///   `isSidechain` -> sidechain.
/// - `type: "assistant"`: for each `message.content[]` block: `thinking` -> Thinking,
///   `text` -> Reply, `tool_use` -> ToolCall (input re-serialized byte-for-byte from the
///   raw slice, not re-encoded) with `touches`: `Write.content`, `Edit.new_string`,
///   `MultiEdit.edits[].new_string` -> Inserted(line_hash of each line); `Bash` commands
///   naming a path -> Shell; additionally `TodoWrite` -> Plan(Replace),
///   `TaskUpdate` -> Plan(Upsert{taskId,status,subject}); `message.usage` -> Usage;
///   `message.stop_reason == "end_turn"` -> TurnEnded.
/// - `type: "user"`: string content -> Prompt; `tool_result` blocks -> ToolResult
///   (`is_error`); `toolUseResult.task.{id,subject}` -> Plan(Upsert{status: Pending}).
/// - `attachment`, `api-request`, `api-request-blob`, `permission-mode`, `mode`,
///   `last-prompt`, `ai-title`, `pr-link`, `bridge-session` -> Other { tag }.
/// - any other `type` -> Unrecognized (shows up in the raw view; bump VERSION once modeled).
pub(crate) fn decode_transcript(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}

/// `claude -p --output-format stream-json --verbose` line. Same record shapes as the
/// transcript plus `system`/`init` and the final `result` (-> `Result { text, is_error }`).
pub(crate) fn decode_stream_json(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}

/// Hook payload (`Notification`, `Stop`, `PreToolUse`, ...) forwarded by `argus hook`.
/// `Notification` -> `PermissionWait`, the precise "waiting for permission" signal.
pub(crate) fn decode_hook(bytes: &[u8]) -> Decoded {
    todo!("not implemented")
}
