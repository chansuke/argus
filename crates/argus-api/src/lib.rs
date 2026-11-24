//! The one command / query / subscription surface of argus.
//!
//! ```text
//!            ops.rs (the list)
//!   ┌───────────┼──────────────┬──────────────────┐
//!   Handler     dispatch()     Client<T> (Rust)   typescript() -> @argus/client gen/api.ts
//!   (host)      ▲   ▲   ▲
//!               │   │   └ Unix socket (CLI, agents' `argus` / MCP)   argus-host
//!               │   └──── WebSocket   (Expo app on the phone)        argus-host
//!               └──────── Tauri IPC   (React desktop webview)        argus-app
//! ```
//! A transport only moves frames and identifies the caller. Adding an op is one line in
//! `ops.rs`; every transport and both frontends get it, and the host fails to compile
//! until it implements the new `Handler` method.

mod ops;
pub mod vm;
pub mod wire;

use argus_json::Json;

pub use ops::{Client, Handler, OPS, dispatch, typescript};
use wire::vm_enum;

/// Who is calling, as established by the transport (never by the payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The desktop app, or the `argus` CLI run by the user on this machine.
    Local,
    /// A paired phone (pairing token checked by the transport). It is the human, with the
    /// same ops as the desktop: decide, merge, answer permission prompts (user decision,
    /// 2026-11-17). `device` names the pairing, so unpairing revokes exactly it.
    Phone { device: String },
    /// A process launched by argus for this run (`ARGUS_RUN` + the run's socket token).
    Agent { run: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Query,
    Command,
    Subscribe,
}

/// Declared per op in `ops.rs` (`[agent]` or nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpAccess {
    Human,
    /// Also callable by agent runs.
    Agent,
}

#[derive(Clone, Copy, Debug)]
pub struct OpInfo {
    pub name: &'static str,
    pub kind: OpKind,
    pub access: OpAccess,
}

impl Caller {
    /// The one access rule, applied by `dispatch` before the input is even parsed.
    pub fn may(&self, op: &OpInfo) -> bool {
        match self {
            Caller::Local | Caller::Phone { .. } => true,
            Caller::Agent { .. } => op.access == OpAccess::Agent,
        }
    }
}

vm_enum! { ApiError {
    UnknownOp { op: String },
    Forbidden { op: String },
    BadInput { path: String, what: String },
    NotFound { what: String },
    /// The ledger refused the write (wrong author or scope).
    Rejected { what: String },
    /// A verdict or merge named a tree that is no longer the head: reload and look again.
    StaleHead { head: Option<String> },
    Internal { what: String },
} }

vm_enum! {
    /// Frames for the stream transports (WebSocket, Unix socket: one JSON object per
    /// message / line). Tauri IPC uses `invoke` + `Channel` instead and needs no frames.
    ClientFrame {
    /// First frame. Phone: pairing token. Agent: run id + per-run token from its env.
    Hello { token: Option<String>, run: Option<String> },
    Call { id: u32, op: String, input: Json },
    Subscribe { id: u32, op: String, input: Json },
    Unsubscribe { id: u32 },
} }

vm_enum! { ServerFrame {
    Reply { id: u32, ok: Option<Json>, err: Option<ApiError> },
    /// Current value of subscription `id`, sent once on subscribe and again on change.
    Push { id: u32, value: Json },
} }

/// Client side of any transport (Rust).
pub trait Transport {
    fn call(&mut self, op: &str, input: Json) -> Result<Json, ApiError>;
}
