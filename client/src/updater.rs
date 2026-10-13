//! Converges the installed binary on the desired [`Release`], with
//! automatic rollback. A bad build must never brick a speaker that's
//! screwed into a wall.
//!
//! On-disk layout under `root`:
//!
//! ```text
//! releases/<version>/bounce-client
//! current   -> releases/<version>    (systemd runs this)
//! previous  -> releases/<version>
//! probation                          (exists only while a new version is unproven)
//! ```

use std::path::PathBuf;
use std::time::Duration;

use bounce_common::ids::Version;
use bounce_common::update::{Release, UpdateState};

pub struct Updater {
    root: PathBuf,
    running: Version,
    state: UpdateState,
    /// The server's HTTP base. Release artifacts are fetched from here.
    server: String,
}

/// Written when switching to a new version. Each start increments
/// `attempts`, and [`Updater::confirm_healthy`] deletes the file.
pub struct Probation {
    pub version: Version,
    pub previous: Version,
    pub attempts: u32,
}

/// The first thing `main` checks, before connecting.
pub enum StartupOutcome {
    Normal,
    FirstBootAfterUpdate {
        from: Version,
    },
    /// Probation ran out of attempts, so `current` now points back at
    /// `previous`. The caller restarts into it and reports
    /// `FaultKind::UpdateRolledBack`.
    RolledBack {
        from: Version,
        to: Version,
    },
}

pub const MAX_PROBATION_ATTEMPTS: u32 = 3;

/// How long a new version must stay connected and playing-capable before
/// it's confirmed. systemd's `WatchdogSec` covers a build that hangs instead
/// of crashing.
pub const PROBATION_PERIOD: Duration = Duration::from_secs(120);

impl Updater {
    #[must_use]
    pub fn on_startup(root: PathBuf) -> StartupOutcome {
        todo!()
    }

    /// Idempotent: called on every desired-state change. Starts a download
    /// if the desired release differs from the running one and nothing is
    /// staged for it yet. `None` means stay put.
    pub fn converge(&mut self, desired: Option<&Release>) -> &UpdateState {
        todo!()
    }

    /// Flips `current`, writes `probation`, and exits for systemd to
    /// restart. The caller only invokes this while nothing is playing.
    pub fn apply_staged(&mut self) -> ! {
        todo!()
    }

    /// Called once the new version has held a session for
    /// [`PROBATION_PERIOD`].
    pub fn confirm_healthy(&mut self) {
        todo!()
    }
}
