use serde::{Deserialize, Serialize};

use crate::ids::Version;

/// A client build the server can hand out. The artifact is fetched over the
/// server's HTTP endpoint, not the session, so a 10 MB download never
/// competes with audio chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    pub target: Target,
    pub artifact_path: String,
    pub sha256: [u8; 32],
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    /// Pi Zero, Pi 1.
    ArmV6,
    /// Pi Zero 2, Pi 3 and later on 32-bit OS.
    ArmV7,
    Aarch64,
    X86_64,
}

/// Client-side progress toward the release in its desired state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UpdateState {
    UpToDate,
    Downloading {
        received_bytes: u64,
        total_bytes: u64,
    },
    /// Verified and written beside the running binary. Applied on the next
    /// restart, which the client schedules when nothing is playing.
    Staged {
        version: Version,
    },
    Failed {
        version: Version,
        reason: String,
    },
}
