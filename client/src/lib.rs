//! Thread layout, with no async runtime so the client stays small:
//!
//! - The main thread runs the [`link::Link`] and the [`Device`]. It does
//!   every non-audio thing.
//! - The audio thread owns the [`player::Player`] and its output. It never
//!   touches the network or blocks on a lock the main thread holds. It
//!   reads the clock offset and writes playback counters through atomics,
//!   and sends its state changes on a channel the main thread drains.
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
    ClientMessage, DesiredState, Playback, PlayerState, ReportedState, ServerMessage,
};
use bounce_common::time::SinceBoot;
use bounce_common::update::UpdateState;

use crate::clock::ClockSync;
use crate::player::{Output, OutputSettings, PlayerHandle, PlayerInput};

/// Converges the device on the server's desired state and produces
/// everything the server hears from it. The link feeds it messages and
/// ticks, and sends what it returns.
pub struct Device {
    desired: Option<DesiredState>,
    clock: ClockSync,
    next_sync: SinceBoot,
    player: PlayerHandle,
    /// As last reported.
    player_state: PlayerState,
}

const SYNC_INTERVAL: Duration = Duration::from_secs(1);

impl Device {
    /// Spawns the audio thread, which writes to `output`.
    #[must_use]
    pub fn new(output: Output) -> Self {
        let clock = ClockSync::default();
        let player = PlayerHandle::spawn(clock.shared(), output);
        Self {
            desired: None,
            clock,
            next_sync: SinceBoot(Duration::ZERO),
            player,
            player_state: PlayerState::Idle,
        }
    }

    pub fn on_connect(&mut self) {
        self.clock.reset();
        self.next_sync = SinceBoot(Duration::ZERO);
    }

    pub fn on_message(&mut self, message: ServerMessage, now: SinceBoot) -> Vec<ClientMessage> {
        match message {
            ServerMessage::Desired(desired) => {
                self.converge(&desired);
                self.desired = Some(desired);
                if let Some(state) = self.player.state_change() {
                    self.player_state = state;
                }
                self.reported().into_iter().collect()
            }
            ServerMessage::TimeSync(reply) => {
                self.clock.on_reply(reply, now);
                Vec::new()
            }
            ServerMessage::Audio(chunk) => {
                self.player.send(PlayerInput::Chunk(chunk));
                Vec::new()
            }
            ServerMessage::Command(_)
            | ServerMessage::LogsAck { .. }
            | ServerMessage::Rejected(_) => {
                tracing::debug!(?message, "ignored");
                Vec::new()
            }
        }
    }

    /// Reports the player's state if it changed, and sends at most one
    /// `TimeSync` per `SYNC_INTERVAL`, however often it is called.
    pub fn tick(&mut self, now: SinceBoot) -> Vec<ClientMessage> {
        let mut out = Vec::new();
        if let Some(state) = self.player.state_change() {
            if state != self.player_state {
                self.player_state = state;
                out.extend(self.reported());
            }
        }
        if now >= self.next_sync {
            self.next_sync = SinceBoot(now.0 + SYNC_INTERVAL);
            out.push(ClientMessage::TimeSync(self.clock.request(now)));
        }
        out
    }

    /// Sends the whole desired output every time. The player ignores a
    /// `Start` for the stream it already has, so resending is harmless.
    fn converge(&self, desired: &DesiredState) {
        self.player.send(PlayerInput::Configure(OutputSettings {
            device: desired.output.clone(),
            volume: desired.volume,
            muted: desired.muted,
            latency: desired.output_latency,
        }));
        self.player.send(match desired.playback {
            Playback::Idle => PlayerInput::Stop,
            Playback::Playing { stream, format } => PlayerInput::Start { stream, format },
        });
    }

    fn reported(&self) -> Option<ClientMessage> {
        let desired = self.desired.as_ref()?;
        Some(ClientMessage::Reported(ReportedState {
            applied: desired.revision,
            player: self.player_state.clone(),
            update: UpdateState::UpToDate,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bounce_common::audio::{AudioChunk, AudioFormat, Codec};
    use bounce_common::ids::StreamId;
    use bounce_common::protocol::{Revision, TimeSyncReply};
    use bounce_common::telemetry::Level;
    use bounce_common::time::ServerInstant;
    use bounce_common::zone::Volume;

    use super::*;
    use crate::player::WavOutput;

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
        let mut device = Device::new(Output::Alsa);
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
        let mut device = Device::new(Output::Alsa);
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

    /// Ticks until a report with `want` arrives, collecting the reports.
    fn tick_until(
        device: &mut Device,
        want: &PlayerState,
        seen: &mut Vec<(Revision, PlayerState)>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.last().map(|(_, s)| s) != Some(want) {
            assert!(
                Instant::now() < deadline,
                "never reported {want:?}: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
            for message in device.tick(clock::now()) {
                if let ClientMessage::Reported(r) = message {
                    seen.push((r.applied, r.player));
                }
            }
        }
    }

    #[test]
    fn reports_buffering_then_playing_for_the_desired_stream_then_idle() {
        let dir = std::env::temp_dir().join(format!("bounce-device-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut device = Device::new(Output::Wav(WavOutput {
            dir: dir.clone(),
            wall_origin_us: 0,
        }));
        let now = clock::now();
        let server_now = ServerInstant(u64::try_from(now.0.as_micros()).unwrap());
        device.on_message(
            ServerMessage::TimeSync(TimeSyncReply {
                client_sent: now,
                server_received: server_now,
                server_sent: server_now,
            }),
            now,
        );

        let stream = StreamId(9);
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 2,
            codec: Codec::PcmS16Le,
        };
        let playing = DesiredState {
            playback: Playback::Playing { stream, format },
            ..desired(5)
        };
        device.on_message(ServerMessage::Desired(playing), now);
        for seq in 0..10 {
            device.on_message(
                ServerMessage::Audio(AudioChunk {
                    stream,
                    seq,
                    play_at: ServerInstant(server_now.0 + 50_000 + seq * 10_000),
                    data: vec![0x10; 4 * 480],
                }),
                now,
            );
        }

        let mut seen = Vec::new();
        tick_until(&mut device, &PlayerState::Playing { stream }, &mut seen);
        assert!(
            seen.contains(&(Revision(5), PlayerState::Buffering { stream })),
            "{seen:?}"
        );

        let out = device.on_message(ServerMessage::Desired(desired(6)), clock::now());
        seen.extend(out.into_iter().filter_map(|m| match m {
            ClientMessage::Reported(r) => Some((r.applied, r.player)),
            _ => None,
        }));
        tick_until(&mut device, &PlayerState::Idle, &mut seen);
        assert_eq!(seen.last(), Some(&(Revision(6), PlayerState::Idle)));
        assert!(std::fs::metadata(dir.join("stream-9.wav")).unwrap().len() > 44);
    }
}
