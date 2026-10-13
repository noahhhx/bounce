use std::net::SocketAddr;
use std::path::PathBuf;

use crate::source::SourceConfig;

pub struct Config {
    /// `--listen <addr>` for client sessions. Port 0 picks a free port.
    pub listen: SocketAddr,
    /// `--http <addr>` for release artifacts and the API in
    /// [`crate::http`]. Port 0 picks a free port.
    pub http: SocketAddr,
    /// `--data-dir <path>`, default `/var/lib/bounce`.
    pub data_dir: PathBuf,
    /// Off with `--no-mdns`.
    pub advertise: bool,
    /// `--source spotify:<device name>` or `--source test-tone`, repeatable,
    /// default `spotify:Bounce`. [`bounce_common::ids::SourceId`]s follow
    /// flag order from 0.
    pub sources: Vec<SourceConfig>,
}

/// Printed to stdout once both listeners are bound, as
/// `bounce-server ready session=<addr> http=<addr>`. With port 0 this is the
/// only way to learn the ports, and scripts wait for it as the ready signal.
pub const READY_PREFIX: &str = "bounce-server ready";

#[derive(Debug)]
pub enum ConfigError {
    UnknownFlag(String),
    MissingValue(&'static str),
    Invalid { flag: &'static str, value: String },
}

impl Config {
    /// # Errors
    ///
    /// On an unknown flag or a malformed value.
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self, ConfigError> {
        todo!()
    }
}
