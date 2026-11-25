//! `argus`: the user's and the agents' command line. A client of the same API as the
//! desktop and the phone, over the host's Unix socket (`socket.rs`).
//!
//! It names ops as `argus_api::OPS` does and passes their input as JSON, so every op is
//! reachable and none can drift from the API:
//!
//! ```text
//! argus ops                          every op, its kind, and whether agents may call it
//! argus call presets
//! argus call set_task '{"title": "Parser", "weight": 2}'
//! argus watch job '{"job": "<id>"}'  prints the value again each time it changes
//! argus hook                         an agent CLI's hook: stdin, verbatim, to `hook`
//! argus mcp                          the ops open to agents as MCP tools (`mcp.rs`)
//! ```
//!
//! The socket is `$ARGUS_SOCKET`, else the host's default (`argus_api::place::socket`).
//! A run argus started has `ARGUS_RUN` and `ARGUS_TOKEN` in its environment, and the
//! host then treats the caller as that run's agent; otherwise the caller is the user.

mod mcp;
mod socket;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use argus_api::wire::Wire;
use argus_api::{ApiError, Client, Transport, vm::HookPayload};
use argus_json::Json;

use socket::SocketTransport;

const USAGE: &str = "usage: argus ops
       argus call <op> [<input JSON>]
       argus watch <op> [<input JSON>]
       argus hook
       argus mcp";

enum Command {
    Ops,
    Call { op: String, input: Json },
    Watch { op: String, input: Json },
    Hook,
    Mcp,
}

fn parse(args: &[String]) -> Option<Command> {
    let input = |text: Option<&String>| match text {
        None => Some(Json::Obj(Vec::new())),
        Some(text) => argus_json::parse(text.as_bytes()).ok(),
    };
    match args {
        [cmd] if cmd == "ops" => Some(Command::Ops),
        [cmd] if cmd == "hook" => Some(Command::Hook),
        [cmd] if cmd == "mcp" => Some(Command::Mcp),
        [cmd, op, rest @ ..] if cmd == "call" && rest.len() <= 1 => Some(Command::Call {
            op: op.clone(),
            input: input(rest.first())?,
        }),
        [cmd, op, rest @ ..] if cmd == "watch" && rest.len() <= 1 => Some(Command::Watch {
            op: op.clone(),
            input: input(rest.first())?,
        }),
        _ => None,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the CLI's one read of its environment"
)]
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = parse(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("argus: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    let refused = |op: &str, e: ApiError| format!("{op}: {}", argus_json::write(&e.to_json()));
    match command {
        Command::Ops => {
            for op in argus_api::OPS {
                let kind = format!("{:?}", op.kind).to_lowercase();
                let access = format!("{:?}", op.access).to_lowercase();
                println!("{} {kind} {access}", op.name);
            }
            Ok(())
        }
        Command::Call { op, input } => {
            let value = connect()?.call(&op, input).map_err(|e| refused(&op, e))?;
            println!("{}", argus_json::write(&value));
            Ok(())
        }
        Command::Watch { op, input } => connect()?
            .subscribe(&op, input, |value| {
                println!("{}", argus_json::write(&value))
            })
            .map_err(|e| refused(&op, e)),
        Command::Hook => {
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("cannot read the hook's input: {e}"))?;
            // Nothing on stdout: an agent CLI may read a hook's output as instructions.
            let mut client = Client {
                transport: connect()?,
            };
            client
                .hook(&HookPayload { body })
                .map(|_| ())
                .map_err(|e| refused("hook", e))
        }
        Command::Mcp => {
            let mut transport = Reconnecting { socket: None };
            mcp::serve(&mut transport, std::io::stdin().lock(), std::io::stdout())
                .map_err(|e| format!("mcp: {e}"))
        }
    }
}

/// Connects on the first call, so an MCP client can start before the host, and again
/// after a call that failed inside the host or on the way to it.
struct Reconnecting {
    socket: Option<SocketTransport>,
}

impl Transport for Reconnecting {
    fn call(&mut self, op: &str, input: Json) -> Result<Json, ApiError> {
        let socket = match &mut self.socket {
            Some(socket) => socket,
            None => self
                .socket
                .insert(connect().map_err(|what| ApiError::Internal { what })?),
        };
        let reply = socket.call(op, input);
        if let Err(ApiError::Internal { .. }) = reply {
            self.socket = None;
        }
        reply
    }
}

fn connect() -> Result<SocketTransport, String> {
    let path: PathBuf = env("ARGUS_SOCKET")
        .map(PathBuf::from)
        .or_else(|| argus_api::place::socket(&env))
        .ok_or("cannot tell where the host's socket is: set ARGUS_SOCKET or HOME")?;
    SocketTransport::connect(&path, env("ARGUS_TOKEN"), env("ARGUS_RUN"))
        .map_err(|e| format!("cannot reach the host at {}: {e}", path.display()))
}
