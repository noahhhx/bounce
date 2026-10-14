//! What a platform implements to make sound. The scheduler in the parent
//! module decides what to write and when; a sink only opens, says how its
//! clock works, and takes frames.

use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::{ServerInstant, SinceBoot};

/// A platform's way of producing sound: WAV files now, ALSA or I2S later.
pub trait Output {
    type Sink: Sink;

    /// What `Hello.outputs` advertises. `desired.output` is one of these ids.
    fn devices(&self) -> Vec<OutputDevice>;

    /// Called on the audio thread, once per stream.
    ///
    /// # Errors
    ///
    /// When the device is missing, busy, or cannot play `format` exactly.
    fn open(
        &self,
        device: &str,
        stream: StreamId,
        format: AudioFormat,
    ) -> Result<Self::Sink, OpenError>;
}

/// One open stream on one device. Dropping it stops the device at once.
pub trait Sink {
    /// Asked once per step, before any write. `now` is the step's time,
    /// passed so a fake device needs no clock of its own.
    ///
    /// # Errors
    ///
    /// As [`Sink::write`].
    fn clock(&mut self, now: SinceBoot) -> Result<Clock, SinkError>;

    /// # Errors
    ///
    /// [`SinkError::Lost`] when the sink cannot take any more frames.
    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError>;
}

/// Who decides when a written frame is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// Nobody: a frame is heard the moment it is written, as in a WAV file.
    /// The scheduler's cursor is the only clock.
    Written,
}

pub enum Frames<'a> {
    /// Interleaved, gain already applied. `heard` is when the first frame
    /// reaches the listener.
    Audio {
        pcm: &'a [i16],
        play_at: ServerInstant,
        heard: SinceBoot,
    },
    Silence {
        frames: u64,
    },
}

#[derive(Debug)]
pub enum SinkError {
    /// The device is gone: unplugged, an I/O error, or recovery failed.
    Lost(String),
}

#[derive(Debug)]
pub struct OpenError(pub String);
