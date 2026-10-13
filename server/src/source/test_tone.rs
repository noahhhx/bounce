//! Starts playing as soon as it starts, so routing a zone to it is all it
//! takes to hear something.
//!
//! The signal is a quiet sine with a full-scale 5 ms click at every whole
//! second of stream time.

use std::f64::consts::TAU;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::{Source, SourceControl, SourceEvent, SourceKind, SourceOutput};

const CHUNKS_PER_SECOND: u32 = 100;
const SINE_PEAK: i16 = 3_000;

pub struct TestToneConfig {
    pub name: String,
    pub frequency_hz: u32,
    pub sample_rate: u32,
}

pub struct TestToneSource {
    config: TestToneConfig,
    task: Option<JoinHandle<()>>,
}

impl TestToneSource {
    #[must_use]
    pub fn new(config: TestToneConfig) -> Self {
        Self { config, task: None }
    }
}

impl Source for TestToneSource {
    fn kind(&self) -> SourceKind {
        SourceKind::TestTone
    }

    fn start(&mut self, out: SourceOutput) {
        let tone = Tone::new(self.config.frequency_hz, self.config.sample_rate);
        self.task = Some(tokio::spawn(play(tone, out)));
    }

    /// Source volume is applied downstream in each client's desired volume,
    /// and there is no casting app to mirror it to.
    fn control(&mut self, _: SourceControl) {}

    fn stop(&mut self) -> Option<JoinHandle<()>> {
        let task = self.task.take()?;
        task.abort();
        Some(task)
    }
}

async fn play(mut tone: Tone, out: SourceOutput) {
    out.event(SourceEvent::Started {
        sample_rate: tone.sample_rate,
        channels: 2,
    });
    let frames = tone.sample_rate / CHUNKS_PER_SECOND;
    let mut tick = tokio::time::interval(Duration::from_secs(1) / CHUNKS_PER_SECOND);
    loop {
        tick.tick().await;
        out.pcm(&tone.next_chunk(frames));
    }
}

/// Restarts each whole second. The frequency is a whole number of hertz,
/// so the sine is back at phase zero there too.
struct Tone {
    frequency_hz: u32,
    sample_rate: u32,
    click_frames: u32,
    position: u32,
}

impl Tone {
    fn new(frequency_hz: u32, sample_rate: u32) -> Self {
        Self {
            frequency_hz,
            sample_rate,
            click_frames: sample_rate / 200,
            position: 0,
        }
    }

    /// Interleaved stereo, both channels equal.
    fn next_chunk(&mut self, frames: u32) -> Vec<i16> {
        (0..frames)
            .flat_map(|_| {
                let sample = self.next_sample();
                [sample, sample]
            })
            .collect()
    }

    fn next_sample(&mut self) -> i16 {
        let sample = if self.position < self.click_frames {
            i16::MAX
        } else {
            sine(
                f64::from(self.position) * f64::from(self.frequency_hz)
                    / f64::from(self.sample_rate),
            )
        };
        self.position = (self.position + 1) % self.sample_rate;
        sample
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the product is within ±SINE_PEAK"
)]
fn sine(cycles: f64) -> i16 {
    (f64::from(SINE_PEAK) * (TAU * cycles).sin()) as i16
}

#[cfg(test)]
mod tests {
    use bounce_common::ids::SourceId;
    use tokio::sync::mpsc;

    use super::*;
    use crate::hub::HubEvent;

    #[test]
    fn clicks_full_scale_for_5ms_at_each_whole_second() {
        let mut tone = Tone::new(440, 48_000);
        let two_seconds = tone.next_chunk(96_000);
        let loud: Vec<usize> = two_seconds
            .chunks(2)
            .enumerate()
            .filter(|(_, frame)| frame[0] > SINE_PEAK)
            .map(|(i, frame)| {
                assert_eq!(frame, [i16::MAX, i16::MAX]);
                i
            })
            .collect();
        let expected: Vec<usize> = (0..240).chain(48_000..48_240).collect();
        assert_eq!(loud, expected);
    }

    #[test]
    fn sine_continues_across_chunks() {
        let whole = Tone::new(440, 48_000).next_chunk(1_000);
        let mut split = Tone::new(440, 48_000);
        let mut parts = split.next_chunk(300);
        parts.extend(split.next_chunk(700));
        assert_eq!(whole, parts);
        assert!(whole[2 * 300..].iter().any(|&s| s != 0));
    }

    #[tokio::test(start_paused = true)]
    async fn announces_format_then_plays_in_real_time_until_stopped() {
        let (events, mut events_rx) = mpsc::unbounded_channel();
        let (pcm, mut pcm_rx) = mpsc::unbounded_channel();
        let mut source = TestToneSource::new(TestToneConfig {
            name: "Test tone".to_owned(),
            frequency_hz: 440,
            sample_rate: 48_000,
        });
        source.start(SourceOutput::new(SourceId(0), events, pcm));

        tokio::time::sleep(Duration::from_millis(995)).await;
        let started = events_rx.try_recv().expect("Started sent");
        assert!(matches!(
            started,
            HubEvent::Source {
                event: SourceEvent::Started {
                    sample_rate: 48_000,
                    channels: 2
                },
                ..
            }
        ));
        let mut samples = 0;
        while let Ok(chunk) = pcm_rx.try_recv() {
            samples += chunk.len();
        }
        assert_eq!(samples, 2 * 48_000);

        let _ = source.stop().expect("running").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(pcm_rx.try_recv().is_err(), "no audio after stop");
    }
}
