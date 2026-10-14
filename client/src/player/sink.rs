//! What a platform implements to make sound. The scheduler in the parent
//! module decides what to write and when; a sink only opens, says how its
//! clock works, and takes frames.

use std::time::Duration;

use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::{ServerInstant, SinceBoot};

/// A platform's way of producing sound: WAV files, ALSA, later I2S.
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

    /// Never blocks for long: the scheduler keeps about [`WRITE_AHEAD`]
    /// queued, and a clocked sink holds at least twice that.
    ///
    /// # Errors
    ///
    /// [`SinkError::Underrun`] once the sink has recovered from running dry,
    /// [`SinkError::Lost`] when it cannot take any more frames.
    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError>;
}

/// Who decides when a written frame is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// Nobody: a frame is heard the moment it is written, as in a WAV file.
    /// The scheduler's cursor is the only clock.
    Written,
    /// A device consumes frames at its own rate. The next frame written at
    /// the step's `now` is heard `queued` later, before any output latency
    /// the server configured.
    Device { queued: Duration },
}

pub enum Frames<'a> {
    /// Interleaved, gain already applied. `heard` is when the first frame
    /// should reach the listener.
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
    /// The device ran dry and has recovered: it is empty and ready.
    Underrun,
    /// The device is gone: unplugged, an I/O error, or recovery failed.
    Lost(String),
}

#[derive(Debug)]
pub struct OpenError(pub String);

/// How far ahead the scheduler keeps a clocked device. Network jitter is
/// absorbed by the chunk buffer, so this only covers the audio thread
/// waking late.
pub const WRITE_AHEAD: Duration = Duration::from_millis(50);
