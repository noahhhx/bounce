//! Estimates the offset between this device's clock and the server's, so
//! a chunk's server-clock `play_at` can be turned into a local deadline.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bounce_common::protocol::{TimeSyncReply, TimeSyncRequest};
use bounce_common::time::{ClockOffset, ServerInstant, SinceBoot};

static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Time since the first call in this process.
#[must_use]
pub fn now() -> SinceBoot {
    SinceBoot(ORIGIN.get_or_init(Instant::now).elapsed())
}

#[derive(Default)]
pub struct ClockSync {
    /// Recent round trips. The offset comes from the lowest-RTT samples,
    /// since queueing delay on Wi-Fi only ever adds to RTT.
    samples: VecDeque<Sample>,
    /// Written here, read by the audio thread. A single writer, so an
    /// atomic is enough.
    shared: SharedOffset,
}

const WINDOW: usize = 16;

struct Sample {
    rtt: Duration,
    offset: ClockOffset,
}

/// Microseconds, or `i64::MIN` before the first estimate.
#[derive(Clone)]
pub struct SharedOffset(Arc<AtomicI64>);

impl Default for SharedOffset {
    fn default() -> Self {
        Self(Arc::new(AtomicI64::new(i64::MIN)))
    }
}

impl SharedOffset {
    #[must_use]
    pub fn get(&self) -> Option<ClockOffset> {
        match self.0.load(Ordering::Relaxed) {
            i64::MIN => None,
            offset => Some(ClockOffset(offset)),
        }
    }

    /// A server instant from before this process started saturates to
    /// zero, a deadline that has already passed.
    #[must_use]
    pub fn to_local(&self, at: ServerInstant) -> Option<SinceBoot> {
        let ClockOffset(offset) = self.get()?;
        let local = micros_i64(at.0).saturating_sub(offset);
        Some(SinceBoot(Duration::from_micros(
            u64::try_from(local).unwrap_or(0),
        )))
    }

    fn set(&self, offset: Option<ClockOffset>) {
        self.0
            .store(offset.map_or(i64::MIN, |o| o.0), Ordering::Relaxed);
    }
}

impl ClockSync {
    #[must_use]
    pub fn request(&self, now: SinceBoot) -> TimeSyncRequest {
        TimeSyncRequest { client_sent: now }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.shared.set(None);
    }

    pub fn on_reply(&mut self, reply: TimeSyncReply, received: SinceBoot) {
        let client_sent = duration_micros(reply.client_sent.0);
        let client_received = duration_micros(received.0);
        let server_received = micros_i64(reply.server_received.0);
        let server_sent = micros_i64(reply.server_sent.0);

        let rtt = client_received
            .saturating_sub(client_sent)
            .saturating_sub(server_sent.saturating_sub(server_received));
        let Ok(rtt) = u64::try_from(rtt) else {
            tracing::debug!(
                ?reply,
                ?received,
                "time sync reply with negative round trip"
            );
            return;
        };
        let offset = i64::midpoint(
            server_received.saturating_sub(client_sent),
            server_sent.saturating_sub(client_received),
        );

        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            rtt: Duration::from_micros(rtt),
            offset: ClockOffset(offset),
        });
        let best = self.samples.iter().min_by_key(|s| s.rtt);
        self.shared.set(best.map(|s| s.offset));
    }

    /// Max minus min RTT over the window: how much queueing delay varies.
    #[must_use]
    pub fn jitter(&self) -> Duration {
        let rtts = self.samples.iter().map(|s| s.rtt);
        match (rtts.clone().min(), rtts.max()) {
            (Some(min), Some(max)) => max.saturating_sub(min),
            _ => Duration::ZERO,
        }
    }

    #[must_use]
    pub fn shared(&self) -> SharedOffset {
        self.shared.clone()
    }
}

fn duration_micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

fn micros_i64(us: u64) -> i64 {
    i64::try_from(us).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFSET_US: u64 = 7_000_000;

    fn round_trip(clock: &mut ClockSync, sent_us: u64, up_us: u64, down_us: u64) {
        let server_received = sent_us + up_us + OFFSET_US;
        let server_sent = server_received + 100;
        let received = sent_us + up_us + 100 + down_us;
        clock.on_reply(
            TimeSyncReply {
                client_sent: SinceBoot(Duration::from_micros(sent_us)),
                server_received: ServerInstant(server_received),
                server_sent: ServerInstant(server_sent),
            },
            SinceBoot(Duration::from_micros(received)),
        );
    }

    #[test]
    fn offset_comes_from_the_lowest_rtt_sample() {
        let mut clock = ClockSync::default();
        assert_eq!(clock.shared().get(), None);

        round_trip(&mut clock, 1_000_000, 3_000, 1_000);
        round_trip(&mut clock, 2_000_000, 300, 300);
        round_trip(&mut clock, 3_000_000, 10_000, 1_000);

        assert_eq!(clock.shared().get(), Some(ClockOffset(7_000_000)));
        assert_eq!(clock.jitter(), Duration::from_micros(11_000 - 600));
    }

    #[test]
    fn a_lopsided_trip_alone_is_off_by_half_the_asymmetry() {
        let mut clock = ClockSync::default();
        round_trip(&mut clock, 1_000_000, 10_000, 1_000);
        assert_eq!(clock.shared().get(), Some(ClockOffset(7_004_500)));
    }

    #[test]
    fn best_sample_ages_out_of_the_window() {
        let mut clock = ClockSync::default();
        round_trip(&mut clock, 0, 100, 100);
        for i in 1..=WINDOW as u64 {
            round_trip(&mut clock, i * 1_000_000, 4_000, 2_000);
        }
        assert_eq!(clock.shared().get(), Some(ClockOffset(7_001_000)));
    }

    #[test]
    fn to_local_subtracts_the_offset() {
        let mut clock = ClockSync::default();
        let shared = clock.shared();
        assert_eq!(shared.to_local(ServerInstant(OFFSET_US)), None);

        round_trip(&mut clock, 0, 200, 200);
        assert_eq!(
            shared.to_local(ServerInstant(OFFSET_US + 2_500_000)),
            Some(SinceBoot(Duration::from_millis(2_500)))
        );
        assert_eq!(
            shared.to_local(ServerInstant(5)),
            Some(SinceBoot(Duration::ZERO))
        );
    }

    #[test]
    fn reset_forgets_the_estimate() {
        let mut clock = ClockSync::default();
        round_trip(&mut clock, 0, 100, 100);
        clock.reset();
        assert_eq!(clock.shared().get(), None);
        assert_eq!(clock.jitter(), Duration::ZERO);
    }
}
