//! Thread layout, with no async runtime so the client stays small:
//!
//! - The main thread runs the [`link::Link`] and the [`Device`]. It does
//!   every non-audio thing.
//! - The audio thread owns the [`player::Player`] and its output. It never
//!   touches the network. It reads the clock offset and writes playback
//!   counters through atomics, and sends state changes on a channel. Its
//!   error logs take the log buffer's lock for one append.
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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bounce_common::protocol::{
    ClientMessage, DesiredState, Playback, PlayerState, ReportedState, ServerMessage,
};
use bounce_common::time::SinceBoot;
use bounce_common::update::UpdateState;

use crate::clock::ClockSync;
use crate::player::{Output, OutputSettings, PlayerHandle, PlayerInput};
use crate::telemetry::LogBuffer;

/// Converges the device on the server's desired state and produces
/// everything the server hears from it. The link feeds it messages and
/// ticks, and sends what it returns.
pub struct Device {
    desired: Option<DesiredState>,
    clock: ClockSync,
    next_sync: SinceBoot,
    player: PlayerHandle,
    player_state: PlayerState,
    logs: Arc<Mutex<LogBuffer>>,
}

const SYNC_INTERVAL: Duration = Duration::from_secs(1);

const MAX_LOG_BATCH: usize = 64;

impl Device {
    #[must_use]
    pub fn new(output: Output, logs: Arc<Mutex<LogBuffer>>) -> Self {
        let clock = ClockSync::default();
        let player = PlayerHandle::spawn(clock.shared(), output);
        Self {
            desired: None,
            clock,
            next_sync: SinceBoot(Duration::ZERO),
            player,
            player_state: PlayerState::Idle,
            logs,
        }
    }

    pub fn on_connect(&mut self) {
        self.clock.reset();
        self.next_sync = SinceBoot(Duration::ZERO);
    }

    pub fn on_message(&mut self, message: ServerMessage, now: SinceBoot) -> Vec<ClientMessage> {
        match message {
            ServerMessage::Desired(desired) => {
                telemetry::lock(&self.logs).set_level(desired.log_level);
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
            ServerMessage::LogsAck { boot, up_to } => {
                telemetry::lock(&self.logs).ack(boot, up_to);
                Vec::new()
            }
            ServerMessage::Command(_) | ServerMessage::Rejected(_) => {
                tracing::debug!(?message, "ignored");
                Vec::new()
            }
        }
    }

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
        let batch = telemetry::lock(&self.logs).batch(MAX_LOG_BATCH);
        out.extend(batch.map(ClientMessage::Logs));
        out
    }

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
    use bounce_common::ids::{BootId, StreamId};
    use bounce_common::protocol::{Revision, TimeSyncReply};
    use bounce_common::telemetry::{Level, LogSeq};
    use bounce_common::time::ServerInstant;
    use bounce_common::zone::Volume;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::player::WavOutput;
    use crate::telemetry::ForwardLayer;

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

    const BOOT: BootId = BootId(77);

    pub(crate) fn logs() -> Arc<Mutex<LogBuffer>> {
        Arc::new(Mutex::new(LogBuffer::new(BOOT, 16)))
    }

    fn at(ms: u64) -> SinceBoot {
        SinceBoot(Duration::from_millis(ms))
    }

    fn log_seqs(out: &[ClientMessage]) -> Option<Vec<u64>> {
        out.iter().find_map(|m| match m {
            ClientMessage::Logs(batch) => {
                assert_eq!(batch.boot, BOOT);
                Some(batch.records.iter().map(|r| r.seq.0).collect())
            }
            _ => None,
        })
    }

    fn log(logs: &Mutex<LogBuffer>, level: Level) {
        let at = at(0);
        telemetry::lock(logs).push(at, level, "t".into(), "m".into(), Vec::new());
    }

    #[test]
    fn tick_sends_unacked_logs_until_an_ack_for_this_boot() {
        let logs = logs();
        let mut device = Device::new(Output::Alsa, logs.clone());
        log(&logs, Level::Info);
        log(&logs, Level::Info);
        assert_eq!(log_seqs(&device.tick(at(0))), Some(vec![1, 2]));
        assert_eq!(log_seqs(&device.tick(at(1))), Some(vec![1, 2]));

        let other_boot = ServerMessage::LogsAck {
            boot: BootId(78),
            up_to: LogSeq(2),
        };
        assert!(device.on_message(other_boot, at(2)).is_empty());
        assert_eq!(log_seqs(&device.tick(at(3))), Some(vec![1, 2]));

        let acked = ServerMessage::LogsAck {
            boot: BOOT,
            up_to: LogSeq(1),
        };
        device.on_message(acked, at(4));
        assert_eq!(log_seqs(&device.tick(at(5))), Some(vec![2]));
        log(&logs, Level::Info);
        assert_eq!(log_seqs(&device.tick(at(6))), Some(vec![2, 3]));
    }

    #[test]
    fn desired_sets_the_level_of_buffered_logs() {
        let logs = logs();
        let mut device = Device::new(Output::Alsa, logs.clone());
        let quiet = DesiredState {
            log_level: Level::Warn,
            ..desired(1)
        };
        device.on_message(ServerMessage::Desired(quiet), at(0));
        log(&logs, Level::Info);
        assert_eq!(log_seqs(&device.tick(at(1))), None);
        log(&logs, Level::Warn);
        assert_eq!(log_seqs(&device.tick(at(2))), Some(vec![1]));
    }

    #[test]
    fn sending_and_acking_logs_logs_nothing_more_to_send() {
        let logs = logs();
        let subscriber = tracing_subscriber::registry().with(ForwardLayer::new(logs.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let mut device = Device::new(Output::Alsa, logs.clone());
            let verbose = DesiredState {
                log_level: Level::Trace,
                ..desired(1)
            };
            device.on_message(ServerMessage::Desired(verbose), at(0));
            tracing::info!("one record");
            let sent = device.tick(at(1));
            let seqs = log_seqs(&sent).expect("the record is sent");
            let up_to = LogSeq(*seqs.last().unwrap());
            device.on_message(ServerMessage::LogsAck { boot: BOOT, up_to }, at(2));
            for ms in [3, 1_003, 2_003] {
                let out = device.tick(at(ms));
                assert_eq!(log_seqs(&out), None, "{out:?}");
            }
        });
    }

    #[test]
    fn desired_is_reported_applied_and_idle() {
        let mut device = Device::new(Output::Alsa, logs());
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
        let mut device = Device::new(Output::Alsa, logs());
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
        let output = Output::Wav(WavOutput {
            dir: dir.clone(),
            wall_origin_us: 0,
        });
        let mut device = Device::new(output, logs());
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
