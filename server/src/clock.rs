//! The server clock that every playback deadline and time sync reply uses.

use std::sync::OnceLock;
use std::time::Instant;

use bounce_common::time::ServerInstant;

static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Microseconds since the first call in this process. Monotonic, so a wall
/// clock step never moves a deadline.
#[must_use]
pub fn now() -> ServerInstant {
    let elapsed = ORIGIN.get_or_init(Instant::now).elapsed();
    ServerInstant(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX))
}
