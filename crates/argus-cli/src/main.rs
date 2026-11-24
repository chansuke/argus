//! `argus`: the user's and the agents' command line. A client of the same API as the
//! desktop and the phone (generated `argus_api::Client`), over the host's Unix socket.
//!
//! Human:   argus run --agent claude:opus --agent codex:gpt-5 "Implement X"
//!          argus status | argus timeline <job> --since 1h | argus raw <job> --source <s>
//!          argus review <job> [--preset panel | --model claude:opus ...] --lens security
//!          argus packet <job>            (the review screen as text, agenda by tier)
//!          argus promote <job> <finding> | argus feedback <job> (preview what will be sent)
//!          argus decide <job> approve|request-changes|abandon [--note ...] | argus merge <job>
//!          argus check <job> tla specs/Raft.tla --engine tlars
//!          argus check <job> device apps/mobile --platform ios --app fingerprint:<hash>
//!          argus stats --period last-7d --by model | argus insights --period 2026-11
//!          argus skill accept <candidate> --to <skills dir>
//! Agent:   argus task add "Parser" --weight 2 | argus task done <id> --result "..."
//!          argus task block <id> "needs API key" | argus check --self tla <spec> | argus hook
//!          (ARGUS_RUN / ARGUS_SOCKET / ARGUS_TOKEN come from the run's env)
//! MCP:     argus mcp — stdio MCP server exposing the `[agent]` ops as tools.

#![expect(
    dead_code,
    unused_variables,
    reason = "todo!() stubs (socket transport, MCP server) take arguments and hold items they do not use yet"
)]

mod mcp;

use argus_api::{ApiError, Transport};
use argus_json::Json;

/// Unix-socket transport: `Hello`, then one `Call` per request.
struct SocketTransport {
    // stream: UnixStream, next_id: u32
}

impl Transport for SocketTransport {
    fn call(&mut self, op: &str, input: Json) -> Result<Json, ApiError> {
        todo!("not implemented")
    }
}

fn main() {
    // TODO: parse args (no clap: std-only); build the input view model; call the
    //       generated Client method; print. `argus hook` reads stdin verbatim into
    //       HookPayload. `argus mcp` -> mcp::serve(client).
    todo!("not implemented")
}
