use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use bounce_common::ids::{ClientId, Version, ZoneId};
use bounce_common::protocol::{DesiredState, Hello};
use bounce_common::zone::Zone;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub clients: BTreeMap<ClientId, SavedClient>,
    pub zones: BTreeMap<ZoneId, Zone>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedClient {
    pub hello: Hello,
    pub name: Option<String>,
    pub zone: Option<ZoneId>,
    pub desired: DesiredState,
    pub release_pin: Option<Version>,
    pub bad_releases: BTreeSet<Version>,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(f, "not a saved fleet: {error}"),
        }
    }
}

/// # Errors
///
/// When the file exists but cannot be read or parsed.
pub fn load(path: &Path) -> Result<Snapshot, LoadError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(LoadError::Parse),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Snapshot::default()),
        Err(error) => Err(LoadError::Io(error)),
    }
}

/// # Errors
///
/// On any write, sync or rename failure. `path` is then unchanged.
pub fn save(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(snapshot).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&json)?;
    file.sync_all()?;
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use bounce_common::audio::Codec;
    use bounce_common::ids::BootId;
    use bounce_common::protocol::{OutputDevice, Playback, Revision, PROTOCOL_VERSION};
    use bounce_common::telemetry::Level;
    use bounce_common::update::Target;
    use bounce_common::zone::Volume;

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bounce-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn snapshot() -> Snapshot {
        let id = ClientId::parse("b0c00000000000000000000000000001").unwrap();
        let version = Version {
            major: 0,
            minor: 1,
            patch: 0,
        };
        let client = SavedClient {
            hello: Hello {
                protocol: PROTOCOL_VERSION,
                client: id.clone(),
                boot: BootId(7),
                version,
                target: Target::Aarch64,
                hostname: "pi".to_owned(),
                outputs: vec![OutputDevice {
                    id: "hw:0".to_owned(),
                    description: "DAC".to_owned(),
                }],
                codecs: vec![Codec::PcmS16Le],
            },
            name: Some("Kitchen left".to_owned()),
            zone: Some(ZoneId(3)),
            desired: DesiredState {
                revision: Revision(12),
                name: "Kitchen left".to_owned(),
                output: "hw:0".to_owned(),
                volume: Volume::new(40),
                muted: false,
                output_latency: Duration::from_millis(15),
                log_level: Level::Info,
                playback: Playback::Idle,
                release: None,
            },
            release_pin: Some(version),
            bad_releases: BTreeSet::from([version]),
        };
        let zone = Zone {
            id: ZoneId(3),
            name: "Kitchen".to_owned(),
            members: BTreeSet::from([id.clone()]),
            volume: Volume::new(50),
            source: None,
        };
        Snapshot {
            clients: BTreeMap::from([(id, client)]),
            zones: BTreeMap::from([(ZoneId(3), zone)]),
        }
    }

    #[test]
    fn a_saved_snapshot_loads_back_equal() {
        let path = dir("snapshot-round-trip").join("fleet.json");
        save(&path, &snapshot()).unwrap();
        assert_eq!(load(&path).unwrap(), snapshot());
    }

    #[test]
    fn a_missing_file_is_an_empty_fleet() {
        let path = dir("snapshot-missing").join("fleet.json");
        assert_eq!(load(&path).unwrap(), Snapshot::default());
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_an_empty_fleet() {
        let path = dir("snapshot-corrupt").join("fleet.json");
        fs::write(&path, "{\"clients\":").unwrap();
        assert!(matches!(load(&path), Err(LoadError::Parse(_))));
    }
}
