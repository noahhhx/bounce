use std::net::SocketAddr;
use std::path::PathBuf;

use bounce_common::ids::ClientId;

pub struct Config {
    /// `--client-id <32 hex>`, otherwise `/etc/machine-id`.
    pub client_id: ClientId,
    pub server: Server,
    /// `--data-dir <path>`, default `/var/lib/bounce`.
    pub data_dir: PathBuf,
    pub output: OutputBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    Fixed(SocketAddr),
    Discover,
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
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Result<Self, ConfigError> {
        let mut client_id = None;
        let mut server = Server::Discover;
        let mut data_dir = PathBuf::from("/var/lib/bounce");
        let mut output = OutputBackend::Alsa;

        while let Some(arg) = args.next() {
            let mut value = |flag| args.next().ok_or(ConfigError::MissingValue(flag));
            match arg.as_str() {
                "--client-id" => client_id = Some(value("--client-id")?),
                "--server" => server = Server::Fixed(parse_addr("--server", value("--server")?)?),
                "--data-dir" => data_dir = value("--data-dir")?.into(),
                "--output" => output = parse_output(value("--output")?)?,
                _ => return Err(ConfigError::UnknownFlag(arg)),
            }
        }
        let client_id = if let Some(raw) = client_id {
            parse_client_id("--client-id", raw)?
        } else {
            let raw = std::fs::read_to_string(MACHINE_ID).map_err(ConfigError::MachineId)?;
            parse_client_id(MACHINE_ID, raw.trim().to_owned())?
        };

        Ok(Self {
            client_id,
            server,
            data_dir,
            output,
        })
    }
}

const MACHINE_ID: &str = "/etc/machine-id";

fn parse_client_id(flag: &'static str, value: String) -> Result<ClientId, ConfigError> {
    ClientId::parse(&value).ok_or(ConfigError::Invalid { flag, value })
}

fn parse_addr(flag: &'static str, value: String) -> Result<SocketAddr, ConfigError> {
    value
        .parse()
        .map_err(|_| ConfigError::Invalid { flag, value })
}

fn parse_output(spec: String) -> Result<OutputBackend, ConfigError> {
    match spec.split_once(':') {
        None if spec == "alsa" => Ok(OutputBackend::Alsa),
        Some(("wav", dir)) if !dir.is_empty() => Ok(OutputBackend::Wav { dir: dir.into() }),
        _ => Err(ConfigError::Invalid {
            flag: "--output",
            value: spec,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const ID: &str = "b0c00000000000000000000000000001";

    fn parse(args: &[&str]) -> Result<Config, ConfigError> {
        Config::from_args(args.iter().map(|&a| a.to_owned()))
    }

    #[test]
    fn reads_every_flag() {
        let config = parse(&[
            "--client-id",
            ID,
            "--server",
            "127.0.0.1:7700",
            "--data-dir",
            "/tmp/c1",
            "--output",
            "wav:/tmp/c1/wav",
        ])
        .unwrap();
        assert_eq!(config.client_id, ClientId::parse(ID).unwrap());
        assert_eq!(
            config.server,
            Server::Fixed(SocketAddr::from(([127, 0, 0, 1], 7700)))
        );
        assert_eq!(config.data_dir, Path::new("/tmp/c1"));
        assert!(
            matches!(config.output, OutputBackend::Wav { dir } if dir == Path::new("/tmp/c1/wav"))
        );
    }

    #[test]
    fn defaults_to_alsa_var_lib_and_discovery() {
        let config = parse(&["--client-id", ID]).unwrap();
        assert_eq!(config.server, Server::Discover);
        assert_eq!(config.data_dir, Path::new("/var/lib/bounce"));
        assert!(matches!(config.output, OutputBackend::Alsa));
        let explicit = parse(&["--client-id", ID, "--output", "alsa"]).unwrap();
        assert!(matches!(explicit.output, OutputBackend::Alsa));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(parse(&["--port"]), Err(ConfigError::UnknownFlag(f)) if f == "--port"));
        assert!(matches!(
            parse(&["--client-id", ID, "--server"]),
            Err(ConfigError::MissingValue("--server"))
        ));
        assert!(matches!(
            parse(&["--client-id", "B0C00000000000000000000000000001"]),
            Err(ConfigError::Invalid {
                flag: "--client-id",
                ..
            })
        ));
        assert!(matches!(
            parse(&["--client-id", ID, "--server", "localhost"]),
            Err(ConfigError::Invalid {
                flag: "--server",
                ..
            })
        ));
        assert!(matches!(
            parse(&["--client-id", ID, "--output", "wav:"]),
            Err(ConfigError::Invalid {
                flag: "--output",
                ..
            })
        ));
    }
}
