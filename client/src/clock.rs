//! Estimates the offset between this device's clock and the server's, so
//! a chunk's server-clock `play_at` can be turned into a local deadline.

use std::collections::VecDeque;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::time::Duration;

use bounce_common::protocol::{TimeSyncReply, TimeSyncRequest};
use bounce_common::time::{ClockOffset, ServerInstant, SinceBoot};

pub struct ClockSync {
    /// Recent round trips. The offset comes from the lowest-RTT samples,
    /// since queueing delay on Wi-Fi only ever adds to RTT.
    samples: VecDeque<Sample>,
    /// Written here, read by the audio thread. A single writer, so an
    /// atomic is enough.
    shared: SharedOffset,
}

struct Sample {
    rtt: Duration,
    offset: ClockOffset,
}

/// Microseconds, or `i64::MIN` before the first estimate.
#[derive(Clone)]
pub struct SharedOffset(Arc<AtomicI64>);

impl SharedOffset {
    #[must_use]
    pub fn get(&self) -> Option<ClockOffset> {
        todo!()
    }

    #[must_use]
    pub fn to_local(&self, at: ServerInstant) -> Option<SinceBoot> {
        todo!()
    }
}

impl ClockSync {
    #[must_use]
    pub fn request(&self, now: SinceBoot) -> TimeSyncRequest {
        TimeSyncRequest { client_sent: now }
    }

    pub fn on_reply(&mut self, reply: TimeSyncReply, received: SinceBoot) {
        todo!()
    }

    #[must_use]
    pub fn jitter(&self) -> Duration {
        todo!()
    }

    #[must_use]
    pub fn shared(&self) -> SharedOffset {
        self.shared.clone()
    }
}
