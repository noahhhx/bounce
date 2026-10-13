//! Cast targets. Each one advertises itself to casting apps on the LAN and
//! turns what it receives into PCM plus events. The broadcaster downstream
//! never learns which source produced the audio.

pub mod spotify;

use std::time::Duration;

use bounce_common::zone::Volume;

/// Each source shows up as one device in its casting app. Which zones hear
/// it is decided in the web UI, not the casting app.
pub trait Source: Send {
    fn kind(&self) -> SourceKind;

    /// Start advertising. Audio and events go to `out` until [`Source::stop`].
    fn start(&mut self, out: SourceOutput);

    /// A change made outside the casting app, such as a volume move in the
    /// web UI, that the app should reflect.
    fn control(&mut self, control: SourceControl);

    fn stop(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Spotify,
}

/// Handed to a source on start. Wraps the channels to the hub and to this
/// source's broadcaster, so a source only ever calls these two methods.
pub struct SourceOutput {}

impl SourceOutput {
    /// Interleaved samples in the format of the last [`SourceEvent::Started`].
    pub fn pcm(&self, samples: &[i16]) {
        todo!()
    }

    pub fn event(&self, event: SourceEvent) {
        todo!()
    }
}

#[derive(Debug, Clone)]
pub enum SourceEvent {
    Started {
        sample_rate: u32,
        channels: u8,
    },
    Track(TrackMetadata),
    Paused,
    Resumed,
    Stopped,
    /// The user moved the volume slider in the casting app. This is the
    /// source's master volume, scaled by each zone's own volume.
    VolumeChanged(Volume),
}

#[derive(Debug, Clone)]
pub enum SourceControl {
    SetVolume(Volume),
}

#[derive(Debug, Clone)]
pub struct TrackMetadata {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover_url: Option<String>,
    pub duration: Duration,
}
