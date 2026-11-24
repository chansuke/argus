//! Per-run socket tokens. A process that talks to argus (its plan exports `ARGUS_RUN`)
//! gets a fresh random token in `ARGUS_TOKEN`; the socket transport turns
//! `Hello { run, token }` into `Caller::Agent` only while that run holds that token.
//! Tokens live in memory only: a restarted host abandons the runs that held them.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use argus_core::ids::RunId;

#[derive(Clone, Default)]
pub(crate) struct RunTokens {
    live: Arc<Mutex<BTreeMap<RunId, String>>>,
}

/// Holds a run's token; dropping it revokes the token.
pub(crate) struct Issued {
    tokens: RunTokens,
    run: RunId,
    pub token: String,
}

impl Drop for Issued {
    fn drop(&mut self) {
        let mut live = self.tokens.live.lock().unwrap();
        if live.get(&self.run) == Some(&self.token) {
            live.remove(&self.run);
        }
    }
}

impl RunTokens {
    /// A new token for `run`, replacing any earlier one (a retried attempt).
    pub(crate) fn issue(&self, run: RunId) -> std::io::Result<Issued> {
        let token = random_hex()?;
        self.live.lock().unwrap().insert(run, token.clone());
        Ok(Issued {
            tokens: self.clone(),
            run,
            token,
        })
    }

    /// Whether `token` is the one `run` holds now. Compared in constant time.
    pub(crate) fn check(&self, run: RunId, token: &str) -> bool {
        let live = self.live.lock().unwrap();
        live.get(&run)
            .is_some_and(|t| same(t.as_bytes(), token.as_bytes()))
    }
}

/// Equal-length inputs take the same time whatever they contain.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 256 random bits from the kernel, as 64 hex digits.
pub(crate) fn random_hex() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_core::ids::Id128;

    #[test]
    fn a_token_is_valid_for_its_run_until_it_is_dropped() {
        let tokens = RunTokens::default();
        let issued = tokens.issue(RunId(Id128(1))).unwrap();
        assert_eq!(issued.token.len(), 64);
        assert!(tokens.check(RunId(Id128(1)), &issued.token));
        assert!(!tokens.check(RunId(Id128(2)), &issued.token));
        assert!(!tokens.check(RunId(Id128(1)), &issued.token[1..]));
        let token = issued.token.clone();
        drop(issued);
        assert!(!tokens.check(RunId(Id128(1)), &token));
    }

    #[test]
    fn a_retried_attempts_token_replaces_the_earlier_one() {
        let tokens = RunTokens::default();
        let first = tokens.issue(RunId(Id128(1))).unwrap();
        let second = tokens.issue(RunId(Id128(1))).unwrap();
        assert_ne!(first.token, second.token);
        assert!(!tokens.check(RunId(Id128(1)), &first.token));
        // The earlier attempt ending does not revoke the later one.
        drop(first);
        assert!(tokens.check(RunId(Id128(1)), &second.token));
    }
}
