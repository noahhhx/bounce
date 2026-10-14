//! Outputs for tests.

use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::SinceBoot;

use super::sink::{Clock, Frames, OpenError, Output, Sink, SinkError};

/// An output whose every open fails, like a missing sound card.
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
