//! The single owner of server state, written as a reducer: an event in,
//! effects out, no IO. The caller supplies `now`, so health timeouts and
//! the whole client lifecycle can be tested with a fake clock.

use std::collections::HashMap;
use std::time::Instant;

use serde::Deserialize;

use bounce_common::audio::AudioFormat;
use bounce_common::ids::{ClientId, SourceId, StreamId, Version, ZoneId};
use bounce_common::protocol::{ClientMessage, Command, DesiredState, Hello, ServerMessage};
use bounce_common::update::Release;
use bounce_common::zone::{Volume, Zone};

use crate::fleet::{ClientRecord, Fleet};
use crate::health::{Alert, AlertKey, Thresholds};
use crate::http::StateView;
use crate::source::{SourceControl, SourceEvent, SourceKind, TrackMetadata};
use crate::updates::ReleaseCatalog;

pub struct Hub {
    fleet: Fleet,
    zones: HashMap<ZoneId, Zone>,
    sources: HashMap<SourceId, SourceState>,
    catalog: ReleaseCatalog,
    thresholds: Thresholds,
    alerts: HashMap<AlertKey, Alert>,
}

/// What the hub knows about a running source. The [`crate::source::Source`]
/// itself runs in its own task.
pub struct SourceState {
    pub kind: SourceKind,
    pub name: String,
    /// The casting app's volume slider. A client's output volume is this
    /// scaled by its zone's volume.
    pub volume: Volume,
    pub playback: SourcePlayback,
}

pub enum SourcePlayback {
    Idle,
    Active {
        stream: StreamId,
        format: AudioFormat,
        paused: bool,
        track: Option<TrackMetadata>,
    },
}

pub enum HubEvent {
    Connected {
        hello: Hello,
    },
    /// Everything except logs, which go to the telemetry store directly.
    Message {
        client: ClientId,
        message: ClientMessage,
    },
    Disconnected {
        client: ClientId,
    },
    /// Fires every second. Drives heartbeat timeouts and time-based health
    /// rules, which no client message would otherwise trigger.
    Tick,
    Source {
        source: SourceId,
        event: SourceEvent,
    },
    Admin(AdminRequest),
}

/// What the web UI can ask for.
#[derive(Debug, Deserialize)]
pub enum AdminRequest {
    UpsertZone {
        zone: ZoneId,
        name: String,
    },
    MoveClient {
        client: ClientId,
        zone: Option<ZoneId>,
    },
    /// Which zones hear the Spotify device is chosen here, not in Spotify.
    RouteZone {
        zone: ZoneId,
        source: Option<SourceId>,
    },
    SetZoneVolume {
        zone: ZoneId,
        volume: Volume,
    },
    SetSourceVolume {
        source: SourceId,
        volume: Volume,
    },
    RenameClient {
        client: ClientId,
        name: String,
    },
    PublishRelease(Release),
    /// `None` tracks the latest release for the client's target.
    PinRelease {
        client: ClientId,
        version: Option<Version>,
    },
    SendCommand {
        client: ClientId,
        command: Command,
    },
}

pub enum Effect {
    Send {
        to: ClientId,
        message: ServerMessage,
    },
    /// Sent after a `Rejected`, or when a heartbeat times out on a socket
    /// that still looks open.
    Close {
        client: ClientId,
    },
    Subscribe {
        source: SourceId,
        client: ClientId,
    },
    Unsubscribe {
        source: SourceId,
        client: ClientId,
    },
    ControlSource {
        source: SourceId,
        control: SourceControl,
    },
    AlertRaised(Alert),
    AlertCleared(AlertKey),
    /// For [`crate::http`] to serve as `/api/state`.
    StateChanged(StateView),
}

impl Hub {
    pub fn handle(&mut self, event: HubEvent, now: Instant) -> Vec<Effect> {
        todo!()
    }

    fn view(&self) -> StateView {
        todo!()
    }

    /// The one place zone membership, zone routing, source and zone volume,
    /// the release catalog and per-client settings combine into what a
    /// client should be doing. The caller sends it, with a bumped revision,
    /// only if it differs from the record's current desired state.
    fn desired_state(&self, record: &ClientRecord) -> DesiredState {
        todo!()
    }

    /// Runs the health rules for one client and diffs against `alerts`.
    fn reevaluate(&mut self, client: &ClientId, now: Instant) -> Vec<Effect> {
        todo!()
    }
}
