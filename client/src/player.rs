//! The audio thread: hold chunks until their deadline, then write them to
//! ALSA. This is where sync is won or lost.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

use bounce_common::audio::{AudioChunk, AudioFormat};
use bounce_common::ids::StreamId;
use bounce_common::protocol::PlayerState;
use bounce_common::zone::Volume;

use crate::clock::SharedOffset;

/// The network thread's handle. Bounded, so if the audio thread stalls the
/// network thread drops chunks instead of growing memory on a 512 MB device.
pub struct PlayerHandle {
    tx: SyncSender<PlayerInput>,
    counters: Arc<PlaybackCounters>,
}

pub enum PlayerInput {
    Chunk(AudioChunk),
    Start {
        stream: StreamId,
        format: AudioFormat,
    },
    Stop,
    Configure(OutputSettings),
}

pub struct OutputSettings {
    pub device: String,
    pub volume: Volume,
    pub muted: bool,
    pub latency: Duration,
}

/// Written by the audio thread, read by the metrics collector. Totals since
/// boot, matching [`bounce_common::telemetry::PlaybackMetrics`].
#[derive(Default)]
pub struct PlaybackCounters {
    pub underruns: AtomicU64,
    pub late_chunks: AtomicU64,
    pub buffered_us: AtomicU64,
}

pub struct Player {
    state: PlayerState,
    buffer: JitterBuffer,
    output: Output,
    clock: SharedOffset,
    counters: Arc<PlaybackCounters>,
}

/// Chunks for the current stream in sequence order. Arrives in order over
/// the session, so this is a queue, not a reorder buffer.
pub struct JitterBuffer {
    stream: Option<StreamId>,
    chunks: VecDeque<AudioChunk>,
}

pub enum Output {
    Alsa(AlsaOutput),
    Wav(WavOutput),
}

pub struct AlsaOutput {
    device: String,
}

/// Writes in real time, paced by the same deadlines, so underrun padding
/// and volume land in the file exactly as they would at the DAC.
///
/// Each stream gets `<dir>/stream-<id>.wav`, 16-bit PCM behind the canonical
/// 44-byte header (sizes rewritten after every write, so the file is
/// readable while the client runs), and `<dir>/stream-<id>.timing`, one
/// `<first frame> <play_at us>` line per chunk written.
pub struct WavOutput {
    dir: PathBuf,
}

impl PlayerHandle {
    /// Spawns the audio thread at real-time priority.
    #[must_use]
    pub fn spawn(clock: SharedOffset, output: Output) -> Self {
        todo!()
    }

    pub fn send(&self, input: PlayerInput) {
        todo!()
    }

    #[must_use]
    pub fn counters(&self) -> &PlaybackCounters {
        &self.counters
    }
}

impl Player {
    /// Drops chunks already past their deadline and counts them late.
    /// Pads with silence when the buffer runs dry and counts an underrun.
    /// Corrects small drift by resampling slightly, not by skipping.
    fn run(self) {
        todo!()
    }
}
