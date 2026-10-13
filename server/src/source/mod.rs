//! Cast targets. Each one advertises itself to casting apps on the LAN and
//! turns what it receives into PCM plus events. The broadcaster downstream
//! never learns which source produced the audio.

pub mod spotify;
pub mod test_tone;

use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use bounce_common::ids::SourceId;
use bounce_common::zone::Volume;

use self::spotify::SpotifyConfig;
use self::test_tone::TestToneConfig;
use crate::hub::HubEvent;

/// Each source shows up as one device in its casting app. Which zones hear
/// it is decided in the web UI, not the casting app.
pub trait Source: Send {
    fn kind(&self) -> SourceKind;

    /// Start advertising. Audio and events go to `out` until [`Source::stop`].
    fn start(&mut self, out: SourceOutput);

    /// A change made outside the casting app, such as a volume move in the
    /// web UI, that the app should reflect.
    fn control(&mut self, control: SourceControl);

    /// The returned task ends once the source has released its sockets and
    /// mDNS records. Await it before the runtime shuts down: librespot's mDNS
    /// responder aborts the process if its tasks are torn down first.
    fn stop(&mut self) -> Option<JoinHandle<()>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SourceKind {
    Spotify,
    TestTone,
}

pub enum SourceConfig {
    Spotify(SpotifyConfig),
    TestTone(TestToneConfig),
}

/// Handed to a source on start. Wraps the channels to the hub and to this
/// source's broadcaster, so a source only ever calls these two methods.
/// Both are safe to call from a non-async thread, and both drop quietly once
/// the receiver is gone at shutdown.
#[derive(Clone)]
pub struct SourceOutput {
    source: SourceId,
    events: UnboundedSender<HubEvent>,
    /// Unbounded because the source paces itself to real time: the queue
    /// only grows while the broadcaster stalls, and a source never blocks
    /// or loses audio on a momentary stall.
    pcm: UnboundedSender<Vec<i16>>,
}

impl SourceOutput {
    #[must_use]
    pub fn new(
        source: SourceId,
        events: UnboundedSender<HubEvent>,
        pcm: UnboundedSender<Vec<i16>>,
    ) -> Self {
        Self {
            source,
            events,
            pcm,
        }
    }

    /// Interleaved samples in the format of the last [`SourceEvent::Started`].
    /// Call at the rate the audio plays: the broadcaster stamps each chunk's
    /// play time on arrival.
    pub fn pcm(&self, samples: &[i16]) {
        let _ = self.pcm.send(samples.to_vec());
    }

    pub fn event(&self, event: SourceEvent) {
        let _ = self.events.send(HubEvent::Source {
            source: self.source,
            event,
        });
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
