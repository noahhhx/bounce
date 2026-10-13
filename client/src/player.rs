//! The audio thread: hold chunks until their deadline, then write them to
//! the output. This is where sync is won or lost.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bounce_common::audio::{AudioChunk, AudioFormat};
use bounce_common::ids::StreamId;
use bounce_common::protocol::PlayerState;
use bounce_common::time::SinceBoot;
use bounce_common::zone::Volume;

use crate::clock::{self, SharedOffset};

/// The network thread's handle. Chunks are bounded, so if the audio thread
/// stalls the network thread drops chunks instead of growing memory on a
/// 512 MB device. The channel itself is unbounded and the bound is
/// `queued`, so `Start`, `Stop` and `Configure` are never dropped and stay
/// ordered with the chunks around them.
pub struct PlayerHandle {
    tx: Sender<PlayerInput>,
    queued: Arc<AtomicUsize>,
    states: Receiver<PlayerState>,
    counters: Arc<PlaybackCounters>,
}

/// Over two seconds of 10 ms chunks, more than a late joiner's replay.
const MAX_QUEUED: usize = 256;

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
    /// Periods of silence written while playing because no chunk was due.
    pub underruns: AtomicU64,
    pub late_chunks: AtomicU64,
    /// Chunks the network thread dropped because the audio thread was
    /// behind.
    pub dropped_chunks: AtomicU64,
    pub buffered_us: AtomicU64,
}

pub struct Player {
    state: State,
    /// Chunks for the current stream in sequence order. They arrive in
    /// order over the session, so this is a queue, not a reorder buffer.
    buffer: VecDeque<AudioChunk>,
    output: Output,
    settings: OutputSettings,
    clock: SharedOffset,
    counters: Arc<PlaybackCounters>,
    changes: Sender<PlayerState>,
}

/// [`PlayerState`] plus what only some states have: the open stream once
/// one is buffering, and where the output is up to once it plays.
enum State {
    Idle,
    Buffering(Track),
    Playing {
        track: Track,
        /// The local time the next frame written reaches the listener.
        cursor: SinceBoot,
    },
    OutputUnavailable {
        device: String,
    },
}

struct Track {
    stream: StreamId,
    format: AudioFormat,
    wav: WavStream,
}

pub enum Output {
    /// Not implemented. Starting a stream reports `OutputUnavailable`.
    Alsa,
    Wav(WavOutput),
}

/// Writes in real time, paced by the same deadlines, so underrun padding
/// and volume land in the file exactly as they would at the DAC.
///
/// Each stream gets `<dir>/stream-<id>.wav`, 16-bit PCM behind the canonical
/// 44-byte header (sizes rewritten after every write, so the file is
/// readable while the client runs), and `<dir>/stream-<id>.timing`, one
/// `<first frame> <play_at us>` line per chunk written. Starting a stream
/// whose files exist appends to them.
pub struct WavOutput {
    pub dir: PathBuf,
}

struct WavStream {
    wav: File,
    timing: File,
    frames: u64,
    block_align: u16,
}

/// How far a chunk's deadline may sit from where the output is up to and
/// still be written there. Without resampling, this is also the drift the
/// player absorbs before it drops a chunk or pads a gap. The WAV sink is
/// paced by the deadlines themselves and loopback has no clock drift, so
/// resampling is left for a real DAC.
const TOLERANCE: Duration = Duration::from_millis(1);
/// Silence written at a time when the buffer is dry.
const PERIOD: Duration = Duration::from_millis(10);
/// The output buffer the player models. Waking up to this late, it writes
/// everything that fell due, as a DAC playing from its buffer would have:
/// WAV writes on a busy ext4 disk were measured blocking 26 to 180 ms.
/// Further behind, as after the process was stopped, it skips to now in
/// one silent write.
const MAX_BEHIND: Duration = Duration::from_millis(500);
/// How often to look for a clock estimate while chunks wait for one.
const NO_CLOCK_POLL: Duration = Duration::from_millis(10);
/// Ten seconds of 10 ms chunks: only reached while no clock estimate lets
/// any of them be judged late.
const MAX_BUFFERED: usize = 1_000;

impl PlayerHandle {
    /// # Panics
    ///
    /// If the OS cannot start a thread.
    #[must_use]
    pub fn spawn(clock: SharedOffset, output: Output) -> Self {
        let (tx, rx) = mpsc::channel();
        let (changes, states) = mpsc::channel();
        let counters = Arc::<PlaybackCounters>::default();
        let queued = Arc::<AtomicUsize>::default();
        let player = Player::new(clock, output, Arc::clone(&counters), changes);
        let taken = Arc::clone(&queued);
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || player.run(&rx, &taken))
            .expect("spawning the audio thread");
        Self {
            tx,
            queued,
            states,
            counters,
        }
    }

    pub fn send(&self, input: PlayerInput) {
        if matches!(input, PlayerInput::Chunk(_)) {
            if self.queued.load(Ordering::Relaxed) >= MAX_QUEUED {
                self.counters.dropped_chunks.fetch_add(1, Ordering::Relaxed);
                return;
            }
            self.queued.fetch_add(1, Ordering::Relaxed);
        }
        if self.tx.send(input).is_err() {
            tracing::error!("audio thread is gone");
        }
    }

    /// The newest state the player entered since the last call, if any.
    #[must_use]
    pub fn state_change(&self) -> Option<PlayerState> {
        self.states.try_iter().last()
    }

    #[must_use]
    pub fn counters(&self) -> &PlaybackCounters {
        &self.counters
    }
}

impl Player {
    fn new(
        clock: SharedOffset,
        output: Output,
        counters: Arc<PlaybackCounters>,
        changes: Sender<PlayerState>,
    ) -> Self {
        Self {
            state: State::Idle,
            buffer: VecDeque::new(),
            output,
            settings: OutputSettings {
                device: String::new(),
                volume: Volume::new(0),
                muted: false,
                latency: Duration::ZERO,
            },
            clock,
            counters,
            changes,
        }
    }

    fn run(mut self, inputs: &Receiver<PlayerInput>, queued: &AtomicUsize) {
        loop {
            let input = match self.step(clock::now()) {
                Some(wake) => inputs.recv_timeout(wake.0.saturating_sub(clock::now().0)),
                None => inputs.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match input {
                Ok(input) => {
                    if matches!(input, PlayerInput::Chunk(_)) {
                        queued.fetch_sub(1, Ordering::Relaxed);
                    }
                    self.apply(input);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn apply(&mut self, input: PlayerInput) {
        match input {
            PlayerInput::Chunk(chunk) => {
                if self.state.stream() != Some(chunk.stream) {
                    return;
                }
                if self.buffer.len() == MAX_BUFFERED {
                    self.buffer.pop_front();
                    self.counters.late_chunks.fetch_add(1, Ordering::Relaxed);
                }
                self.buffer.push_back(chunk);
            }
            PlayerInput::Start { stream, format } => {
                if self.state.stream() == Some(stream) {
                    return;
                }
                self.buffer.clear();
                let state = match &self.output {
                    Output::Alsa => self.unavailable(),
                    Output::Wav(output) => match output.open(stream, format) {
                        Ok(wav) => State::Buffering(Track {
                            stream,
                            format,
                            wav,
                        }),
                        Err(error) => {
                            tracing::error!(%error, dir = %output.dir.display(), "cannot open WAV");
                            self.unavailable()
                        }
                    },
                };
                self.enter(state);
            }
            PlayerInput::Stop => {
                self.buffer.clear();
                self.enter(State::Idle);
            }
            PlayerInput::Configure(settings) => self.settings = settings,
        }
    }

    /// Writes everything due by `now`, and returns when something next
    /// falls due, or `None` when only an input can change anything.
    fn step(&mut self, now: SinceBoot) -> Option<SinceBoot> {
        let before = self.state.reported();
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (state, wake) = match state {
            State::Buffering(track) => self.buffering(track, now),
            State::Playing { track, cursor } => self.playing(track, cursor, now),
            other => (other, None),
        };
        self.state = state;
        self.announce(before);
        let buffered: u64 = self.buffer.iter().map(|c| self.chunk_micros(c)).sum();
        self.counters.buffered_us.store(buffered, Ordering::Relaxed);
        wake
    }

    fn buffering(&mut self, track: Track, now: SinceBoot) -> (State, Option<SinceBoot>) {
        loop {
            let Some(chunk) = self.buffer.front() else {
                return (State::Buffering(track), None);
            };
            let Some(deadline) = self.deadline(chunk) else {
                return (State::Buffering(track), Some(after(now, NO_CLOCK_POLL)));
            };
            if after(deadline, TOLERANCE) < now {
                self.drop_late();
            } else if deadline > now {
                return (State::Buffering(track), Some(deadline));
            } else {
                return self.playing(track, deadline, now);
            }
        }
    }

    fn playing(
        &mut self,
        mut track: Track,
        mut cursor: SinceBoot,
        now: SinceBoot,
    ) -> (State, Option<SinceBoot>) {
        match self.write_due(&mut track, &mut cursor, now) {
            Ok(()) => (State::Playing { track, cursor }, Some(cursor)),
            Err(error) => {
                tracing::error!(%error, "cannot write WAV");
                (self.unavailable(), None)
            }
        }
    }

    fn write_due(
        &mut self,
        track: &mut Track,
        cursor: &mut SinceBoot,
        now: SinceBoot,
    ) -> io::Result<()> {
        let rate = track.format.sample_rate;
        let gain = self.gain();
        if now > after(*cursor, MAX_BEHIND) {
            track
                .wav
                .silence(frames_in(now.0.saturating_sub(cursor.0), rate))?;
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            *cursor = now;
        }
        while *cursor <= now {
            let deadline = self.buffer.front().and_then(|c| self.deadline(c));
            match deadline {
                Some(d) if after(d, TOLERANCE) < *cursor => self.drop_late(),
                Some(d) if d <= after(*cursor, TOLERANCE) => {
                    let chunk = self.buffer.pop_front().expect("deadline came from a chunk");
                    let frames = track.wav.chunk(&chunk, gain)?;
                    *cursor = after(*cursor, duration_of(frames, rate));
                }
                gap => {
                    let silence = gap.map_or(PERIOD, |d| d.0.saturating_sub(cursor.0).min(PERIOD));
                    let frames = track.wav.silence(frames_in(silence, rate))?;
                    self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                    *cursor = after(*cursor, duration_of(frames, rate));
                }
            }
        }
        Ok(())
    }

    /// When the chunk's first frame must reach the output: its server
    /// deadline on the local clock, early by the device's output latency.
    /// `None` until the clock has an estimate.
    fn deadline(&self, chunk: &AudioChunk) -> Option<SinceBoot> {
        let at = self.clock.to_local(chunk.play_at)?;
        Some(SinceBoot(at.0.saturating_sub(self.settings.latency)))
    }

    fn drop_late(&mut self) {
        self.buffer.pop_front();
        self.counters.late_chunks.fetch_add(1, Ordering::Relaxed);
    }

    fn gain(&self) -> i32 {
        if self.settings.muted {
            0
        } else {
            i32::from(self.settings.volume.percent())
        }
    }

    fn chunk_micros(&self, chunk: &AudioChunk) -> u64 {
        let Some(track) = self.state.track() else {
            return 0;
        };
        let frames = chunk.data.len() as u64 / u64::from(track.wav.block_align);
        frames * 1_000_000 / u64::from(track.format.sample_rate)
    }

    fn unavailable(&self) -> State {
        State::OutputUnavailable {
            device: self.settings.device.clone(),
        }
    }

    fn enter(&mut self, state: State) {
        let before = self.state.reported();
        self.state = state;
        self.announce(before);
    }

    fn announce(&self, before: PlayerState) {
        let now = self.state.reported();
        if now != before {
            let _ = self.changes.send(now);
        }
    }
}

impl State {
    fn track(&self) -> Option<&Track> {
        match self {
            Self::Buffering(track) | Self::Playing { track, .. } => Some(track),
            Self::Idle | Self::OutputUnavailable { .. } => None,
        }
    }

    fn stream(&self) -> Option<StreamId> {
        self.track().map(|t| t.stream)
    }

    fn reported(&self) -> PlayerState {
        match self {
            Self::Idle => PlayerState::Idle,
            Self::Buffering(track) => PlayerState::Buffering {
                stream: track.stream,
            },
            Self::Playing { track, .. } => PlayerState::Playing {
                stream: track.stream,
            },
            Self::OutputUnavailable { device } => PlayerState::OutputUnavailable {
                device: device.clone(),
            },
        }
    }
}

const HEADER_LEN: u64 = 44;

impl WavOutput {
    fn open(&self, stream: StreamId, format: AudioFormat) -> io::Result<WavStream> {
        std::fs::create_dir_all(&self.dir)?;
        let base = self.dir.join(format!("stream-{}", stream.0));
        let wav = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.with_extension("wav"))?;
        let timing = OpenOptions::new()
            .append(true)
            .create(true)
            .open(base.with_extension("timing"))?;
        let block_align = 2 * u16::from(format.channels);
        let data_len = wav.metadata()?.len().saturating_sub(HEADER_LEN);
        let stream = WavStream {
            wav,
            timing,
            frames: data_len / u64::from(block_align),
            block_align,
        };
        stream.wav.set_len(HEADER_LEN + stream.data_len())?;
        stream.write_header(format)?;
        Ok(stream)
    }
}

impl WavStream {
    /// Returns the frames written.
    fn chunk(&mut self, chunk: &AudioChunk, gain: i32) -> io::Result<u64> {
        let first = self.frames;
        let pcm: Vec<u8> = chunk
            .data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&s| scale(i16::from_le_bytes(s), gain).to_le_bytes())
            .collect();
        let frames = self.append(&pcm)?;
        writeln!(self.timing, "{first} {}", chunk.play_at.0)?;
        Ok(frames)
    }

    fn silence(&mut self, frames: u64) -> io::Result<u64> {
        let len = usize::try_from(frames * u64::from(self.block_align)).expect("fits in memory");
        self.append(&vec![0; len])
    }

    fn append(&mut self, pcm: &[u8]) -> io::Result<u64> {
        self.wav.write_all_at(pcm, HEADER_LEN + self.data_len())?;
        let frames = pcm.len() as u64 / u64::from(self.block_align);
        self.frames += frames;
        let data_len = size_field(self.data_len());
        self.wav.write_all_at(&(data_len + 36).to_le_bytes(), 4)?;
        self.wav.write_all_at(&data_len.to_le_bytes(), 40)?;
        Ok(frames)
    }

    fn data_len(&self) -> u64 {
        self.frames * u64::from(self.block_align)
    }

    fn write_header(&self, format: AudioFormat) -> io::Result<()> {
        let data_len = size_field(self.data_len());
        let byte_rate = format.sample_rate * u32::from(self.block_align);
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(data_len + 36).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&u16::from(format.channels).to_le_bytes());
        header.extend_from_slice(&format.sample_rate.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&self.block_align.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&data_len.to_le_bytes());
        self.wav.write_all_at(&header, 0)
    }
}

/// A WAV past 4 GiB of data, about six hours of 48 kHz stereo, keeps
/// playing with its size fields pinned at the maximum.
fn size_field(data_len: u64) -> u32 {
    u32::try_from(data_len)
        .unwrap_or(u32::MAX - 36)
        .min(u32::MAX - 36)
}

/// Linear amplitude: `gain` is a volume percent.
fn scale(sample: i16, gain: i32) -> i16 {
    i16::try_from(i32::from(sample) * gain / 100).expect("gain is at most 100")
}

fn after(at: SinceBoot, by: Duration) -> SinceBoot {
    SinceBoot(at.0 + by)
}

fn frames_in(duration: Duration, rate: u32) -> u64 {
    u64::try_from(duration.as_nanos() * u128::from(rate) / 1_000_000_000).unwrap_or(u64::MAX)
}

fn duration_of(frames: u64, rate: u32) -> Duration {
    Duration::from_nanos(frames * 1_000_000_000 / u64::from(rate))
}

#[cfg(test)]
mod tests {
    use bounce_common::audio::Codec;
    use bounce_common::time::{ClockOffset, ServerInstant};

    use super::*;

    const FORMAT: AudioFormat = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
        codec: Codec::PcmS16Le,
    };
    const STREAM: StreamId = StreamId(7);
    /// 10 ms of stereo at 48 kHz.
    const FRAMES: usize = 480;

    struct Rig {
        player: Player,
        changes: Receiver<PlayerState>,
        dir: PathBuf,
    }

    /// Local time equals server time, so `play_at` is the deadline.
    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("bounce-player-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let clock = SharedOffset::default();
        clock.set(Some(ClockOffset(0)));
        let (changes_tx, changes) = mpsc::channel();
        let mut player = Player::new(
            clock,
            Output::Wav(WavOutput { dir: dir.clone() }),
            Arc::default(),
            changes_tx,
        );
        player.apply(PlayerInput::Configure(settings(100, false)));
        player.apply(PlayerInput::Start {
            stream: STREAM,
            format: FORMAT,
        });
        Rig {
            player,
            changes,
            dir,
        }
    }

    fn settings(volume: u8, muted: bool) -> OutputSettings {
        OutputSettings {
            device: "wav".into(),
            volume: Volume::new(volume),
            muted,
            latency: Duration::ZERO,
        }
    }

    fn chunk(seq: u64, play_at_ms: u64, sample: i16) -> PlayerInput {
        PlayerInput::Chunk(AudioChunk {
            stream: STREAM,
            seq,
            play_at: ServerInstant(play_at_ms * 1_000),
            data: [sample; 2 * FRAMES]
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect(),
        })
    }

    fn ms(ms: u64) -> SinceBoot {
        SinceBoot(Duration::from_millis(ms))
    }

    impl Rig {
        fn wav(&self) -> Vec<u8> {
            std::fs::read(self.dir.join("stream-7.wav")).unwrap()
        }

        /// Samples after the header.
        fn samples(&self) -> Vec<i16> {
            self.wav()[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&s| i16::from_le_bytes(s))
                .collect()
        }

        fn timing(&self) -> String {
            std::fs::read_to_string(self.dir.join("stream-7.timing")).unwrap()
        }

        fn states(&self) -> Vec<PlayerState> {
            self.changes.try_iter().collect()
        }

        fn counter(&self, pick: fn(&PlaybackCounters) -> &AtomicU64) -> u64 {
            pick(&self.player.counters).load(Ordering::Relaxed)
        }
    }

    #[test]
    fn a_chunk_is_not_written_before_its_deadline() {
        let mut rig = rig("early");
        rig.player.apply(chunk(0, 100, 1_000));

        assert_eq!(rig.player.step(ms(99)), Some(ms(100)));
        assert_eq!(rig.samples().len(), 0);
        assert_eq!(rig.states(), [PlayerState::Buffering { stream: STREAM }]);

        assert_eq!(rig.player.step(ms(100)), Some(ms(110)));
        assert_eq!(rig.samples(), [1_000; 2 * FRAMES]);
        assert_eq!(rig.timing(), "0 100000\n");
        assert_eq!(rig.states(), [PlayerState::Playing { stream: STREAM }]);
    }

    #[test]
    fn without_a_clock_estimate_chunks_wait() {
        let mut rig = rig("no-clock");
        rig.player.clock.set(None);
        rig.player.apply(chunk(0, 100, 1_000));
        assert_eq!(rig.player.step(ms(500)), Some(ms(510)));
        assert_eq!(rig.samples().len(), 0);
        assert_eq!(rig.counter(|c| &c.late_chunks), 0);
    }

    #[test]
    fn a_chunk_past_its_deadline_is_dropped_and_counted_late() {
        let mut rig = rig("late");
        rig.player.apply(chunk(0, 100, 1_000));
        rig.player.apply(chunk(1, 110, 2_000));

        assert_eq!(rig.player.step(ms(105)), Some(ms(110)));
        assert_eq!(rig.counter(|c| &c.late_chunks), 1);
        rig.player.step(ms(110));
        assert_eq!(rig.samples(), [2_000; 2 * FRAMES]);
        assert_eq!(rig.timing(), "0 110000\n");
    }

    #[test]
    fn waking_late_within_the_buffer_writes_everything_that_fell_due() {
        let mut rig = rig("short-stall");
        for seq in 0..31 {
            rig.player.apply(chunk(seq, 100 + seq * 10, 1_000));
        }
        rig.player.step(ms(100));
        assert_eq!(rig.player.step(ms(400)), Some(ms(410)));

        assert_eq!(rig.samples(), vec![1_000; 31 * 2 * FRAMES]);
        assert_eq!(rig.counter(|c| &c.underruns), 0);
        assert_eq!(rig.counter(|c| &c.late_chunks), 0);
    }

    #[test]
    fn waking_later_than_the_buffer_skips_to_now_in_silence() {
        let mut rig = rig("long-stall");
        for seq in 0..=200 {
            rig.player.apply(chunk(seq, 100 + seq * 10, 1_000));
        }
        rig.player.step(ms(100));
        assert_eq!(rig.player.step(ms(2_000)), Some(ms(2_010)));

        let timing = rig.timing();
        assert_eq!(timing.lines().last(), Some("91200 2000000"));
        assert_eq!(timing.lines().count(), 2);
        assert_eq!(rig.counter(|c| &c.late_chunks), 189);
        assert_eq!(rig.counter(|c| &c.underruns), 1);
    }

    #[test]
    fn a_dry_buffer_writes_silence_and_counts_an_underrun() {
        let mut rig = rig("dry");
        rig.player.apply(chunk(0, 100, 1_000));
        rig.player.step(ms(100));
        assert_eq!(rig.player.step(ms(110)), Some(ms(120)));

        let samples = rig.samples();
        assert_eq!(samples.len(), 2 * 2 * FRAMES);
        assert!(samples[2 * FRAMES..].iter().all(|&s| s == 0));
        assert_eq!(rig.counter(|c| &c.underruns), 1);
        assert_eq!(
            rig.states().last(),
            Some(&PlayerState::Playing { stream: STREAM })
        );
    }

    #[test]
    fn a_gap_is_padded_and_the_next_chunk_lands_at_its_deadline() {
        let mut rig = rig("gap");
        rig.player.apply(chunk(0, 100, 1_000));
        rig.player.apply(chunk(1, 130, 2_000));
        for at in [100, 110, 120, 130] {
            rig.player.step(ms(at));
        }

        assert_eq!(rig.timing(), "0 100000\n1440 130000\n");
        let samples = rig.samples();
        assert_eq!(samples.len(), 2 * 4 * FRAMES);
        assert!(samples[2 * FRAMES..2 * 3 * FRAMES].iter().all(|&s| s == 0));
        assert!(samples[2 * 3 * FRAMES..].iter().all(|&s| s == 2_000));
        assert_eq!(rig.counter(|c| &c.underruns), 2);
    }

    #[test]
    fn output_latency_brings_the_deadline_forward() {
        let mut rig = rig("latency");
        rig.player.apply(PlayerInput::Configure(OutputSettings {
            latency: Duration::from_millis(30),
            ..settings(100, false)
        }));
        rig.player.apply(chunk(0, 100, 1_000));
        assert_eq!(rig.player.step(ms(60)), Some(ms(70)));
        rig.player.step(ms(70));
        assert_eq!(rig.samples().len(), 2 * FRAMES);
    }

    #[test]
    fn volume_scales_linearly_and_zero_or_muted_writes_zeros() {
        let mut rig = rig("volume");
        rig.player
            .apply(PlayerInput::Configure(settings(50, false)));
        rig.player.apply(chunk(0, 100, -20_000));
        rig.player.step(ms(100));
        rig.player.apply(PlayerInput::Configure(settings(0, false)));
        rig.player.apply(chunk(1, 110, i16::MAX));
        rig.player.step(ms(110));
        rig.player
            .apply(PlayerInput::Configure(settings(100, true)));
        rig.player.apply(chunk(2, 120, i16::MIN));
        rig.player.step(ms(120));

        let samples = rig.samples();
        assert!(samples[..2 * FRAMES].iter().all(|&s| s == -10_000));
        assert!(samples[2 * FRAMES..].iter().all(|&s| s == 0));
        assert_eq!(samples.len(), 3 * 2 * FRAMES);
    }

    #[test]
    fn the_wav_header_matches_the_data_after_every_write() {
        let mut rig = rig("header");
        for (seq, at) in [(0_usize, 100), (1, 110)] {
            rig.player.apply(chunk(seq as u64, at, 1));
            rig.player.step(ms(at));
            let wav = rig.wav();
            let u32_at = |i: usize| u32::from_le_bytes(wav[i..i + 4].try_into().unwrap());
            let u16_at = |i: usize| u16::from_le_bytes(wav[i..i + 2].try_into().unwrap());
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(u32_at(4) as usize, wav.len() - 8);
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!((u32_at(16), u16_at(20), u16_at(22)), (16, 1, 2));
            assert_eq!((u32_at(24), u32_at(28)), (48_000, 48_000 * 4));
            assert_eq!((u16_at(32), u16_at(34)), (4, 16));
            assert_eq!(&wav[36..40], b"data");
            assert_eq!(u32_at(40) as usize, wav.len() - 44);
            assert_eq!(wav.len(), 44 + (seq + 1) * 4 * FRAMES);
        }
    }

    #[test]
    fn restarting_a_stream_appends_to_its_files() {
        let mut rig = rig("restart");
        rig.player.apply(chunk(0, 100, 1));
        rig.player.step(ms(100));
        rig.player.apply(PlayerInput::Stop);
        rig.player.step(ms(110));
        rig.player.apply(PlayerInput::Start {
            stream: STREAM,
            format: FORMAT,
        });
        rig.player.apply(chunk(50, 600, 2));
        rig.player.step(ms(600));

        assert_eq!(rig.timing(), "0 100000\n480 600000\n");
        assert_eq!(rig.samples().len(), 2 * 2 * FRAMES);
        assert_eq!(
            rig.states(),
            [
                PlayerState::Buffering { stream: STREAM },
                PlayerState::Playing { stream: STREAM },
                PlayerState::Idle,
                PlayerState::Buffering { stream: STREAM },
                PlayerState::Playing { stream: STREAM },
            ]
        );
    }

    #[test]
    fn chunks_of_another_stream_are_ignored() {
        let mut rig = rig("other-stream");
        rig.player.apply(PlayerInput::Chunk(AudioChunk {
            stream: StreamId(8),
            seq: 0,
            play_at: ServerInstant(100_000),
            data: vec![1; 4 * FRAMES],
        }));
        assert_eq!(rig.player.step(ms(100)), None);
        assert_eq!(rig.samples().len(), 0);
    }

    #[test]
    fn alsa_reports_its_device_unavailable() {
        let (changes_tx, changes) = mpsc::channel();
        let mut player = Player::new(
            SharedOffset::default(),
            Output::Alsa,
            Arc::default(),
            changes_tx,
        );
        player.apply(PlayerInput::Configure(OutputSettings {
            device: "default".into(),
            ..settings(100, false)
        }));
        player.apply(PlayerInput::Start {
            stream: STREAM,
            format: FORMAT,
        });
        player.apply(PlayerInput::Stop);
        let states: Vec<_> = changes.try_iter().collect();
        assert_eq!(
            states,
            [
                PlayerState::OutputUnavailable {
                    device: "default".into()
                },
                PlayerState::Idle
            ]
        );
    }

    #[test]
    fn a_full_handle_drops_chunks_but_never_control() {
        let (tx, rx) = mpsc::channel();
        let (_, states) = mpsc::channel();
        let handle = PlayerHandle {
            tx,
            queued: Arc::default(),
            states,
            counters: Arc::default(),
        };
        for seq in 0..=MAX_QUEUED as u64 {
            handle.send(chunk(seq, 0, 0));
        }
        handle.send(PlayerInput::Stop);

        let received: Vec<_> = rx.try_iter().collect();
        assert_eq!(received.len(), MAX_QUEUED + 1);
        assert!(matches!(received.last(), Some(PlayerInput::Stop)));
        assert_eq!(handle.counters().dropped_chunks.load(Ordering::Relaxed), 1);
    }
}
