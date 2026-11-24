//! Stops a run's processes when the host dies without stopping them. Each run lives in
//! its own process group, so Ctrl-C on `argusd`, a SIGTERM, or a crash would otherwise
//! leave the agent running (and billing). std cannot catch a signal and argus has no
//! `unsafe`, so a small `sh` watches instead: it reads a pipe the host holds, and the
//! kernel closes that pipe however the host ends.
//!
//! The watchdog has its own process group, so a signal sent to the host's group does not
//! reach it. The ledger learns of the stop on the next start, which abandons or retries
//! the run as for any interrupted attempt.

use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
use std::time::Duration;

/// `$1` is the run's process group, `$2` the grace in seconds. A line on stdin ("done")
/// means the host saw the run end; end of input means the host is gone.
const WATCH: &str = r#"read -r line && exit 0
p=$1
[ "$p" -gt 1 ] || exit 1
for sig in TERM KILL; do
  for c in $(cat /proc/$p/task/$p/children 2>/dev/null); do
    [ "$c" -gt 1 ] && kill -$sig -- -$c 2>/dev/null
  done
  kill -$sig -- -$p 2>/dev/null
  [ $sig = KILL ] || sleep $2
done
exit 0
"#;

/// Watches one run's process group until released.
pub(crate) struct Reaper {
    watchdog: Child,
}

impl Reaper {
    /// Watches `pgid` (the run's leader, as `runner::may_signal` allows). A watchdog
    /// that fires sends SIGTERM to the group and to the groups of the leader's
    /// children (a PTY program leads its own), then SIGKILL after `grace`.
    pub(crate) fn watch(pgid: u32, grace: Duration) -> std::io::Result<Reaper> {
        let watchdog = crate::proc::command("sh")
            .args(["-c", WATCH, "argus-reaper"])
            .arg(pgid.to_string())
            .arg(grace.as_secs().max(1).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        Ok(Reaper { watchdog })
    }

    /// The run has ended: the watchdog exits without signalling anything.
    pub(crate) fn release(mut self) {
        use std::io::Write;
        if let Some(mut pipe) = self.watchdog.stdin.take() {
            let _ = pipe.write_all(b"done\n");
        }
        let _ = self.watchdog.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// A stand-in for an agent: its own process group, as runs have.
    fn agent() -> Child {
        // Null stdio: one left behind by a failed test must not hold the harness's pipes.
        crate::proc::command("sleep")
            .arg("1000")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_run_is_stopped_when_the_host_is_gone() {
        let mut agent = agent();
        let mut reaper = Reaper::watch(agent.id(), Duration::from_secs(1)).unwrap();
        // The host dying closes its end of the pipe without a line.
        drop(reaper.watchdog.stdin.take());
        // The watchdog exits after SIGTERM, the grace, and SIGKILL.
        assert!(reaper.watchdog.wait().unwrap().success());
        let status = agent.try_wait().unwrap();
        // Stopped by the first signal, which a program can handle to clean up.
        assert_eq!(status.and_then(|s| s.signal()), Some(15), "{status:?}");
    }

    #[test]
    fn a_released_run_is_left_alone() {
        let mut agent = agent();
        let reaper = Reaper::watch(agent.id(), Duration::from_secs(1)).unwrap();
        // Returns once the watchdog has exited.
        reaper.release();
        assert_eq!(agent.try_wait().unwrap(), None);
        // One pid, never a group.
        agent.kill().unwrap();
        agent.wait().unwrap();
    }
}
