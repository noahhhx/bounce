use serde::{Deserialize, Serialize};

/// Read from `/etc/machine-id` on the device, so it survives reinstalls of
/// the client binary and needs no provisioning step.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientId(String);

impl ClientId {
    /// Accepts the 32 lowercase hex characters `/etc/machine-id` contains.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        todo!()
    }
}

/// Changes on every client process start. Lets the server tell a reconnect
/// from a restart, and scopes log sequence numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BootId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ZoneId(pub u32);

/// One running cast target, such as the Spotify Connect device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(pub u32);

/// One continuous broadcast from one source. A new track on the same source
/// keeps the stream; switching source or format starts a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}
