//! The audio thread: hold chunks until their deadline, then write them to
//! the output.

#[cfg(test)]
pub(crate) mod fake;
mod sink;
mod wav;

use std::collections::VecDeque;
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

pub use sink::{Clock, Frames, OpenError, Output, Sink, SinkError};
pub use wav::{WavOutput, WavStream};

pub struct PlayerHandle {
    tx: Sender<PlayerInput>,
    queued: Arc<AtomicUsize>,
    states: Receiver<PlayerState>,
    counters: Arc<PlaybackCounters>,
}

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
    pub underruns: AtomicU64,
    pub late_chunks: AtomicU64,
    pub dropped_chunks: AtomicU64,
    pub buffered_us: AtomicU64,
}

pub struct Player<O: Output> {
    state: State<O::Sink>,
    buffer: VecDeque<AudioChunk>,
    output: O,
    settings: OutputSettings,
    clock: SharedOffset,
    counters: Arc<PlaybackCounters>,
    changes: Sender<PlayerState>,
    /// Reused for gain, so steady playback does not allocate.
    scratch: Vec<i16>,
}

enum State<S> {
    Idle,
    Buffering(Track<S>),
    Playing { track: Track<S>, cursor: SinceBoot },
    OutputUnavailable { device: String },
}

struct Track<S> {
    stream: StreamId,
    format: AudioFormat,
    sink: S,
}

/// No resampling: the WAV sink is paced by the deadlines and loopback has no
/// clock drift, so drift correction waits for a real DAC.
const TOLERANCE: Duration = Duration::from_millis(1);
const PERIOD: Duration = Duration::from_millis(10);
/// WAV writes on a busy ext4 disk were measured blocking 26 to 180 ms.
const MAX_BEHIND: Duration = Duration::from_millis(500);
const NO_CLOCK_POLL: Duration = Duration::from_millis(10);
const MAX_BUFFERED: usize = 1_000;

impl PlayerHandle {
    /// # Panics
    ///
    /// If the OS cannot start a thread.
    #[must_use]
    pub fn spawn<O: Output + Send + 'static>(clock: SharedOffset, output: O) -> Self {
        let (tx, rx) = mpsc::channel();
        let (changes, states) = mpsc::channel();
        let counters = Arc::<PlaybackCounters>::default();
        let queued = Arc::<AtomicUsize>::default();
        let player_counters = Arc::clone(&counters);
        let taken = Arc::clone(&queued);
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || Player::new(clock, output, player_counters, changes).run(&rx, &taken))
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

    #[must_use]
    pub fn state_change(&self) -> Option<PlayerState> {
        self.states.try_iter().last()
    }

    #[must_use]
    pub fn counters(&self) -> &PlaybackCounters {
        &self.counters
    }
}

impl<O: Output> Player<O> {
    fn new(
        clock: SharedOffset,
        output: O,
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
            scratch: Vec::new(),
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
                let state = match self.output.open(&self.settings.device, stream, format) {
                    Ok(sink) => State::Buffering(Track {
                        stream,
                        format,
                        sink,
                    }),
                    Err(OpenError(error)) => {
                        tracing::error!(device = self.settings.device, %error, "cannot open the output");
                        self.unavailable()
                    }
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

    fn buffering(
        &mut self,
        track: Track<O::Sink>,
        now: SinceBoot,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
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
        mut track: Track<O::Sink>,
        mut cursor: SinceBoot,
        now: SinceBoot,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
        match self.write_due(&mut track, &mut cursor, now) {
            Ok(()) => (State::Playing { track, cursor }, Some(cursor)),
            Err(SinkError::Lost(error)) => {
                tracing::error!(device = self.settings.device, %error, "output lost");
                (self.unavailable(), None)
            }
        }
    }

    fn write_due(
        &mut self,
        track: &mut Track<O::Sink>,
        cursor: &mut SinceBoot,
        now: SinceBoot,
    ) -> Result<(), SinkError> {
        let rate = track.format.sample_rate;
        let channels = u64::from(track.format.channels);
        let gain = self.gain();
        if now > after(*cursor, MAX_BEHIND) {
            let frames = frames_in(now.0.saturating_sub(cursor.0), rate);
            track.sink.write(Frames::Silence { frames })?;
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            *cursor = now;
        }
        while *cursor <= now {
            let deadline = self.buffer.front().and_then(|c| self.deadline(c));
            match deadline {
                Some(d) if after(d, TOLERANCE) < *cursor => self.drop_late(),
                Some(d) if d <= after(*cursor, TOLERANCE) => {
                    let chunk = self.buffer.pop_front().expect("deadline came from a chunk");
                    self.scratch.clear();
                    let samples = chunk.data.as_chunks::<2>().0;
                    self.scratch
                        .extend(samples.iter().map(|&s| scale(i16::from_le_bytes(s), gain)));
                    track.sink.write(Frames::Audio {
                        pcm: &self.scratch,
                        play_at: chunk.play_at,
                        heard: after(*cursor, self.settings.latency),
                    })?;
                    let frames = self.scratch.len() as u64 / channels;
                    *cursor = after(*cursor, duration_of(frames, rate));
                }
                gap => {
                    let silence = gap.map_or(PERIOD, |d| d.0.saturating_sub(cursor.0).min(PERIOD));
                    let frames = frames_in(silence, rate);
                    track.sink.write(Frames::Silence { frames })?;
                    self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                    *cursor = after(*cursor, duration_of(frames, rate));
                }
            }
        }
        Ok(())
    }

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
        let frames = chunk.data.len() as u64 / (2 * u64::from(track.format.channels));
        frames * 1_000_000 / u64::from(track.format.sample_rate)
    }

    fn unavailable(&self) -> State<O::Sink> {
        State::OutputUnavailable {
            device: self.settings.device.clone(),
        }
    }

    fn enter(&mut self, state: State<O::Sink>) {
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

impl<S> State<S> {
    fn track(&self) -> Option<&Track<S>> {
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
    use std::path::PathBuf;

    use bounce_common::audio::Codec;
    use bounce_common::time::{ClockOffset, ServerInstant};

    use super::*;

    const FORMAT: AudioFormat = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
        codec: Codec::PcmS16Le,
    };
    const STREAM: StreamId = StreamId(7);
    const FRAMES: usize = 480;

    struct Rig {
        player: Player<WavOutput>,
        changes: Receiver<PlayerState>,
        dir: PathBuf,
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("bounce-player-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let clock = SharedOffset::default();
        clock.set(Some(ClockOffset(0)));
        let (changes_tx, changes) = mpsc::channel();
        let mut player = Player::new(
            clock,
            WavOutput {
                dir: dir.clone(),
                wall_origin_us: 0,
            },
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
        assert_eq!(rig.timing(), "0 100000 100000\n");
        assert_eq!(rig.states(), [PlayerState::Playing { stream: STREAM }]);
    }

    #[test]
    fn timing_records_when_each_chunk_plays_on_the_wall_clock() {
        let mut rig = rig("heard");
        rig.player.clock.set(Some(ClockOffset(5_000)));
        rig.player.apply(chunk(0, 100, 1_000));
        let PlayerInput::Chunk(mut off) = chunk(1, 110, 2_000) else {
            unreachable!()
        };
        off.play_at = ServerInstant(110_600);
        rig.player.apply(PlayerInput::Chunk(off));

        rig.player.step(ms(95));
        rig.player.step(ms(105));
        assert_eq!(rig.timing(), "0 100000 95000\n480 110600 105000\n");
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
        assert_eq!(rig.timing(), "0 110000 110000\n");
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
        assert_eq!(timing.lines().last(), Some("91200 2000000 2000000"));
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

        assert_eq!(rig.timing(), "0 100000 100000\n1440 130000 130000\n");
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

        assert_eq!(rig.timing(), "0 100000 100000\n480 600000 600000\n");
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
    fn an_output_that_cannot_open_reports_its_device_unavailable() {
        let (changes_tx, changes) = mpsc::channel();
        let mut player = Player::new(
            SharedOffset::default(),
            fake::Unavailable,
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
