//! Thread layout, with no async runtime so the client stays small:
//!
//! - The main thread runs the [`link::Link`] and the [`Device`]. It does
//!   every non-audio thing.
//! - The audio thread owns the [`player::Player`] and the ALSA handle. It
//!   never touches the network or blocks on a lock the main thread holds.
//!   It reads the clock offset and writes playback counters through atomics.
//! - The `tracing` layer writes into the shared [`telemetry::LogBuffer`]
//!   from whatever thread logged.

#![expect(
    unused_variables,
    dead_code,
    clippy::needless_pass_by_value,
    reason = "sketch: bodies are todo!()"
)]

pub mod clock;
pub mod config;
pub mod link;
pub mod player;
pub mod telemetry;
pub mod transport;
pub mod updater;

use std::time::Duration;

use bounce_common::protocol::{
    ClientMessage, DesiredState, PlayerState, ReportedState, ServerMessage,
};
use bounce_common::time::SinceBoot;
use bounce_common::update::UpdateState;

use crate::clock::ClockSync;

/// Converges the device on the server's desired state and produces
/// everything the server hears from it. The link feeds it messages and
/// ticks, and sends what it returns.
pub struct Device {
    desired: Option<DesiredState>,
    clock: ClockSync,
    next_sync: SinceBoot,
}

const SYNC_INTERVAL: Duration = Duration::from_secs(1);

impl Default for Device {
    fn default() -> Self {
        Self {
            desired: None,
            clock: ClockSync::default(),
            next_sync: SinceBoot(Duration::ZERO),
        }
    }
}

impl Device {
    pub fn on_connect(&mut self) {
        self.clock.reset();
        self.next_sync = SinceBoot(Duration::ZERO);
    }

    pub fn on_message(&mut self, message: ServerMessage, now: SinceBoot) -> Vec<ClientMessage> {
        match message {
            ServerMessage::Desired(desired) => {
                let applied = desired.revision;
                self.desired = Some(desired);
                vec![ClientMessage::Reported(ReportedState {
                    applied,
                    player: PlayerState::Idle,
                    update: UpdateState::UpToDate,
                })]
            }
            ServerMessage::TimeSync(reply) => {
                self.clock.on_reply(reply, now);
                Vec::new()
            }
            ServerMessage::Audio(_)
            | ServerMessage::Command(_)
            | ServerMessage::LogsAck { .. }
            | ServerMessage::Rejected(_) => {
                tracing::debug!(?message, "ignored");
                Vec::new()
            }
        }
    }

    /// Sends at most one `TimeSync` per `SYNC_INTERVAL`, however often it
    /// is called.
    pub fn tick(&mut self, now: SinceBoot) -> Vec<ClientMessage> {
        if now < self.next_sync {
            return Vec::new();
        }
        self.next_sync = SinceBoot(now.0 + SYNC_INTERVAL);
        vec![ClientMessage::TimeSync(self.clock.request(now))]
    }
}

#[cfg(test)]
mod tests {
    use bounce_common::protocol::{Playback, Revision};
    use bounce_common::telemetry::Level;
    use bounce_common::zone::Volume;

    use super::*;

    pub(crate) fn desired(revision: u64) -> DesiredState {
        DesiredState {
            revision: Revision(revision),
            name: "Kitchen".into(),
            output: "wav".into(),
            volume: Volume::new(40),
            muted: false,
            output_latency: Duration::ZERO,
            log_level: Level::Info,
            playback: Playback::Idle,
            release: None,
        }
    }

    fn at(ms: u64) -> SinceBoot {
        SinceBoot(Duration::from_millis(ms))
    }

    #[test]
    fn desired_is_reported_applied_and_idle() {
        let mut device = Device::default();
        let out = device.on_message(ServerMessage::Desired(desired(3)), at(0));
        assert!(
            matches!(
                out.as_slice(),
                [ClientMessage::Reported(ReportedState {
                    applied: Revision(3),
                    player: PlayerState::Idle,
                    update: UpdateState::UpToDate,
                })]
            ),
            "{out:?}"
        );
        assert_eq!(device.desired, Some(desired(3)));
    }

    #[test]
    fn tick_sends_at_most_one_time_sync_per_second() {
        let mut device = Device::default();
        let syncs = |out: Vec<ClientMessage>| {
            out.iter()
                .filter(|m| matches!(m, ClientMessage::TimeSync(_)))
                .count()
        };
        assert_eq!(syncs(device.tick(at(0))), 1);
        assert_eq!(syncs(device.tick(at(1))), 0);
        assert_eq!(syncs(device.tick(at(999))), 0);
        assert_eq!(syncs(device.tick(at(1000))), 1);
        assert_eq!(syncs(device.tick(at(1500))), 0);

        device.on_connect();
        assert_eq!(syncs(device.tick(at(1600))), 1);
    }
}
