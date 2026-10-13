use std::net::SocketAddr;
use std::path::PathBuf;

use bounce_common::ids::ClientId;

pub struct Config {
    /// `--client-id <32 hex>`, otherwise `/etc/machine-id`.
    pub client_id: ClientId,
    /// `--server <host:port>`, which skips mDNS discovery.
    pub server: Option<SocketAddr>,
    /// `--data-dir <path>`, default `/var/lib/bounce`.
    pub data_dir: PathBuf,
    pub output: OutputBackend,
}

/// `--output alsa` (default) or `--output wav:<dir>`.
pub enum OutputBackend {
    /// Advertises the device's ALSA playback devices in `Hello`.
    Alsa,
    /// Advertises one output, `wav`, and writes what would have reached
    /// the DAC into `dir`. See [`crate::player::WavOutput`].
    Wav { dir: PathBuf },
}

#[derive(Debug)]
pub enum ConfigError {
    UnknownFlag(String),
    MissingValue(&'static str),
    Invalid { flag: &'static str, value: String },
    MachineId(std::io::Error),
}

impl Config {
    /// # Errors
    ///
    /// On an unknown flag, a malformed value, or an unreadable
    /// `/etc/machine-id` when `--client-id` is absent.
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self, ConfigError> {
        todo!()
    }
}
