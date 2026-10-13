//! Messages on the session between one client and the server.
//!
//! Each client holds one long-lived, ordered, reliable connection. Audio,
//! control, and telemetry share it. Liveness falls out of the connection
//! plus the metrics heartbeat, with no separate agent to deploy.
//!
//! Server-to-client control is desired state, not commands: the server
//! sends the whole [`DesiredState`] and the client converges on it. Resending
//! it after a reconnect is always safe, and a client that missed messages
//! while offline needs nothing replayed.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio::{AudioChunk, AudioFormat, Codec};
use crate::ids::{BootId, ClientId, StreamId, Version};
use crate::telemetry::{Fault, Level, LogBatch, LogSeq, MetricsSample};
use crate::time::{ServerInstant, SinceBoot};
use crate::update::{Release, Target, UpdateState};
use crate::zone::Volume;

/// [`Hello`], [`Rejected`] and [`Release`] must stay decodable by every
/// shipped client. They are the path an outdated client uses to update
/// itself, so breaking them strands devices that are hard to reach.
pub const PROTOCOL_VERSION: u16 = 1;

/// How long either end waits without hearing anything before it treats
/// the session as dead. A Wi-Fi drop sends no FIN, so the socket alone
/// cannot tell.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// The mDNS service the server advertises and clients browse for. The SRV
/// record carries the port.
pub const MDNS_SERVICE: &str = "_bounce._tcp.local.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Hello(Hello),
    TimeSync(TimeSyncRequest),
    Reported(ReportedState),
    Metrics(MetricsSample),
    Logs(LogBatch),
    Fault(Fault),
    Diagnostics(Diagnostics),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Rejected(Rejected),
    Desired(DesiredState),
    TimeSync(TimeSyncReply),
    Audio(AudioChunk),
    LogsAck { boot: BootId, up_to: LogSeq },
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub client: ClientId,
    pub boot: BootId,
    pub version: Version,
    pub target: Target,
    pub hostname: String,
    pub outputs: Vec<OutputDevice>,
    pub codecs: Vec<Codec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputDevice {
    /// ALSA device string, e.g. `hw:CARD=sndrpihifiberry`.
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejected {
    pub reason: RejectReason,
    /// Set when installing this release would let the server accept the
    /// client.
    pub upgrade: Option<Release>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RejectReason {
    ProtocolTooOld { server: u16 },
    ProtocolTooNew { server: u16 },
    DuplicateClientId,
}

/// Everything the server wants a client to be doing. Zones do not appear:
/// the server resolves zone membership and zone volume into the fields
/// below, so the client never needs to know zones exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesiredState {
    pub revision: Revision,
    pub name: String,
    pub output: String,
    pub volume: Volume,
    pub muted: bool,
    /// Measured DAC and amp delay for this device, subtracted from each
    /// chunk's deadline so rooms line up.
    pub output_latency: Duration,
    pub log_level: Level,
    pub playback: Playback,
    /// `None` when no build for this client's target has been published, so
    /// the client keeps running its current version.
    pub release: Option<Release>,
}

/// Increases on every change to a client's [`DesiredState`]. The client
/// echoes the last revision it fully applied in [`ReportedState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Revision(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Playback {
    Idle,
    Playing {
        stream: StreamId,
        format: AudioFormat,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportedState {
    pub applied: Revision,
    pub player: PlayerState,
    pub update: UpdateState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerState {
    Idle,
    Buffering { stream: StreamId },
    Playing { stream: StreamId },
    OutputUnavailable { device: String },
}

/// Imperatives that have no meaning as state. Anything that should still
/// hold after a reconnect belongs in [`DesiredState`] instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    RestartProcess,
    RebootDevice,
    /// Play a short chime so a person can find which speaker this is.
    Identify,
    CollectDiagnostics {
        request: u64,
    },
}

/// The answer to [`Command::CollectDiagnostics`]: output the server cannot
/// see through metrics, such as `dmesg`, `aplay -l`, `vcgencmd get_throttled`,
/// and the journal tail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    pub request: u64,
    pub sections: Vec<(String, String)>,
}

/// One NTP-style round trip. The client computes offset and round-trip time
/// from its own send and receive times plus the two server timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncRequest {
    pub client_sent: SinceBoot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncReply {
    pub client_sent: SinceBoot,
    pub server_received: ServerInstant,
    pub server_sent: ServerInstant,
}
