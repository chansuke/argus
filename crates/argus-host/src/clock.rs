//! The host's only reads of the clock. Everything else asks here, so a test or a
//! replay can tell which code depends on time.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use argus_core::ids::UnixMillis;

/// Wall-clock time, for domain timestamps and policy decisions.
#[expect(clippy::disallowed_methods, reason = "this is the host's wall clock")]
pub(crate) fn now() -> UnixMillis {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    UnixMillis(since.map_or(0, |d| d.as_millis() as i64))
}

/// Monotonic time, for timeouts and elapsed durations.
#[expect(
    clippy::disallowed_methods,
    reason = "this is the host's monotonic clock"
)]
pub(crate) fn monotonic() -> Instant {
    Instant::now()
}
