use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{ClientId, SourceId, ZoneId};

/// A set of clients with one volume and one source. A client is in at most
/// one zone; the server's zone table enforces that, not this type. Zones
/// routed to the same source play in sync with each other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub id: ZoneId,
    pub name: String,
    pub members: BTreeSet<ClientId>,
    pub volume: Volume,
    /// Chosen in the web UI. `None` is silent.
    pub source: Option<SourceId>,
}

/// 0 to 100, clamped at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u8")]
pub struct Volume(u8);

impl Volume {
    #[must_use]
    pub fn new(percent: u8) -> Self {
        Self(percent.min(100))
    }

    #[must_use]
    pub fn percent(self) -> u8 {
        self.0
    }
}

impl From<u8> for Volume {
    fn from(percent: u8) -> Self {
        Self::new(percent)
    }
}
