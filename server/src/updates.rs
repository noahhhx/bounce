//! Client builds the server can push. The server is the update source, so
//! clients never need internet access.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use bounce_common::ids::Version;
use bounce_common::update::{Release, Target};

use crate::fleet::ClientRecord;

pub struct ReleaseCatalog {
    releases: HashMap<Target, BTreeMap<Version, Release>>,
    /// Served over HTTP at each release's `artifact_path`.
    artifact_dir: PathBuf,
}

impl ReleaseCatalog {
    #[must_use]
    pub fn new(artifact_dir: PathBuf) -> Self {
        Self {
            releases: HashMap::new(),
            artifact_dir,
        }
    }

    /// Copies the artifact into `artifact_dir` and checks its hash before
    /// the release becomes visible, so no client is ever offered a build
    /// the server can't serve.
    ///
    /// # Errors
    ///
    /// Leaves the catalog unchanged on any [`PublishError`].
    pub fn publish(&mut self, release: Release, artifact: PathBuf) -> Result<(), PublishError> {
        todo!()
    }

    /// The pinned version if set, otherwise the newest release for the
    /// client's target. Skips the client's bad releases in both cases.
    #[must_use]
    pub fn release_for(&self, client: &ClientRecord) -> Option<&Release> {
        let releases = self.releases.get(&client.hello.target)?;
        let usable = |release: &&Release| !client.bad_releases.contains(&release.version);
        match client.release_pin {
            Some(pin) => releases.get(&pin).filter(usable),
            None => releases.values().rev().find(usable),
        }
    }
}

#[derive(Debug)]
pub enum PublishError {
    HashMismatch,
    AlreadyPublished,
    Io(std::io::Error),
}
