//! Spotify Connect via librespot. librespot's `Sink` trait receives decoded
//! packets; our sink forwards them to [`SourceOutput::pcm`]. Its player
//! events map onto [`SourceEvent`].

use std::path::PathBuf;

use super::{Source, SourceControl, SourceKind, SourceOutput};

pub struct SpotifyConfig {
    /// The name shown in the Spotify app's device picker. One device for the
    /// whole house, e.g. "Bounce".
    pub device_name: String,
    pub bitrate: Bitrate,
    /// Credentials and audio cache. Persisted so a restart doesn't need the
    /// phone to re-pair.
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum Bitrate {
    Kbps96,
    Kbps160,
    Kbps320,
}

pub struct SpotifySource {
    config: SpotifyConfig,
}

impl SpotifySource {
    #[must_use]
    pub fn new(config: SpotifyConfig) -> Self {
        Self { config }
    }
}

impl Source for SpotifySource {
    fn kind(&self) -> SourceKind {
        SourceKind::Spotify
    }

    fn start(&mut self, out: SourceOutput) {
        todo!("spawn librespot discovery + Spirc with a Sink that forwards to `out`")
    }

    fn control(&mut self, control: SourceControl) {
        todo!("Spirc::set_volume")
    }

    fn stop(&mut self) {
        todo!("Spirc::shutdown")
    }
}
