//! The audio thread: hold chunks until their deadline, then write them to
//! the output.

#[cfg(target_os = "linux")]
mod alsa_output;
mod drift;
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
use drift::{Correction, DeviceClock};

#[cfg(target_os = "linux")]
pub use alsa_output::{AlsaOutput, AlsaSink};
pub use sink::{Clock, Frames, OpenError, Output, Sink, SinkError, WRITE_AHEAD};
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
    gained: Vec<i16>,
}

enum State<S> {
    Idle,
    Buffering {
        track: Track<S>,
        cursor: Option<SinceBoot>,
    },
    Playing {
        track: Track<S>,
        cursor: SinceBoot,
        underrun_counted: bool,
    },
    OutputUnavailable {
        device: String,
        wanted: Wanted,
        retry_at: Option<SinceBoot>,
    },
}

#[derive(Clone, Copy)]
struct Wanted {
    stream: StreamId,
    format: AudioFormat,
}

struct Track<S> {
    wanted: Wanted,
    sink: S,
    drift: DeviceClock,
}

struct Lead {
    fill_to: Duration,
    pad_when_dry_within: Duration,
    max_behind: Duration,
    min_wake: Duration,
}

impl Lead {
    const WRITTEN: Self = Self {
        fill_to: Duration::ZERO,
        pad_when_dry_within: Duration::ZERO,
        max_behind: MAX_BEHIND,
        min_wake: Duration::ZERO,
    };
    /// `PipeWire` was measured running dry without an error, so content that
    /// should already be playing means the device starved. While `PipeWire`
    /// holds back its first 60 ms, the walk would otherwise wake every 0.3 ms.
    const DEVICE: Self = Self {
        fill_to: WRITE_AHEAD,
        pad_when_dry_within: Duration::from_millis(15),
        max_behind: Duration::ZERO,
        min_wake: Duration::from_millis(5),
    };
}

struct Walked {
    heard: bool,
    wake: SinceBoot,
}

const TOLERANCE: Duration = Duration::from_millis(1);
const PERIOD: Duration = Duration::from_millis(10);
/// WAV writes on a busy ext4 disk were measured blocking 26 to 180 ms.
const MAX_BEHIND: Duration = Duration::from_millis(500);
const NO_CLOCK_POLL: Duration = Duration::from_millis(10);
const MAX_BUFFERED: usize = 1_000;
const OPEN_RETRY: Duration = Duration::from_secs(2);

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
            gained: Vec::new(),
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
                if self.state.track().map(|t| t.wanted.stream) != Some(chunk.stream) {
                    return;
                }
                if self.buffer.len() == MAX_BUFFERED {
                    self.buffer.pop_front();
                    self.counters.late_chunks.fetch_add(1, Ordering::Relaxed);
                }
                self.buffer.push_back(chunk);
            }
            PlayerInput::Start { stream, format } => {
                if self.state.wanted().map(|w| w.stream) == Some(stream) {
                    return;
                }
                self.buffer.clear();
                self.open(Wanted { stream, format });
            }
            PlayerInput::Stop => {
                self.buffer.clear();
                self.enter(State::Idle);
            }
            PlayerInput::Configure(settings) => {
                let moved = settings.device != self.settings.device;
                self.settings = settings;
                if let Some(wanted) = self.state.wanted().filter(|_| moved) {
                    self.open(wanted);
                }
            }
        }
    }

    fn open(&mut self, wanted: Wanted) {
        let before = self.state.reported();
        // The old sink goes first: one sound card opened twice is busy.
        drop(std::mem::replace(&mut self.state, State::Idle));
        self.state = match self.track(wanted) {
            Ok(track) => State::Buffering {
                track,
                cursor: None,
            },
            Err(OpenError(error)) => {
                tracing::error!(device = self.settings.device, %error, "cannot open the output");
                self.unavailable(wanted, None)
            }
        };
        self.announce(before);
    }

    fn track(&self, wanted: Wanted) -> Result<Track<O::Sink>, OpenError> {
        let sink = self
            .output
            .open(&self.settings.device, wanted.stream, wanted.format)?;
        Ok(Track {
            wanted,
            sink,
            drift: DeviceClock::new(wanted.format.sample_rate),
        })
    }

    fn step(&mut self, now: SinceBoot) -> Option<SinceBoot> {
        let before = self.state.reported();
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (state, wake) = match state {
            State::Idle => (State::Idle, None),
            State::Buffering { track, cursor } => self.buffering(track, cursor, now),
            State::Playing {
                track,
                cursor,
                underrun_counted,
            } => self.playing(track, cursor, underrun_counted, now),
            State::OutputUnavailable {
                wanted, retry_at, ..
            } => self.retry(wanted, retry_at, now),
        };
        self.state = state;
        self.announce(before);
        let buffered: u64 = self.buffer.iter().map(|c| self.chunk_micros(c)).sum();
        self.counters.buffered_us.store(buffered, Ordering::Relaxed);
        wake
    }

    fn retry(
        &mut self,
        wanted: Wanted,
        retry_at: Option<SinceBoot>,
        now: SinceBoot,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
        let retry_at = retry_at.unwrap_or(after(now, OPEN_RETRY));
        if retry_at > now {
            return (self.unavailable(wanted, Some(retry_at)), Some(retry_at));
        }
        match self.track(wanted) {
            Ok(track) => {
                tracing::info!(device = self.settings.device, "the output is back");
                self.buffering(track, None, now)
            }
            Err(OpenError(error)) => {
                tracing::debug!(device = self.settings.device, %error, "still cannot open the output");
                let retry_at = after(now, OPEN_RETRY);
                (self.unavailable(wanted, Some(retry_at)), Some(retry_at))
            }
        }
    }

    fn buffering(
        &mut self,
        mut track: Track<O::Sink>,
        cursor: Option<SinceBoot>,
        now: SinceBoot,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
        let clock = match track.sink.clock(now) {
            Ok(clock) => clock,
            Err(error) => return self.failed(track, error, now, false),
        };
        let mut cursor = match (cursor, clock) {
            (Some(cursor), _) => cursor,
            (None, Clock::Device { queued }) => after(now, queued),
            (None, Clock::Written) => match self.first_deadline(now) {
                Ok(deadline) => deadline,
                Err(wake) => {
                    let state = State::Buffering {
                        track,
                        cursor: None,
                    };
                    return (state, wake);
                }
            },
        };
        match self.walk(&mut track, &mut cursor, clock, now, true) {
            Ok(Walked { heard: true, wake }) => {
                let state = State::Playing {
                    track,
                    cursor,
                    underrun_counted: false,
                };
                (state, Some(wake))
            }
            Ok(Walked { heard: false, wake }) => {
                let cursor = Some(cursor);
                (State::Buffering { track, cursor }, Some(wake))
            }
            Err(error) => self.failed(track, error, now, false),
        }
    }

    fn first_deadline(&mut self, now: SinceBoot) -> Result<SinceBoot, Option<SinceBoot>> {
        loop {
            let Some(chunk) = self.buffer.front() else {
                return Err(None);
            };
            let Some(deadline) = self.deadline(chunk) else {
                return Err(Some(after(now, NO_CLOCK_POLL)));
            };
            if after(deadline, TOLERANCE) < now {
                self.drop_late();
            } else if deadline > now {
                return Err(Some(deadline));
            } else {
                return Ok(deadline);
            }
        }
    }

    fn playing(
        &mut self,
        mut track: Track<O::Sink>,
        mut cursor: SinceBoot,
        underrun_counted: bool,
        now: SinceBoot,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
        let walked = track
            .sink
            .clock(now)
            .and_then(|clock| self.walk(&mut track, &mut cursor, clock, now, underrun_counted));
        match walked {
            Ok(Walked { heard, wake }) => {
                let underrun_counted = underrun_counted && !heard;
                let state = State::Playing {
                    track,
                    cursor,
                    underrun_counted,
                };
                (state, Some(wake))
            }
            Err(error) => self.failed(track, error, now, true),
        }
    }

    fn failed(
        &mut self,
        mut track: Track<O::Sink>,
        error: SinkError,
        now: SinceBoot,
        playing: bool,
    ) -> (State<O::Sink>, Option<SinceBoot>) {
        match error {
            SinkError::Underrun => {
                track.drift = DeviceClock::new(track.wanted.format.sample_rate);
                let state = if playing {
                    self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                    let cursor = match track.sink.clock(now) {
                        Ok(Clock::Device { queued }) => after(now, queued),
                        _ => now,
                    };
                    State::Playing {
                        track,
                        cursor,
                        underrun_counted: true,
                    }
                } else {
                    State::Buffering {
                        track,
                        cursor: None,
                    }
                };
                (state, Some(now))
            }
            SinkError::Lost(error) => {
                tracing::error!(device = self.settings.device, %error, "lost the output");
                let retry_at = after(now, OPEN_RETRY);
                (
                    self.unavailable(track.wanted, Some(retry_at)),
                    Some(retry_at),
                )
            }
        }
    }

    fn walk(
        &mut self,
        track: &mut Track<O::Sink>,
        cursor: &mut SinceBoot,
        clock: Clock,
        now: SinceBoot,
        priming: bool,
    ) -> Result<Walked, SinkError> {
        let rate = track.wanted.format.sample_rate;
        let (lead, horizon) = match clock {
            Clock::Written => (Lead::WRITTEN, now),
            Clock::Device { queued } => {
                let queued = Self::correct(track, cursor, now, queued, priming)?;
                (Lead::DEVICE, SinceBoot(cursor.0.saturating_sub(queued)))
            }
        };
        if !priming && now > after(*cursor, lead.max_behind) {
            if clock != Clock::Written {
                return Err(SinkError::Underrun);
            }
            let frames = frames_in(now.0.saturating_sub(cursor.0), rate);
            track.write(Frames::Silence { frames })?;
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            *cursor = now;
        }
        let mut heard = false;
        let wake_at = |cursor: SinceBoot, ahead: Duration| {
            let due = cursor.0.saturating_sub(horizon.0).saturating_sub(ahead);
            SinceBoot(now.0 + due.max(lead.min_wake))
        };
        loop {
            if *cursor > after(horizon, lead.fill_to) {
                let wake = wake_at(*cursor, lead.fill_to);
                return Ok(Walked { heard, wake });
            }
            let deadline = self.buffer.front().and_then(|c| self.deadline(c));
            match deadline {
                Some(d) if after(d, TOLERANCE) < *cursor => self.drop_late(),
                Some(d) if d <= after(*cursor, TOLERANCE) => {
                    self.play_front(track, cursor, priming && !heard)?;
                    heard = true;
                }
                gap => {
                    let filling = priming && !heard;
                    let pad_when_dry_within = if filling {
                        lead.fill_to
                    } else {
                        lead.pad_when_dry_within
                    };
                    if *cursor > after(horizon, pad_when_dry_within) {
                        let wake = wake_at(*cursor, pad_when_dry_within);
                        return Ok(Walked { heard, wake });
                    }
                    let silence = gap.map_or(PERIOD, |d| d.0.saturating_sub(cursor.0).min(PERIOD));
                    let frames = frames_in(silence, rate);
                    track.write(Frames::Silence { frames })?;
                    if !filling {
                        self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    *cursor = after(*cursor, duration_of(frames, rate));
                }
            }
        }
    }

    fn correct(
        track: &mut Track<O::Sink>,
        cursor: &mut SinceBoot,
        now: SinceBoot,
        queued: Duration,
        priming: bool,
    ) -> Result<Duration, SinkError> {
        let rate = track.wanted.format.sample_rate;
        track
            .drift
            .observe(frames_between(*cursor, after(now, queued), rate));
        match track.drift.correction(priming) {
            Correction::PadSilence(frames) => {
                track.write(Frames::Silence { frames })?;
                track.drift.reset();
                Ok(queued + duration_of(frames, rate))
            }
            Correction::SkipAhead(frames) => {
                *cursor = after(*cursor, duration_of(frames, rate));
                track.drift.reset();
                Ok(queued)
            }
            Correction::None | Correction::RepeatFrame | Correction::DropFrame => Ok(queued),
        }
    }

    fn play_front(
        &mut self,
        track: &mut Track<O::Sink>,
        cursor: &mut SinceBoot,
        priming: bool,
    ) -> Result<(), SinkError> {
        let chunk = self.buffer.pop_front().expect("the walk saw a chunk");
        let gain = self.gain();
        let channels = usize::from(track.wanted.format.channels);
        self.gained.clear();
        let samples = chunk.data.as_chunks::<2>().0;
        self.gained
            .extend(samples.iter().map(|&s| scale(i16::from_le_bytes(s), gain)));
        let frames = self.gained.len() / channels;
        if let Some(last) = self.gained.len().checked_sub(channels) {
            match track.drift.correction(priming) {
                Correction::RepeatFrame => {
                    self.gained.extend_from_within(last..);
                    track.drift.inserted(1);
                }
                Correction::DropFrame => {
                    self.gained.truncate(last);
                    track.drift.inserted(-1);
                }
                Correction::None | Correction::PadSilence(_) | Correction::SkipAhead(_) => {}
            }
        }
        track.write(Frames::Audio {
            pcm: &self.gained,
            play_at: chunk.play_at,
            heard: after(*cursor, self.settings.latency),
        })?;
        let rate = track.wanted.format.sample_rate;
        *cursor = after(*cursor, duration_of(frames as u64, rate));
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
        let format = track.wanted.format;
        let frames = chunk.data.len() as u64 / (2 * u64::from(format.channels));
        frames * 1_000_000 / u64::from(format.sample_rate)
    }

    fn unavailable(&mut self, wanted: Wanted, retry_at: Option<SinceBoot>) -> State<O::Sink> {
        self.buffer.clear();
        State::OutputUnavailable {
            device: self.settings.device.clone(),
            wanted,
            retry_at,
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

impl<S: Sink> Track<S> {
    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError> {
        if matches!(frames, Frames::Silence { frames: 0 }) {
            return Ok(());
        }
        self.sink.write(frames)
    }
}

impl<S> State<S> {
    fn track(&self) -> Option<&Track<S>> {
        match self {
            Self::Buffering { track, .. } | Self::Playing { track, .. } => Some(track),
            Self::Idle | Self::OutputUnavailable { .. } => None,
        }
    }

    fn wanted(&self) -> Option<Wanted> {
        match self {
            Self::OutputUnavailable { wanted, .. } => Some(*wanted),
            other => other.track().map(|t| t.wanted),
        }
    }

    fn reported(&self) -> PlayerState {
        match self {
            Self::Idle => PlayerState::Idle,
            Self::Buffering { track, .. } => PlayerState::Buffering {
                stream: track.wanted.stream,
            },
            Self::Playing { track, .. } => PlayerState::Playing {
                stream: track.wanted.stream,
            },
            Self::OutputUnavailable { device, .. } => PlayerState::OutputUnavailable {
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

fn frames_between(from: SinceBoot, to: SinceBoot, rate: u32) -> i64 {
    let nanos = i128::try_from(to.0.as_nanos()).unwrap_or(i128::MAX)
        - i128::try_from(from.0.as_nanos()).unwrap_or(i128::MAX);
    i64::try_from(nanos * i128::from(rate) / 1_000_000_000).unwrap_or(i64::MAX)
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

    mod clocked {
        use std::cell::RefCell;
        use std::rc::Rc;

        use super::*;
        use crate::player::fake::{Dac, FakeOutput, Heard};

        const ARRIVAL: u64 = 10;
        const MINUTE: u64 = 60_000;

        struct DacRig {
            player: Player<FakeOutput>,
            dac: Rc<RefCell<Dac>>,
            changes: Receiver<PlayerState>,
            lead: u64,
            now: SinceBoot,
            wake: Option<SinceBoot>,
            sent: u64,
        }

        fn dac_rig(dac: Dac, lead: u64) -> DacRig {
            let dac = dac.shared();
            let clock = SharedOffset::default();
            clock.set(Some(ClockOffset(0)));
            let (changes_tx, changes) = mpsc::channel();
            let mut player = Player::new(
                clock,
                FakeOutput(Rc::clone(&dac)),
                Arc::default(),
                changes_tx,
            );
            player.apply(PlayerInput::Configure(on("dac")));
            player.apply(PlayerInput::Start {
                stream: STREAM,
                format: FORMAT,
            });
            let mut rig = DacRig {
                player,
                dac,
                changes,
                lead,
                now: ms(0),
                wake: None,
                sent: 0,
            };
            rig.step_at(ms(0));
            rig
        }

        fn on(device: &str) -> OutputSettings {
            OutputSettings {
                device: device.into(),
                ..settings(100, false)
            }
        }

        fn sample(seq: u64) -> i16 {
            i16::try_from(seq % 30_000).unwrap() + 1
        }

        impl DacRig {
            fn run_until(&mut self, until: u64) {
                let mut repeats = 0;
                loop {
                    let arrival = ms(self.sent * ARRIVAL);
                    let next = self.wake.map_or(arrival, |w| w.min(arrival)).max(self.now);
                    if next > ms(until) {
                        return;
                    }
                    repeats = if next == self.now { repeats + 1 } else { 0 };
                    assert!(repeats < 10, "the player keeps waking at {next:?}");
                    self.step_at(next);
                }
            }

            fn stall_until(&mut self, at: u64) {
                self.step_at(ms(at));
            }

            fn step_at(&mut self, now: SinceBoot) {
                self.now = now;
                while ms(self.sent * ARRIVAL) <= now {
                    let play_at = self.sent * ARRIVAL + self.lead;
                    self.player
                        .apply(chunk(self.sent, play_at, sample(self.sent)));
                    self.sent += 1;
                }
                self.wake = self.player.step(now);
            }

            fn heard(&self) -> Vec<Heard> {
                self.dac.borrow().heard.clone()
            }

            fn states(&self) -> Vec<PlayerState> {
                self.changes.try_iter().collect()
            }

            fn counter(&self, pick: fn(&PlaybackCounters) -> &AtomicU64) -> u64 {
                pick(&self.player.counters).load(Ordering::Relaxed)
            }
        }

        fn lateness_us(heard: &Heard) -> i64 {
            let at = i64::try_from(heard.at.0.as_micros()).unwrap();
            at - i64::try_from(heard.play_at.0).unwrap()
        }

        fn worst(heard: &[Heard]) -> i64 {
            heard.iter().map(|h| lateness_us(h).abs()).max().unwrap()
        }

        fn playing() -> PlayerState {
            PlayerState::Playing { stream: STREAM }
        }

        fn buffering() -> PlayerState {
            PlayerState::Buffering { stream: STREAM }
        }

        #[test]
        fn a_drifting_dac_stays_within_a_millisecond_of_content_for_ten_minutes() {
            for ppm in [-200.0, 0.0, 33.0, 200.0] {
                let mut rig = dac_rig(
                    Dac {
                        ppm,
                        ..Dac::default()
                    },
                    1_000,
                );
                rig.run_until(10 * MINUTE);

                let heard = rig.heard();
                assert!(heard.len() > 59_800, "{ppm} ppm: heard {}", heard.len());
                assert!(
                    worst(&heard) < 1_000,
                    "{ppm} ppm: off by {} µs",
                    worst(&heard)
                );
                assert_eq!(rig.counter(|c| &c.underruns), 0, "{ppm} ppm: underruns");
                assert_eq!(rig.counter(|c| &c.late_chunks), 0, "{ppm} ppm: late chunks");
                let max_queued = rig.dac.borrow().max_queued;
                assert!(
                    max_queued < 2 * WRITE_AHEAD,
                    "{ppm} ppm: the device held {max_queued:?}, so a real write would block"
                );
            }
        }

        #[test]
        fn period_quantized_delay_readings_hold_within_a_millisecond() {
            let dac = Dac {
                ppm: 33.0,
                quantum: 1_024,
                ..Dac::default()
            };
            let mut rig = dac_rig(dac, 1_000);
            rig.run_until(3 * MINUTE);

            let heard = rig.heard();
            assert!(heard.len() > 17_800, "heard {}", heard.len());
            assert!(worst(&heard) < 1_000, "off by {} µs", worst(&heard));
            assert_eq!(rig.counter(|c| &c.underruns), 0);
            assert_eq!(rig.counter(|c| &c.late_chunks), 0);
        }

        #[test]
        fn the_start_transient_lands_the_first_chunk_on_its_deadline() {
            let dac = Dac {
                start_delay: Duration::from_millis(60),
                ..Dac::default()
            };
            let mut rig = dac_rig(dac, 300);
            rig.run_until(2_000);

            let heard = rig.heard();
            assert_eq!(heard[0].first_sample, sample(0), "the first chunk is heard");
            assert!(
                lateness_us(&heard[0]).abs() < 100,
                "first chunk off by {} µs",
                lateness_us(&heard[0])
            );
            assert!(worst(&heard) < 1_000, "off by {} µs", worst(&heard));
            assert_eq!(rig.counter(|c| &c.underruns), 0);
            assert_eq!(rig.counter(|c| &c.late_chunks), 0);
            assert_eq!(rig.states(), [buffering(), playing()]);
        }

        #[test]
        fn a_stalled_thread_counts_one_underrun_and_resumes_on_the_deadlines() {
            for silent_underruns in [false, true] {
                let dac = Dac {
                    silent_underruns,
                    ..Dac::default()
                };
                let mut rig = dac_rig(dac, 1_000);
                rig.run_until(3_000);
                let before = rig.heard().len();
                rig.stall_until(3_305);
                rig.run_until(5_000);

                let mode = if silent_underruns {
                    "silent"
                } else {
                    "reported"
                };
                assert_eq!(rig.counter(|c| &c.underruns), 1, "{mode} underrun");
                let resumed = &rig.heard()[before..];
                assert_eq!(
                    resumed[0].play_at,
                    ServerInstant(3_310_000),
                    "{mode}: playback resumes on the first chunk still in the future"
                );
                assert!(
                    worst(resumed) < 1_000,
                    "{mode}: off by {} µs",
                    worst(resumed)
                );
                assert!(
                    rig.counter(|c| &c.late_chunks) > 0,
                    "{mode}: chunks due in the stall are dropped"
                );
                assert_eq!(
                    rig.states(),
                    [buffering(), playing()],
                    "{mode}: stays Playing"
                );
            }
        }

        #[test]
        fn an_unplugged_device_is_retried_until_it_plays_again() {
            let mut rig = dac_rig(Dac::default(), 1_000);
            rig.run_until(2_000);
            rig.dac.borrow_mut().unplugged = true;
            rig.run_until(2_100);
            let unavailable = PlayerState::OutputUnavailable {
                device: "dac".into(),
            };
            assert_eq!(rig.states(), [buffering(), playing(), unavailable]);

            rig.dac.borrow_mut().unplugged = false;
            let before = rig.heard().len();
            rig.run_until(7_000);
            assert_eq!(
                rig.states(),
                [buffering(), playing()],
                "back without a new Start"
            );
            let resumed = &rig.heard()[before..];
            assert!(
                resumed.len() > 100,
                "heard {} after the replug",
                resumed.len()
            );
            assert!(worst(resumed) < 1_000, "off by {} µs", worst(resumed));
            assert_eq!(rig.counter(|c| &c.underruns), 0);
        }

        #[test]
        fn a_busy_device_is_retried_until_it_opens() {
            let mut rig = dac_rig(
                Dac {
                    busy: true,
                    ..Dac::default()
                },
                1_000,
            );
            rig.run_until(500);
            let unavailable = PlayerState::OutputUnavailable {
                device: "dac".into(),
            };
            assert_eq!(rig.states(), [unavailable]);

            rig.dac.borrow_mut().busy = false;
            rig.run_until(5_000);
            assert_eq!(rig.states(), [buffering(), playing()]);
            let heard = rig.heard();
            assert!(heard.len() > 100, "heard {}", heard.len());
            assert!(worst(&heard) < 1_000, "off by {} µs", worst(&heard));
        }

        #[test]
        fn configure_moves_playback_to_another_device_on_the_deadlines() {
            let mut rig = dac_rig(Dac::default(), 1_000);
            rig.run_until(2_000);
            rig.player.apply(PlayerInput::Configure(on("other")));
            rig.run_until(4_000);

            let heard = rig.heard();
            let first_moved = heard.iter().position(|h| h.device == "other").unwrap();
            assert!(
                heard[first_moved..].iter().all(|h| h.device == "other"),
                "nothing more is heard on the old device"
            );
            assert!(
                heard[first_moved].play_at <= ServerInstant(2_070_000),
                "the new device resumes with the first chunk the old one was not given, \
                 at {:?}",
                heard[first_moved].play_at
            );
            assert!(worst(&heard) < 1_000, "off by {} µs", worst(&heard));
            assert_eq!(rig.dac.borrow().opens, ["dac", "other"]);
            assert_eq!(
                rig.states(),
                [buffering(), playing(), buffering(), playing()]
            );
        }

        #[test]
        fn a_resent_start_or_configure_changes_nothing() {
            let mut rig = dac_rig(Dac::default(), 1_000);
            rig.run_until(2_000);
            for at in (2_000..3_000).step_by(100) {
                rig.player.apply(PlayerInput::Configure(on("dac")));
                rig.player.apply(PlayerInput::Start {
                    stream: STREAM,
                    format: FORMAT,
                });
                rig.run_until(at + 100);
            }

            assert_eq!(rig.dac.borrow().opens, ["dac"]);
            assert_eq!(rig.states(), [buffering(), playing()]);
            let heard = rig.heard();
            assert_eq!(
                heard.iter().map(|h| h.first_sample).collect::<Vec<_>>(),
                (0..heard.len() as u64).map(sample).collect::<Vec<_>>(),
                "every chunk is heard once, in order"
            );
            assert!(worst(&heard) < 1_000, "off by {} µs", worst(&heard));
            assert_eq!(rig.counter(|c| &c.underruns), 0);
        }
    }
}
