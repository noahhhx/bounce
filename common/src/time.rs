use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Microseconds on the server's monotonic clock. Every playback deadline is
/// expressed in this clock, and clients translate it with their
/// [`ClockOffset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServerInstant(pub u64);

/// Time since the client process started. Pi Zeros have no RTC, so wall
/// clock is wrong until NTP syncs. Client timestamps use this instead, and
/// the server converts them to wall time on receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SinceBoot(pub Duration);

/// `server_clock - client_clock` in microseconds, as estimated by the
/// client from [`crate::protocol::TimeSync`] round trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockOffset(pub i64);
