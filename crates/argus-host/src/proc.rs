//! The host's only constructor of child processes. The runner starts each process for
//! a Run that core recorded first; git and the PTY wrapper start theirs here too, so a
//! review of "what does argus execute" starts from one place.

use std::ffi::OsStr;
use std::process::Command;

pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}
