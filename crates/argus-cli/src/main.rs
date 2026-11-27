//! `argus`: the user's and the agents' command line. A client of the same API as the
//! desktop and the phone, over the host's Unix socket (`socket.rs`).
//!
//! Every op in `argus_api::OPS` is a subcommand that takes the op's fields as arguments
//! (`args.rs`, read against the op's input schema), so every op is reachable and none
//! can drift from the API:
//!
//! ```text
//! argus ops                          every op, its kind, and whether agents may call it
//! argus help <op>                    what the op does and the arguments it takes
//! argus request-review <job> --reviewers preset:quick --lens security
//! argus watch job <job>              prints the value again each time it changes
//! argus call set_task '{"job": "<id>", "title": "Parser"}'   input as JSON
//! argus hook                         an agent CLI's hook: stdin, verbatim, to `hook`
//! argus mcp                          the ops open to agents as MCP tools (`mcp.rs`)
//! ```
//!
//! The socket is `$ARGUS_SOCKET`, else the host's default (`argus_api::place::socket`).
//! A run argus started has `ARGUS_RUN` and `ARGUS_TOKEN` in its environment, and the
//! host then treats the caller as that run's agent; otherwise the caller is the user.

mod args;
mod mcp;
mod socket;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use argus_api::wire::Wire;
use argus_api::{ApiError, Client, OpInfo, Transport, vm::HookPayload};
use argus_json::Json;

use socket::SocketTransport;

const USAGE: &str = "usage: argus ops
       argus help <op>
       argus <op> [<argument>...]
       argus watch <op> [<argument>...]
       argus call <op> [<input JSON>]
       argus hook
       argus mcp";

/// The words that are not op names. `hook` is also an op: `argus hook` is that op, fed
/// from stdin.
const COMMANDS: &[&str] = &["ops", "help", "call", "watch", "hook", "mcp"];

enum Command {
    Ops,
    Help { op: &'static OpInfo },
    Call { op: String, input: Json },
    Watch { op: String, input: Json },
    Hook,
    Mcp,
}

/// Why the arguments name no command: the general usage, or what is wrong with one op's.
enum Misuse {
    Usage,
    Op { op: &'static OpInfo, what: String },
}

/// An op by the name typed: `request-review` or `request_review`.
fn op_named(name: &str) -> Option<&'static OpInfo> {
    let name = name.replace('-', "_");
    argus_api::OPS.iter().find(|op| op.name == name)
}

fn parse(args: &[String]) -> Result<Command, Misuse> {
    let op_input = |op: &'static OpInfo, rest: &[String]| {
        args::input(&(op.input_schema)(), rest, mint).map_err(|e| Misuse::Op {
            op,
            what: e.to_string(),
        })
    };
    let json = |text: Option<&String>| match text {
        None => Ok(Json::Obj(Vec::new())),
        Some(text) => argus_json::parse(text.as_bytes()).map_err(|_| Misuse::Usage),
    };
    match args {
        [cmd] if cmd == "ops" => Ok(Command::Ops),
        [cmd] if cmd == "hook" => Ok(Command::Hook),
        [cmd] if cmd == "mcp" => Ok(Command::Mcp),
        [cmd, name] if cmd == "help" => op_named(name)
            .map(|op| Command::Help { op })
            .ok_or(Misuse::Usage),
        [cmd, op, rest @ ..] if cmd == "call" && rest.len() <= 1 => Ok(Command::Call {
            op: op.clone(),
            input: json(rest.first())?,
        }),
        [cmd, name, rest @ ..] if cmd == "watch" => {
            let op = op_named(name).ok_or(Misuse::Usage)?;
            // One JSON object, as before arguments: the input itself.
            let input = match rest {
                [text] if text.starts_with('{') => json(Some(text))?,
                _ => op_input(op, rest)?,
            };
            Ok(Command::Watch {
                op: op.name.to_owned(),
                input,
            })
        }
        [name, rest @ ..] if !COMMANDS.contains(&name.as_str()) => {
            let op = op_named(name).ok_or(Misuse::Usage)?;
            Ok(Command::Call {
                op: op.name.to_owned(),
                input: op_input(op, rest)?,
            })
        }
        _ => Err(Misuse::Usage),
    }
}

/// A fresh `request_id` for a command the user did not name one for; retrying with the
/// same effect needs the same id, so pass `--request-id` to choose it. Only a random id
/// will do: a reused one (a pid, a second) would make the host drop a new command as a
/// repeat, so without one the user must name it.
fn mint() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .ok()
        .map(|id| format!("cli-{}", id.trim()))
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
    let command = match parse(&args) {
        Ok(command) => command,
        Err(Misuse::Usage) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Err(Misuse::Op { op, what }) => {
            let name = args::kebab(op.name);
            eprintln!("argus {name}: {what}\nsee: argus help {name}");
            return ExitCode::from(2);
        }
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
        Command::Help { op } => {
            println!("{}\n", op.doc.trim_end());
            match op.name {
                "hook" => println!("argus hook  (the body is read from stdin)"),
                name => println!("{}", args::usage(name, &(op.input_schema)())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_is_shadowed_by_a_command_but_hook_which_is_that_op() {
        let shadowed: Vec<&str> = argus_api::OPS
            .iter()
            .map(|op| op.name)
            .filter(|name| COMMANDS.contains(name))
            .collect();
        assert_eq!(shadowed, ["hook"]);
    }
}
