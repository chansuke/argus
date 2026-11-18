//! Raw sources: foreign byte streams preserved verbatim, and their decoding into
//! domain observations.
//!
//! Stored: the bytes (one frame per meaningful unit, e.g. one JSONL line).
//! Derived on every replay: the observations. So when Claude Code changes its
//! undocumented format, fixing the adapter fixes all history, and the bytes the adapter
//! misread are still there to look at.
//!
//! Exhaustiveness invariant: every frame decodes to at least one observation
//! ([`Decoded`] is non-empty by construction). A frame the adapter cannot read becomes
//! `Observation::Unrecognized`, never nothing, so no view can silently hide a record.

use std::path::PathBuf;

use crate::adapters;
use crate::ids::*;
use crate::models::AgentCli;

/// Header of a source log. The single truth for how its frames are decoded and which
/// run they belong to (no separate "source attached" event duplicates it).
#[derive(Clone, PartialEq, Debug)]
pub struct SourceMeta {
    pub id: SourceId,
    pub run: RunId,
    pub attempt: Attempt,
    pub kind: SourceKind,
    pub origin: SourceOrigin,
}

impl SourceMeta {
    /// Deterministic id: reopening the same stream of the same attempt after a host
    /// restart resolves to the same log, so ingestion resumes instead of duplicating.
    pub fn derive_id(run: RunId, attempt: Attempt, origin: &SourceOrigin) -> SourceId {
        use std::os::unix::ffi::OsStrExt;
        let mut key = Vec::new();
        key.extend_from_slice(&run.0.0.to_le_bytes());
        key.extend_from_slice(&attempt.0.to_le_bytes());
        let (tag, path): (u8, &[u8]) = match origin {
            SourceOrigin::Stdout => (0, b""),
            SourceOrigin::Stderr => (1, b""),
            SourceOrigin::Terminal => (2, b""),
            SourceOrigin::Stdin => (3, b""),
            SourceOrigin::File(p) => (4, p.as_os_str().as_bytes()),
            SourceOrigin::Hook => (5, b""),
        };
        key.push(tag);
        key.extend_from_slice(path);
        SourceId(Id128::derive("source", &key))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SourceOrigin {
    Stdout,
    Stderr,
    Terminal,
    /// What argus fed the process (the full prompt an LLM received).
    Stdin,
    /// A file argus tails (agent transcript, subagent transcript).
    File(PathBuf),
    /// Hook payloads forwarded by `argus hook` from inside the agent.
    Hook,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SourceKind {
    /// `~/.claude/projects/<slug>/<session>.jsonl` and `<session>/subagents/*.jsonl`.
    ClaudeTranscript,
    /// `claude -p --output-format stream-json` stdout (reviews, summaries, insights).
    ClaudeStreamJson,
    ClaudeHook,
    /// `~/.codex/sessions/**/rollout-*.jsonl` of an interactive Codex session.
    CodexRollout,
    /// `codex exec --json` stdout (headless runs).
    CodexExec,
    Terminal,
    /// `-tool` output of java TLC or tlars.
    TlcTool,
    LeanText,
    LeanJson,
    /// `eas simulator:events --follow --json`: controller actions on a device session.
    EasEvents,
    PlainText,
}

/// What a source is, as far as views care. Views name families, never kinds
/// (`Projection::READS`), and this match is exhaustive: a new agent's formats are placed
/// here once and every view that reads the family sees them. Nothing can be silently
/// invisible because a hand-written `matches!` list somewhere missed it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SourceFamily {
    /// An interactive agent's own conversation log.
    AgentSession,
    /// A headless LLM run (review, summary, insight): conversation plus a final result.
    AgentHeadless,
    /// Out-of-band agent signals (Claude hook payloads: permission waits, stop).
    AgentHook,
    /// Formal verification output (TLC/tlars, Lean).
    Checker,
    /// Device-session controller events.
    Device,
    /// PTY bytes; no record structure.
    Terminal,
    /// Unstructured lines (stderr, stdin echo).
    Text,
}

impl SourceKind {
    pub fn family(self) -> SourceFamily {
        match self {
            SourceKind::ClaudeTranscript | SourceKind::CodexRollout => SourceFamily::AgentSession,
            SourceKind::ClaudeStreamJson | SourceKind::CodexExec => SourceFamily::AgentHeadless,
            SourceKind::ClaudeHook => SourceFamily::AgentHook,
            SourceKind::TlcTool | SourceKind::LeanText | SourceKind::LeanJson => {
                SourceFamily::Checker
            }
            SourceKind::EasEvents => SourceFamily::Device,
            SourceKind::Terminal => SourceFamily::Terminal,
            SourceKind::PlainText => SourceFamily::Text,
        }
    }

    /// Bumped whenever the adapter's output changes; part of every checkpoint key, so a
    /// fixed adapter invalidates exactly the checkpoints built with the old one.
    pub fn decoder_version(self) -> u32 {
        match self {
            SourceKind::ClaudeTranscript
            | SourceKind::ClaudeStreamJson
            | SourceKind::ClaudeHook => adapters::claude::VERSION,
            SourceKind::CodexRollout | SourceKind::CodexExec => adapters::codex::VERSION,
            SourceKind::TlcTool => adapters::tlc::VERSION,
            SourceKind::LeanText | SourceKind::LeanJson => adapters::lean::VERSION,
            SourceKind::EasEvents => adapters::eas::VERSION,
            SourceKind::Terminal | SourceKind::PlainText => 1,
        }
    }

    pub fn framer(self, resume_at: u64) -> Framer {
        let framing = match self {
            SourceKind::TlcTool => Framing::TlcMessages,
            SourceKind::Terminal => Framing::Chunks,
            _ => Framing::Lines,
        };
        Framer {
            framing,
            next_offset: resume_at,
            pending: Vec::new(),
            scanned: 0,
            in_block: false,
        }
    }

    pub(crate) fn decode(self, bytes: &[u8]) -> Decoded {
        match self {
            SourceKind::ClaudeTranscript => adapters::claude::decode_transcript(bytes),
            SourceKind::ClaudeStreamJson => adapters::claude::decode_stream_json(bytes),
            SourceKind::ClaudeHook => adapters::claude::decode_hook(bytes),
            SourceKind::CodexRollout => adapters::codex::decode_rollout(bytes),
            SourceKind::CodexExec => adapters::codex::decode_exec(bytes),
            SourceKind::TlcTool => adapters::tlc::decode(bytes),
            SourceKind::LeanText => adapters::lean::decode_text(bytes),
            SourceKind::LeanJson => adapters::lean::decode_json(bytes),
            SourceKind::EasEvents => adapters::eas::decode_events(bytes),
            SourceKind::Terminal => Decoded::one(Observation::Terminal),
            SourceKind::PlainText => Decoded::one(Observation::Line(
                String::from_utf8_lossy(bytes).into_owned(),
            )),
        }
    }
}

impl AgentCli {
    /// The format of the agent's own session log (what `Import` and the tailer read).
    pub fn session_kind(self) -> SourceKind {
        match self {
            AgentCli::Claude => SourceKind::ClaudeTranscript,
            AgentCli::Codex => SourceKind::CodexRollout,
        }
    }
}

/// One unit of a source log.
///
/// Ingestion progress lives in the log itself: the resume point is the end
/// (`offset + len`) of the last `Bytes` frame after the last `Discontinuity`, or 0.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RawFrame {
    /// Verbatim bytes at `offset` of the source.
    Bytes { offset: u64, bytes: Vec<u8> },
    /// The file was truncated or rewritten under the tailer (an agent compacting or
    /// rewriting its transcript). Frames after this one are the file's new content from
    /// offset 0; nothing before it is ever edited, so every stored reference stays valid.
    /// Records seen again after it are dropped by the one record-dedup rule
    /// (`ledger::Applied`), so re-reading the new file double-counts nothing.
    Discontinuity { stored_end: u64, file_len: u64 },
}

impl RawFrame {
    /// Stored payload: a one-byte tag, then the offset or lengths, then the bytes verbatim.
    pub(crate) fn encode(&self) -> Vec<u8> {
        match self {
            RawFrame::Bytes { offset, bytes } => {
                let mut out = Vec::with_capacity(9 + bytes.len());
                out.push(0);
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
            RawFrame::Discontinuity {
                stored_end,
                file_len,
            } => {
                let mut out = Vec::with_capacity(17);
                out.push(1);
                out.extend_from_slice(&stored_end.to_le_bytes());
                out.extend_from_slice(&file_len.to_le_bytes());
                out
            }
        }
    }
    pub(crate) fn decode(payload: &[u8]) -> Option<RawFrame> {
        let u64_at = |i: usize| Some(u64::from_le_bytes(payload.get(i..i + 8)?.try_into().ok()?));
        match payload.first()? {
            0 => Some(RawFrame::Bytes {
                offset: u64_at(1)?,
                bytes: payload[9..].to_vec(),
            }),
            1 if payload.len() == 17 => Some(RawFrame::Discontinuity {
                stored_end: u64_at(1)?,
                file_len: u64_at(9)?,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Framing {
    Lines,
    /// `@!@!@STARTMSG code:class @!@!@ ... @!@!@ENDMSG code @!@!@` blocks; stray lines alone.
    TlcMessages,
    /// Arbitrary chunks as read (terminal bytes have no record structure).
    Chunks,
}

/// Cuts a byte stream into frames. Holds back an incomplete trailing unit until more bytes
/// arrive (a transcript line being written) or the stream ends.
#[derive(Debug)]
pub struct Framer {
    framing: Framing,
    next_offset: u64,
    pending: Vec<u8>,
    /// `pending[..scanned]` holds whole lines already looked at (an open TLC block).
    scanned: usize,
    in_block: bool,
}

const TLC_START: &[u8] = b"@!@!@STARTMSG";
const TLC_END: &[u8] = b"@!@!@ENDMSG";

impl Framer {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<RawFrame> {
        if let Framing::Chunks = self.framing {
            return self.emit(bytes).into_iter().collect();
        }
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut start = 0;
        while let Some(nl) = self.pending[self.scanned..]
            .iter()
            .position(|&b| b == b'\n')
        {
            let line_start = self.scanned;
            let line_end = line_start + nl + 1;
            self.scanned = line_end;
            let line = &self.pending[line_start..line_end];
            let ends_frame = match self.framing {
                Framing::Lines | Framing::Chunks => true,
                Framing::TlcMessages if self.in_block => {
                    self.in_block = !line.starts_with(TLC_END);
                    !self.in_block
                }
                Framing::TlcMessages => {
                    self.in_block = line.starts_with(TLC_START);
                    !self.in_block
                }
            };
            if ends_frame {
                let bytes = self.pending[start..line_end].to_vec();
                frames.extend(self.emit(&bytes));
                start = line_end;
            }
        }
        self.pending.drain(..start);
        self.scanned -= start;
        frames
    }
    /// End of stream: the incomplete remainder (a last line without `\n`) becomes a frame.
    pub fn finish(&mut self) -> Option<RawFrame> {
        let rest = std::mem::take(&mut self.pending);
        self.scanned = 0;
        self.in_block = false;
        self.emit(&rest)
    }

    fn emit(&mut self, bytes: &[u8]) -> Option<RawFrame> {
        if bytes.is_empty() {
            return None;
        }
        let offset = self.next_offset;
        self.next_offset += bytes.len() as u64;
        Some(RawFrame::Bytes {
            offset,
            bytes: bytes.to_vec(),
        })
    }
}

/// Non-empty list of observations decoded from one frame.
#[derive(Clone, PartialEq, Debug)]
pub struct Decoded {
    first: Observation,
    rest: Vec<Observation>,
}

impl Decoded {
    pub fn one(o: Observation) -> Self {
        Decoded {
            first: o,
            rest: Vec::new(),
        }
    }
    pub fn many(first: Observation, rest: Vec<Observation>) -> Self {
        Decoded { first, rest }
    }
    pub fn iter(&self) -> impl Iterator<Item = &Observation> {
        std::iter::once(&self.first).chain(&self.rest)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Observation {
    Agent(AgentEvent),
    /// Terminal bytes; the content is the raw frame itself.
    Terminal,
    Tlc(TlcMessage),
    Lean(LeanMessage),
    Device(DeviceAction),
    Line(String),
    /// A `RawFrame::Discontinuity`: the timeline shows it; nothing is hidden.
    Discontinuity {
        stored_end: u64,
        file_len: u64,
    },
    Unrecognized {
        reason: String,
    },
}

/// Agent-neutral reading of one agent record. Adapters for every CLI produce this shape.
#[derive(Clone, PartialEq, Debug)]
pub struct AgentEvent {
    pub record: Option<RecordKey>,
    pub parent: Option<RecordKey>,
    /// The record's own timestamp, when it has one.
    pub at: Option<UnixMillis>,
    /// Claude Code `isSidechain`: part of a subagent's conversation.
    pub sidechain: bool,
    pub what: AgentWhat,
}

#[derive(Clone, PartialEq, Debug)]
pub enum AgentWhat {
    Prompt {
        text: String,
    },
    Reply {
        text: String,
        model: Option<ModelId>,
    },
    Thinking {
        text: String,
    },
    /// `input_json` is the tool input exactly as the agent sent it. `touches` is the
    /// adapter's reading of which files the call edits (it alone knows tool names such as
    /// `Edit`, `Write`, `MultiEdit`, `apply_patch`, `Bash`); empty for read-only tools.
    ToolCall {
        call: ToolCallId,
        name: ToolName,
        input_json: String,
        touches: Vec<FileTouch>,
    },
    ToolResult {
        call: ToolCallId,
        is_error: bool,
        output: String,
    },
    /// Hook `Notification` while a tool call is open: the agent waits for permission.
    PermissionWait {
        message: String,
    },
    Usage(TokenUsage),
    /// The agent's own plan, observed without any cooperation from the agent.
    Plan(PlanOp),
    TurnEnded {
        stop_reason: Option<String>,
    },
    /// Final answer of a headless run (`stream-json` `result` record).
    Result {
        text: String,
        is_error: bool,
    },
    /// A record type argus knows but does not model (ai-title, pr-link, mode, ...).
    Other {
        tag: String,
    },
}

/// One file an agent's tool call edits. `path` is as the agent wrote it (usually
/// absolute); the transcript view makes it repo-relative with the job's worktree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileTouch {
    pub path: String,
    pub how: TouchHow,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TouchHow {
    /// Structured edit: the inserted lines are known (`diff::line_hash` of each).
    Inserted(Vec<u64>),
    /// A shell command that names the path (`sed -i`, a generator): content unknown.
    Shell,
}

/// Every plan op is an assignment, so it is decodable from one frame alone (decoders keep
/// no state across frames) and harmless to see twice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PlanOp {
    /// `TodoWrite`: the whole list, replacing the previous one.
    Replace(Vec<PlanItem>),
    /// `TaskCreate` result (`toolUseResult.task.{id,subject}`) or `TaskUpdate` call
    /// (`taskId`, `status`): one item, keyed by the agent's own task id.
    Upsert {
        task: String,
        subject: Option<String>,
        status: Option<PlanStatus>,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlanItem {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// One TLC `-tool` message. `code` is the TLC message code (tlars-msg catalog).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TlcMessage {
    pub code: u32,
    pub class: u8,
    pub text: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LeanMessage {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub severity: LeanSeverity,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeanSeverity {
    Error,
    Warning,
    Info,
}

/// One controller action on a device session (open app, press, screenshot, ...).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceAction {
    pub action: String,
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_frames_round_trip_through_their_stored_form() {
        let frames = [
            RawFrame::Bytes {
                offset: 0,
                bytes: Vec::new(),
            },
            RawFrame::Bytes {
                offset: u64::MAX - 3,
                bytes: b"{\"type\":\"user\"}\n".to_vec(),
            },
            RawFrame::Discontinuity {
                stored_end: 4096,
                file_len: 12,
            },
        ];
        for f in frames {
            assert_eq!(RawFrame::decode(&f.encode()), Some(f));
        }
    }

    #[test]
    fn a_malformed_stored_frame_is_rejected() {
        assert_eq!(RawFrame::decode(b""), None);
        assert_eq!(RawFrame::decode(&[0, 1, 2]), None);
        assert_eq!(RawFrame::decode(&[9; 20]), None);
        let mut disc = RawFrame::Discontinuity {
            stored_end: 1,
            file_len: 2,
        }
        .encode();
        disc.push(0);
        assert_eq!(RawFrame::decode(&disc), None);
    }
}
