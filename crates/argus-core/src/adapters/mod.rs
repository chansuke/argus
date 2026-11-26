//! One module per external tool. Each owns both directions of its format: how to invoke
//! the tool and how to read what it wrote. Private to the crate: views see only
//! `Observation`s, the host sees only `CommandSpec`s.

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod eas;
pub(crate) mod lean;
pub(crate) mod llm;
pub(crate) mod tlc;
mod touch;

/// The MCP server argus gives every agent it starts: `<tools.argus> mcp`, named
/// `argus`, so its tools are `mcp__argus__<op>` to Claude Code. The server learns the run
/// it speaks for from these variables in the agent's environment (the host adds
/// `ARGUS_TOKEN`).
pub(crate) const MCP_SERVER: &str = "argus";
pub(crate) const MCP_ARGS: &[&str] = &["mcp"];
pub(crate) const MCP_ENV: &[&str] = &["ARGUS_RUN", "ARGUS_SOCKET", "ARGUS_TOKEN"];

use crate::source::{Decoded, Observation, SourceKind};

/// Bumped whenever the adapter's output changes; part of every checkpoint key, so a
/// fixed adapter invalidates exactly the checkpoints built with the old one.
pub(crate) fn decoder_version(kind: SourceKind) -> u32 {
    match kind {
        SourceKind::ClaudeTranscript | SourceKind::ClaudeStreamJson | SourceKind::ClaudeHook => {
            claude::VERSION
        }
        SourceKind::CodexRollout | SourceKind::CodexExec | SourceKind::CodexAgentExec => {
            codex::VERSION
        }
        SourceKind::TlcTool => tlc::VERSION,
        SourceKind::LeanText | SourceKind::LeanJson => lean::VERSION,
        SourceKind::EasEvents => eas::VERSION,
        SourceKind::Terminal | SourceKind::PlainText => 1,
    }
}

/// Decodes one frame of `kind` with the adapter that owns its format.
pub(crate) fn decode(kind: SourceKind, bytes: &[u8]) -> Decoded {
    match kind {
        SourceKind::ClaudeTranscript => claude::decode_transcript(bytes),
        SourceKind::ClaudeStreamJson => claude::decode_stream_json(bytes),
        SourceKind::ClaudeHook => claude::decode_hook(bytes),
        SourceKind::CodexRollout => codex::decode_rollout(bytes),
        SourceKind::CodexExec => codex::decode_exec(bytes),
        SourceKind::CodexAgentExec => codex::decode_agent_exec(bytes),
        SourceKind::TlcTool => tlc::decode(bytes),
        SourceKind::LeanText => lean::decode_text(bytes),
        SourceKind::LeanJson => lean::decode_json(bytes),
        SourceKind::EasEvents => eas::decode_events(bytes),
        SourceKind::Terminal => Decoded::one(Observation::Terminal),
        SourceKind::PlainText => Decoded::one(Observation::Line(
            String::from_utf8_lossy(bytes).into_owned(),
        )),
    }
}
