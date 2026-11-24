//! A PTY without FFI: util-linux `script -qfec <command> /dev/null` runs the command on a
//! new pseudo-terminal and relays it through its own stdin and stdout. `-q` drops the
//! start and end banners, `-f` flushes every write (the raw log is live), `-e` returns the
//! child's exit code (128 + n after signal n), and `/dev/null` discards the typescript
//! file, since the host captures stdout itself.

use std::path::{Path, PathBuf};

/// The program and arguments that run `program args...` under a PTY. `script` hands the
/// command to `sh -c`, so every word is quoted, and `exec` lets the program replace that
/// shell (a signal to the session reaches the program itself).
pub(crate) fn wrap(script: &Path, program: &Path, args: &[String]) -> (PathBuf, Vec<String>) {
    let mut command = format!("exec {}", shell_quote(&program.to_string_lossy()));
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    let args = vec!["-qfec".to_owned(), command, "/dev/null".to_owned()];
    (script.to_path_buf(), args)
}

/// One word for `sh -c`: single-quoted, with `'` written as `'\''`.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// util-linux `script` from the dev shell (or the system).
    fn script() -> PathBuf {
        PathBuf::from("script")
    }

    fn run(program: &str, args: &[&str]) -> (Vec<u8>, Option<i32>) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (program, args) = wrap(&script(), Path::new(program), &args);
        let out = crate::proc::command(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.stdout, out.status.code())
    }

    #[test]
    fn arguments_reach_the_program_unchanged() {
        let (out, code) = run(
            "printf",
            &["%s|", "a b", "it's", "$HOME", "-x", "", "\"q\""],
        );
        assert_eq!(String::from_utf8_lossy(&out), "a b|it's|$HOME|-x||\"q\"|");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn the_program_sees_a_terminal() {
        let (out, _) = run("sh", &["-c", "test -t 0 && test -t 1 && printf tty"]);
        assert_eq!(out, b"tty");
    }

    #[test]
    fn the_exit_code_comes_back() {
        assert_eq!(run("sh", &["-c", "exit 7"]).1, Some(7));
        assert_eq!(run("sh", &["-c", "kill -TERM $$"]).1, Some(128 + 15));
    }
}
