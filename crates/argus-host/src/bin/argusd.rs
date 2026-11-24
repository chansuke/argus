//! Headless host (no window): for remote boxes, or to keep agents running with the app
//! closed. Mutually exclusive with the app's embedded host by the store's locks.
//!
//! `argusd [--config <file>]`. The settings file defaults to
//! `$XDG_CONFIG_HOME/argus/config.json`; without one, every setting has its default
//! (`argus_host::host_config`).

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: argusd [--config <file>]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("argusd: {e}");
            ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the binary reads its environment once, to build the HostConfig"
)]
fn run() -> Result<(), String> {
    let defaults = argus_host::Defaults::from_env(|name| {
        std::env::var_os(name).and_then(|v| v.into_string().ok())
    })
    .ok_or("$HOME is not set to an absolute path")?;
    let mut config_path = defaults.config.clone();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                config_path = PathBuf::from(args.next().ok_or(USAGE)?);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => return Err(format!("unknown argument {arg:?}\n{USAGE}")),
        }
    }
    let config = argus_host::read_host_config(&config_path, &defaults)?;
    let (socket, phone) = (config.socket.clone(), config.phone.clone());
    let host = argus_host::Host::start(config).map_err(|e| format!("cannot start: {e}"))?;
    let _socket = argus_host::serve_socket(host.clone(), &socket)
        .map_err(|e| format!("{}: {e}", socket.display()))?;
    let _phone = match &phone {
        Some(access) => Some(
            argus_host::serve_websocket(host.clone(), access)
                .map_err(|e| format!("{}: {e}", access.listen.addr()))?,
        ),
        None => None,
    };
    eprintln!("argusd: serving on {}", socket.display());
    // Runs until the process is stopped. Every write is already on disk, and the next
    // start abandons or retries what was running.
    loop {
        std::thread::park();
    }
}
