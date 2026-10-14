use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::{ServerInstant, SinceBoot};

use super::sink::{Clock, Frames, OpenError, Output, Sink, SinkError};

pub struct Unavailable;

pub enum NoSink {}

impl Output for Unavailable {
    type Sink = NoSink;

    fn devices(&self) -> Vec<OutputDevice> {
        Vec::new()
    }

    fn open(&self, device: &str, _: StreamId, _: AudioFormat) -> Result<NoSink, OpenError> {
        Err(OpenError(format!("no device {device}")))
    }
}

impl Sink for NoSink {
    fn clock(&mut self, _: SinceBoot) -> Result<Clock, SinkError> {
        match *self {}
    }

    fn write(&mut self, _: Frames<'_>) -> Result<(), SinkError> {
        match *self {}
    }
}

#[derive(Default)]
pub struct Dac {
    pub ppm: f64,
    /// As `PipeWire` was measured doing after a start.
    pub start_delay: Duration,
    pub quantum: u64,
    /// As `PipeWire` was measured doing.
    pub silent_underruns: bool,
    pub busy: bool,
    pub unplugged: bool,
    pub heard: Vec<Heard>,
    pub opens: Vec<String>,
    pub max_queued: Duration,
    pub(super) open: Option<Format>,
    pub(super) now: Duration,
    pub(super) written: u64,
    pub(super) run: Option<Run>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub device: String,
    pub play_at: ServerInstant,
    pub at: SinceBoot,
    pub first_sample: i16,
}

#[derive(Clone, Copy)]
pub(super) struct Format {
    rate: u32,
    channels: u64,
}

#[derive(Clone, Copy)]
pub(super) struct Run {
    started: SinceBoot,
    first_frame: u64,
}

impl Dac {
    #[must_use]
    pub fn shared(self) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(self))
    }

    fn rate(&self) -> f64 {
        f64::from(self.format().rate) * (1.0 + self.ppm / 1e6)
    }

    fn format(&self) -> Format {
        self.open.expect("the device is open")
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a simulated position fits in u64"
    )]
    fn consumed(&self, run: Run, now: SinceBoot) -> u64 {
        let elapsed = now.0.saturating_sub(run.started.0).as_secs_f64();
        run.first_frame + (elapsed * self.rate()) as u64
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "simulated positions stay far below 2^52 frames"
    )]
    fn heard_at(&self, run: Run, frame: u64) -> SinceBoot {
        let offset = (frame - run.first_frame) as f64 / self.rate();
        SinceBoot(run.started.0 + Duration::from_secs_f64(offset))
    }

    fn queued(&self) -> u64 {
        let Some(run) = self.run else {
            return 0;
        };
        let consumed = self.consumed(run, SinceBoot(self.now)).min(self.written);
        let pointer = match self.quantum {
            0 | 1 => consumed,
            q => run.first_frame + (consumed - run.first_frame) / q * q,
        };
        self.written - pointer
    }

    fn duration(&self, frames: u64) -> Duration {
        Duration::from_nanos(frames * 1_000_000_000 / u64::from(self.format().rate))
    }
}

pub struct FakeOutput(pub Rc<RefCell<Dac>>);

pub struct FakeSink {
    dac: Rc<RefCell<Dac>>,
    device: String,
}

impl Output for FakeOutput {
    type Sink = FakeSink;

    fn devices(&self) -> Vec<OutputDevice> {
        Vec::new()
    }

    fn open(&self, device: &str, _: StreamId, format: AudioFormat) -> Result<FakeSink, OpenError> {
        let mut dac = self.0.borrow_mut();
        if dac.unplugged {
            return Err(OpenError("no such device".into()));
        }
        if dac.busy || dac.open.is_some() {
            return Err(OpenError("device or resource busy".into()));
        }
        dac.opens.push(device.into());
        dac.open = Some(Format {
            rate: format.sample_rate,
            channels: u64::from(format.channels),
        });
        dac.written = 0;
        dac.run = None;
        Ok(FakeSink {
            dac: Rc::clone(&self.0),
            device: device.into(),
        })
    }
}

impl Sink for FakeSink {
    fn clock(&mut self, now: SinceBoot) -> Result<Clock, SinkError> {
        let mut dac = self.dac.borrow_mut();
        dac.now = now.0;
        if dac.unplugged {
            return Err(SinkError::Lost("unplugged".into()));
        }
        if let Some(run) = dac.run {
            if dac.consumed(run, now) > dac.written {
                dac.run = None;
                if !dac.silent_underruns {
                    return Err(SinkError::Underrun);
                }
            }
        }
        let queued = dac.duration(dac.queued());
        Ok(Clock::Device { queued })
    }

    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError> {
        let mut dac = self.dac.borrow_mut();
        if dac.unplugged {
            return Err(SinkError::Lost("unplugged".into()));
        }
        let start = Run {
            started: SinceBoot(dac.now + dac.start_delay),
            first_frame: dac.written,
        };
        let run = *dac.run.get_or_insert(start);
        let count = match frames {
            Frames::Audio { pcm, play_at, .. } => {
                let heard = Heard {
                    device: self.device.clone(),
                    play_at,
                    at: dac.heard_at(run, dac.written),
                    first_sample: pcm.first().copied().unwrap_or_default(),
                };
                dac.heard.push(heard);
                pcm.len() as u64 / dac.format().channels
            }
            Frames::Silence { frames } => frames,
        };
        assert!(count > 0, "no empty writes");
        dac.written += count;
        let queued = dac.duration(dac.queued());
        dac.max_queued = dac.max_queued.max(queued);
        Ok(())
    }
}

impl Drop for FakeSink {
    fn drop(&mut self) {
        let mut dac = self.dac.borrow_mut();
        dac.open = None;
        dac.run = None;
    }
}
