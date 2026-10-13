use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::source::spotify::{Bitrate, SpotifyConfig};
use crate::source::test_tone::TestToneConfig;
use crate::source::SourceConfig;

pub struct Config {
    /// `--listen <addr>` for client sessions, default `0.0.0.0:7700`. Port 0
    /// picks a free port.
    pub listen: SocketAddr,
    /// `--http <addr>` for release artifacts and the API in
    /// [`crate::http`], default `0.0.0.0:7701`. Port 0 picks a free port.
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
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Result<Self, ConfigError> {
        let mut listen = SocketAddr::from(([0, 0, 0, 0], 7700));
        let mut http = SocketAddr::from(([0, 0, 0, 0], 7701));
        let mut data_dir = PathBuf::from("/var/lib/bounce");
        let mut advertise = true;
        let mut sources = Vec::new();

        while let Some(arg) = args.next() {
            let mut value = |flag| args.next().ok_or(ConfigError::MissingValue(flag));
            match arg.as_str() {
                "--listen" => listen = parse_addr("--listen", value("--listen")?)?,
                "--http" => http = parse_addr("--http", value("--http")?)?,
                "--data-dir" => data_dir = value("--data-dir")?.into(),
                "--no-mdns" => advertise = false,
                "--source" => sources.push(value("--source")?),
                _ => return Err(ConfigError::UnknownFlag(arg)),
            }
        }
        if sources.is_empty() {
            sources.push("spotify:Bounce".to_owned());
        }
        let sources = sources
            .into_iter()
            .map(|spec| parse_source(spec, &data_dir))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            listen,
            http,
            data_dir,
            advertise,
            sources,
        })
    }
}

fn parse_addr(flag: &'static str, value: String) -> Result<SocketAddr, ConfigError> {
    value
        .parse()
        .map_err(|_| ConfigError::Invalid { flag, value })
}

/// Runs after every flag is read, so `--data-dir` may follow `--source`.
fn parse_source(spec: String, data_dir: &Path) -> Result<SourceConfig, ConfigError> {
    match spec.split_once(':') {
        Some(("spotify", name)) if !name.is_empty() => Ok(SourceConfig::Spotify(SpotifyConfig {
            device_name: name.to_owned(),
            bitrate: Bitrate::Kbps320,
            cache_dir: data_dir.join("spotify"),
        })),
        None if spec == "test-tone" => Ok(SourceConfig::TestTone(TestToneConfig {
            name: "Test tone".to_owned(),
            frequency_hz: 440,
            sample_rate: 48_000,
        })),
        _ => Err(ConfigError::Invalid {
            flag: "--source",
            value: spec,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config, ConfigError> {
        Config::from_args(args.iter().map(|&a| a.to_owned()))
    }

    fn spotify(source: &SourceConfig) -> &SpotifyConfig {
        match source {
            SourceConfig::Spotify(config) => config,
            SourceConfig::TestTone(_) => panic!("expected a Spotify source"),
        }
    }

    #[test]
    fn defaults_to_one_spotify_device_named_bounce() {
        let config = parse(&[]).unwrap();
        assert_eq!(config.sources.len(), 1);
        let source = spotify(&config.sources[0]);
        assert_eq!(source.device_name, "Bounce");
        assert_eq!(source.cache_dir, Path::new("/var/lib/bounce/spotify"));
    }

    #[test]
    fn spotify_cache_follows_data_dir_given_after_source() {
        let config = parse(&["--source", "spotify:Den", "--data-dir", "/tmp/b"]).unwrap();
        let source = spotify(&config.sources[0]);
        assert_eq!(source.device_name, "Den");
        assert_eq!(source.cache_dir, Path::new("/tmp/b/spotify"));
    }

    #[test]
    fn sources_keep_flag_order() {
        let config = parse(&["--source", "test-tone", "--source", "spotify:Den"]).unwrap();
        assert!(matches!(config.sources[0], SourceConfig::TestTone(_)));
        assert_eq!(spotify(&config.sources[1]).device_name, "Den");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(parse(&["--port"]), Err(ConfigError::UnknownFlag(f)) if f == "--port"));
        assert!(matches!(
            parse(&["--listen"]),
            Err(ConfigError::MissingValue("--listen"))
        ));
        assert!(matches!(
            parse(&["--source", "spotify:"]),
            Err(ConfigError::Invalid {
                flag: "--source",
                ..
            })
        ));
    }
}
