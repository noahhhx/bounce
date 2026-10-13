//! The single owner of server state, written as a reducer: an event in,
//! effects out, no IO. The caller supplies `now`, so health timeouts and
//! the whole client lifecycle can be tested with a fake clock.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use log::warn;
use serde::Deserialize;

use bounce_common::audio::AudioFormat;
use bounce_common::ids::{ClientId, SourceId, StreamId, Version, ZoneId};
use bounce_common::protocol::{
    ClientMessage, Command, DesiredState, Hello, Playback, Revision, ServerMessage,
};
use bounce_common::telemetry::Level;
use bounce_common::update::Release;
use bounce_common::zone::{Volume, Zone};

use crate::fleet::{ClientRecord, Fleet, Liveness, OfflineCause};
use crate::health::{Alert, AlertKey, Thresholds};
use crate::http::{ClientView, SourceView, StateView};
use crate::source::{SourceControl, SourceEvent, SourceKind, TrackMetadata};
use crate::updates::ReleaseCatalog;

pub struct Hub {
    fleet: Fleet,
    zones: HashMap<ZoneId, Zone>,
    sources: HashMap<SourceId, SourceState>,
    catalog: ReleaseCatalog,
    thresholds: Thresholds,
    alerts: HashMap<AlertKey, Alert>,
    /// What has been emitted as [`Effect::Subscribe`], so reconciling can
    /// emit only the difference.
    subscriptions: HashMap<ClientId, SourceId>,
}

/// What the hub knows about a running source. The [`crate::source::Source`]
/// itself runs in its own task.
pub struct SourceState {
    pub kind: SourceKind,
    pub name: String,
    /// The casting app's volume slider. A client's output volume is this
    /// scaled by its zone's volume.
    pub volume: Volume,
    /// The source's, not the stream's: a track outlives a pause, and can be
    /// announced before the stream that plays it starts.
    pub track: Option<TrackMetadata>,
    pub playback: SourcePlayback,
}

pub enum SourcePlayback {
    Idle,
    Active {
        stream: StreamId,
        format: AudioFormat,
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

#[derive(Debug)]
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
    /// Sources start idle at full volume.
    #[must_use]
    pub fn new(
        sources: impl IntoIterator<Item = (SourceId, SourceKind, String)>,
        catalog: ReleaseCatalog,
        thresholds: Thresholds,
    ) -> Self {
        let sources = sources
            .into_iter()
            .map(|(id, kind, name)| {
                let state = SourceState {
                    kind,
                    name,
                    volume: Volume::new(100),
                    track: None,
                    playback: SourcePlayback::Idle,
                };
                (id, state)
            })
            .collect();
        Self {
            fleet: Fleet::default(),
            zones: HashMap::new(),
            sources,
            catalog,
            thresholds,
            alerts: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }

    /// Applies the event, then reconciles everything derived from state.
    /// Always ends with [`Effect::StateChanged`].
    pub fn handle(&mut self, event: HubEvent, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        let connected = self.apply(event, now, &mut effects);
        self.reconcile(connected.as_ref(), &mut effects);
        effects.push(Effect::StateChanged(self.view()));
        effects
    }

    /// The event's own mutation, plus effects that are not derived state.
    /// Returns the client that just connected.
    fn apply(
        &mut self,
        event: HubEvent,
        now: Instant,
        effects: &mut Vec<Effect>,
    ) -> Option<ClientId> {
        match event {
            HubEvent::Connected { hello } => {
                let client = hello.client.clone();
                self.fleet.connected(hello, now);
                // The new session has a new outbox, so it needs a Subscribe
                // even if the old session's was never withdrawn.
                self.subscriptions.remove(&client);
                return Some(client);
            }
            HubEvent::Disconnected { client } => {
                if let Some(record) = self.client_mut(&client) {
                    record.liveness = Liveness::Offline {
                        since: now,
                        cause: OfflineCause::ConnectionClosed,
                    };
                }
            }
            HubEvent::Message { client, message } => {
                if let Some(record) = self.client_mut(&client) {
                    if let Liveness::Online { last_heard, .. } = &mut record.liveness {
                        *last_heard = now;
                    }
                    if let ClientMessage::Reported(reported) = message {
                        record.reported = Some(reported);
                    }
                }
            }
            HubEvent::Tick => self.expire_heartbeats(now, effects),
            HubEvent::Source { source, event } => self.source_event(source, event),
            HubEvent::Admin(request) => self.admin(request, effects),
        }
        None
    }

    fn expire_heartbeats(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let timeout = self.thresholds.heartbeat_timeout;
        for (client, record) in self.fleet.iter_mut() {
            if let Liveness::Online { last_heard, .. } = record.liveness {
                if now.saturating_duration_since(last_heard) > timeout {
                    record.liveness = Liveness::Offline {
                        since: last_heard,
                        cause: OfflineCause::HeartbeatTimeout,
                    };
                    effects.push(Effect::Close {
                        client: client.clone(),
                    });
                }
            }
        }
    }

    fn source_event(&mut self, source: SourceId, event: SourceEvent) {
        let Some(state) = self.sources.get_mut(&source) else {
            return warn!("event from unknown source {source:?}");
        };
        match event {
            SourceEvent::Started { stream, format } => {
                state.playback = SourcePlayback::Active { stream, format };
            }
            SourceEvent::Track(track) => state.track = Some(track),
            SourceEvent::Stopped => state.playback = SourcePlayback::Idle,
            SourceEvent::VolumeChanged(volume) => state.volume = volume,
        }
    }

    fn admin(&mut self, request: AdminRequest, effects: &mut Vec<Effect>) {
        match request {
            AdminRequest::UpsertZone { zone, name } => match self.zones.entry(zone) {
                Entry::Occupied(mut entry) => entry.get_mut().name = name,
                Entry::Vacant(entry) => {
                    entry.insert(Zone {
                        id: zone,
                        name,
                        members: BTreeSet::new(),
                        volume: Volume::new(50),
                        source: None,
                    });
                }
            },
            AdminRequest::MoveClient { client, zone } => self.move_client(&client, zone),
            AdminRequest::RouteZone { zone, source } => {
                if let Some(source) = source.filter(|s| !self.sources.contains_key(s)) {
                    return warn!("route to unknown source {source:?}");
                }
                if let Some(zone) = self.zone_mut(zone) {
                    zone.source = source;
                }
            }
            AdminRequest::SetZoneVolume { zone, volume } => {
                if let Some(zone) = self.zone_mut(zone) {
                    zone.volume = volume;
                }
            }
            AdminRequest::SetSourceVolume { source, volume } => {
                let Some(state) = self.sources.get_mut(&source) else {
                    return warn!("volume for unknown source {source:?}");
                };
                state.volume = volume;
                effects.push(Effect::ControlSource {
                    source,
                    control: SourceControl::SetVolume(volume),
                });
            }
            AdminRequest::RenameClient { client, name } => {
                if let Some(record) = self.client_mut(&client) {
                    record.name = Some(name);
                }
            }
            AdminRequest::PinRelease { client, version } => {
                if let Some(record) = self.client_mut(&client) {
                    record.release_pin = version;
                }
            }
            AdminRequest::SendCommand { client, command } => match self.client_mut(&client) {
                Some(record) if record.is_online() => effects.push(Effect::Send {
                    to: client,
                    message: ServerMessage::Command(command),
                }),
                Some(_) => warn!("dropped {command:?} for offline client {client:?}"),
                None => {}
            },
            AdminRequest::PublishRelease(release) => {
                warn!(
                    "publishing releases is not supported yet; ignored {:?}",
                    release.version
                );
            }
        }
    }

    /// The only writer of zone membership, so `record.zone` and every
    /// `Zone.members` agree and a client is in at most one zone.
    fn move_client(&mut self, client: &ClientId, zone: Option<ZoneId>) {
        if let Some(zone) = zone.filter(|z| !self.zones.contains_key(z)) {
            return warn!("move to unknown zone {zone:?}");
        }
        let Some(record) = self.client_mut(client) else {
            return;
        };
        record.zone = zone;
        for z in self.zones.values_mut() {
            if Some(z.id) == zone {
                z.members.insert(client.clone());
            } else {
                z.members.remove(client);
            }
        }
    }

    fn client_mut(&mut self, client: &ClientId) -> Option<&mut ClientRecord> {
        let record = self.fleet.get_mut(client);
        if record.is_none() {
            warn!("unknown client {client:?}");
        }
        record
    }

    fn zone_mut(&mut self, zone: ZoneId) -> Option<&mut Zone> {
        let found = self.zones.get_mut(&zone);
        if found.is_none() {
            warn!("unknown zone {zone:?}");
        }
        found
    }

    /// Derives every client's desired state and subscription from current
    /// state and emits only what changed, so no event handler needs to know
    /// which clients it affects. The client in `connected` hears its desired
    /// state even when unchanged, because a new session starts knowing
    /// nothing.
    fn reconcile(&mut self, connected: Option<&ClientId>, effects: &mut Vec<Effect>) {
        let mut derived: Vec<(ClientId, DesiredState)> = self
            .fleet
            .iter()
            .map(|(id, record)| (id.clone(), self.desired_state(record)))
            .collect();
        derived.sort_by(|a, b| a.0.cmp(&b.0));
        for (client, desired) in derived {
            let Some(record) = self.fleet.get_mut(&client) else {
                continue;
            };
            // Revision 0 is the placeholder from a first connect, never sent.
            let changed = desired != record.desired || record.desired.revision == Revision(0);
            if changed {
                record.desired = DesiredState {
                    revision: Revision(record.desired.revision.0 + 1),
                    ..desired
                };
            }
            if record.is_online() && (changed || connected == Some(&client)) {
                effects.push(Effect::Send {
                    to: client,
                    message: ServerMessage::Desired(record.desired.clone()),
                });
            }
        }

        let target: HashMap<ClientId, SourceId> = self
            .fleet
            .iter()
            .filter(|(_, record)| record.is_online())
            .filter_map(|(id, record)| {
                let source = self.zones.get(&record.zone?)?.source?;
                Some((id.clone(), source))
            })
            .collect();
        let mut clients: Vec<&ClientId> = self.subscriptions.keys().chain(target.keys()).collect();
        clients.sort();
        clients.dedup();
        for client in clients {
            let (was, now) = (self.subscriptions.get(client), target.get(client));
            if was == now {
                continue;
            }
            if let Some(&source) = was {
                effects.push(Effect::Unsubscribe {
                    source,
                    client: client.clone(),
                });
            }
            if let Some(&source) = now {
                effects.push(Effect::Subscribe {
                    source,
                    client: client.clone(),
                });
            }
        }
        self.subscriptions = target;
    }

    /// Clients, zones and sources, each sorted by id.
    #[must_use]
    pub fn view(&self) -> StateView {
        let mut clients: Vec<ClientView> = self
            .fleet
            .iter()
            .map(|(id, record)| ClientView {
                id: id.clone(),
                hostname: record.hello.hostname.clone(),
                version: record.hello.version,
                online: record.is_online(),
                zone: record.zone,
                desired: record.desired.clone(),
                reported: record.reported.clone(),
            })
            .collect();
        clients.sort_by(|a, b| a.id.cmp(&b.id));
        let mut zones: Vec<Zone> = self.zones.values().cloned().collect();
        zones.sort_by_key(|z| z.id);
        let mut sources: Vec<SourceView> = self
            .sources
            .iter()
            .map(|(&id, state)| SourceView {
                id,
                kind: state.kind,
                name: state.name.clone(),
                volume: state.volume,
                stream: match state.playback {
                    SourcePlayback::Active { stream, .. } => Some(stream),
                    SourcePlayback::Idle => None,
                },
            })
            .collect();
        sources.sort_by_key(|s| s.id);
        StateView {
            clients,
            zones,
            sources,
        }
    }

    /// The one place zone membership, zone routing, source and zone volume,
    /// the release catalog and per-client settings combine into what a
    /// client should be doing. The caller sends it, with a bumped revision,
    /// only if it differs from the record's current desired state. It
    /// carries the record's current revision so that comparison is equal
    /// when nothing else changed.
    fn desired_state(&self, record: &ClientRecord) -> DesiredState {
        let zone = record.zone.and_then(|id| self.zones.get(&id));
        let source = zone
            .and_then(|z| z.source)
            .and_then(|id| self.sources.get(&id));
        let volume = match (zone, source) {
            (Some(zone), Some(source)) => {
                u16::from(source.volume.percent()) * u16::from(zone.volume.percent()) / 100
            }
            _ => 0,
        };
        let playback = match source.map(|s| &s.playback) {
            Some(&SourcePlayback::Active { stream, format }) => {
                Playback::Playing { stream, format }
            }
            _ => Playback::Idle,
        };
        DesiredState {
            revision: record.desired.revision,
            name: record
                .name
                .clone()
                .unwrap_or_else(|| record.hello.hostname.clone()),
            output: record
                .hello
                .outputs
                .first()
                .map(|o| o.id.clone())
                .unwrap_or_default(),
            volume: Volume::new(u8::try_from(volume).unwrap_or(u8::MAX)),
            muted: false,
            output_latency: Duration::ZERO,
            log_level: Level::Info,
            playback,
            release: self.catalog.release_for(record).cloned(),
        }
    }

    /// Runs the health rules for one client and diffs against `alerts`.
    fn reevaluate(&mut self, client: &ClientId, now: Instant) -> Vec<Effect> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bounce_common::audio::Codec;
    use bounce_common::ids::BootId;
    use bounce_common::protocol::{OutputDevice, PlayerState, ReportedState, PROTOCOL_VERSION};
    use bounce_common::update::{Target, UpdateState};

    use super::*;

    const TONE: SourceId = SourceId(0);
    const KITCHEN: ZoneId = ZoneId(1);
    const LOUNGE: ZoneId = ZoneId(2);
    const FORMAT: AudioFormat = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
        codec: Codec::PcmS16Le,
    };

    fn hub() -> Hub {
        Hub::new(
            [(TONE, SourceKind::TestTone, "Tone".to_owned())],
            ReleaseCatalog::new(PathBuf::new()),
            Thresholds::default(),
        )
    }

    fn client(n: u8) -> ClientId {
        ClientId::parse(&format!("{n:032x}")).unwrap()
    }

    fn hello(n: u8) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client: client(n),
            boot: BootId(1),
            version: Version {
                major: 0,
                minor: 1,
                patch: 0,
            },
            target: Target::X86_64,
            hostname: format!("pi-{n}"),
            outputs: vec![OutputDevice {
                id: "hw:0".to_owned(),
                description: "DAC".to_owned(),
            }],
            codecs: vec![Codec::PcmS16Le],
        }
    }

    fn secs(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    fn admin(hub: &mut Hub, request: AdminRequest, now: Instant) -> Vec<Effect> {
        hub.handle(HubEvent::Admin(request), now)
    }

    /// Client 1 in the kitchen, routed to the tone.
    fn routed(t0: Instant) -> (Hub, Vec<Effect>) {
        let mut hub = hub();
        hub.handle(HubEvent::Connected { hello: hello(1) }, t0);
        let zone = AdminRequest::UpsertZone {
            zone: KITCHEN,
            name: "Kitchen".to_owned(),
        };
        admin(&mut hub, zone, t0);
        let join = AdminRequest::MoveClient {
            client: client(1),
            zone: Some(KITCHEN),
        };
        admin(&mut hub, join, t0);
        let route = AdminRequest::RouteZone {
            zone: KITCHEN,
            source: Some(TONE),
        };
        let effects = admin(&mut hub, route, t0);
        (hub, effects)
    }

    fn start_tone(hub: &mut Hub, now: Instant) -> Vec<Effect> {
        let started = SourceEvent::Started {
            stream: StreamId(5),
            format: FORMAT,
        };
        hub.handle(
            HubEvent::Source {
                source: TONE,
                event: started,
            },
            now,
        )
    }

    fn desired(effects: &[Effect]) -> Vec<(ClientId, DesiredState)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send {
                    to,
                    message: ServerMessage::Desired(desired),
                } => Some((to.clone(), desired.clone())),
                _ => None,
            })
            .collect()
    }

    /// `(subscribed, source, client)` in emitted order.
    fn subscription_changes(effects: &[Effect]) -> Vec<(bool, SourceId, ClientId)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Subscribe { source, client } => Some((true, *source, client.clone())),
                Effect::Unsubscribe { source, client } => Some((false, *source, client.clone())),
                _ => None,
            })
            .collect()
    }

    fn members(hub: &Hub) -> Vec<(ZoneId, Vec<ClientId>)> {
        hub.view()
            .zones
            .into_iter()
            .map(|z| (z.id, z.members.into_iter().collect()))
            .collect()
    }

    #[test]
    fn a_routed_client_subscribes_and_plays_the_stream_its_source_starts() {
        let t0 = Instant::now();
        let (mut hub, routing) = routed(t0);
        assert_eq!(subscription_changes(&routing), [(true, TONE, client(1))]);

        let effects = start_tone(&mut hub, t0);
        let sent = desired(&effects);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, client(1));
        assert_eq!(
            sent[0].1.playback,
            Playback::Playing {
                stream: StreamId(5),
                format: FORMAT
            }
        );
        assert_eq!(subscription_changes(&effects), []);
    }

    #[test]
    fn a_track_announced_before_its_stream_starts_is_kept() {
        let t0 = Instant::now();
        let mut hub = hub();
        let track = TrackMetadata {
            title: "Teardrop".to_owned(),
            artists: vec!["Massive Attack".to_owned()],
            album: "Mezzanine".to_owned(),
            cover_url: None,
            duration: Duration::from_secs(330),
        };
        let event = HubEvent::Source {
            source: TONE,
            event: SourceEvent::Track(track),
        };
        hub.handle(event, t0);
        start_tone(&mut hub, t0);

        let state = &hub.sources[&TONE];
        assert_eq!(
            state.track.as_ref().map(|t| t.title.as_str()),
            Some("Teardrop")
        );
        assert!(matches!(state.playback, SourcePlayback::Active { .. }));
    }

    #[test]
    fn unrouting_goes_idle_with_a_new_revision_and_unsubscribes() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let playing = desired(&start_tone(&mut hub, t0))[0].1.revision;

        let unroute = AdminRequest::RouteZone {
            zone: KITCHEN,
            source: None,
        };
        let effects = admin(&mut hub, unroute, t0);
        let sent = desired(&effects);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.playback, Playback::Idle);
        assert_eq!(sent[0].1.revision, Revision(playing.0 + 1));
        assert_eq!(subscription_changes(&effects), [(false, TONE, client(1))]);
    }

    #[test]
    fn renaming_a_zone_sends_no_desired_state() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let rename = AdminRequest::UpsertZone {
            zone: KITCHEN,
            name: "Galley".to_owned(),
        };
        let effects = admin(&mut hub, rename, t0);
        assert_eq!(desired(&effects), []);
        assert_eq!(hub.view().zones[0].name, "Galley");
    }

    #[test]
    fn moving_a_client_leaves_it_in_exactly_one_zone() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let lounge = AdminRequest::UpsertZone {
            zone: LOUNGE,
            name: "Lounge".to_owned(),
        };
        admin(&mut hub, lounge, t0);
        let effects = admin(
            &mut hub,
            AdminRequest::MoveClient {
                client: client(1),
                zone: Some(LOUNGE),
            },
            t0,
        );
        assert_eq!(
            members(&hub),
            [(KITCHEN, vec![]), (LOUNGE, vec![client(1)])]
        );
        assert_eq!(hub.view().clients[0].zone, Some(LOUNGE));
        assert_eq!(subscription_changes(&effects), [(false, TONE, client(1))]);

        admin(
            &mut hub,
            AdminRequest::MoveClient {
                client: client(1),
                zone: None,
            },
            t0,
        );
        assert_eq!(members(&hub), [(KITCHEN, vec![]), (LOUNGE, vec![])]);
    }

    #[test]
    fn a_reconnect_resends_the_same_revision_once() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let before = hub.view().clients[0].desired.clone();

        hub.handle(HubEvent::Disconnected { client: client(1) }, secs(t0, 1));
        let effects = hub.handle(HubEvent::Connected { hello: hello(1) }, secs(t0, 2));
        assert_eq!(desired(&effects), [(client(1), before)]);
        assert_eq!(subscription_changes(&effects), [(true, TONE, client(1))]);
    }

    #[test]
    fn a_connect_over_a_half_open_session_subscribes_the_new_one() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let effects = hub.handle(HubEvent::Connected { hello: hello(1) }, secs(t0, 1));
        assert_eq!(subscription_changes(&effects), [(true, TONE, client(1))]);
    }

    #[test]
    fn a_first_connect_sends_a_real_revision_once() {
        let effects = hub().handle(HubEvent::Connected { hello: hello(1) }, Instant::now());
        let sent = desired(&effects);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.revision, Revision(1));
        assert_eq!(sent[0].1.name, "pi-1");
        assert_eq!(sent[0].1.output, "hw:0");
    }

    #[test]
    fn a_silent_client_times_out_closes_and_unsubscribes() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let reported = ClientMessage::Reported(ReportedState {
            applied: Revision(1),
            player: PlayerState::Idle,
            update: UpdateState::UpToDate,
        });
        let message = HubEvent::Message {
            client: client(1),
            message: reported,
        };
        hub.handle(message, secs(t0, 10));

        let quiet = hub.handle(HubEvent::Tick, secs(t0, 25));
        assert!(!quiet.iter().any(|e| matches!(e, Effect::Close { .. })));
        assert!(hub.view().clients[0].online);

        let effects = hub.handle(HubEvent::Tick, secs(t0, 26));
        assert!(matches!(&effects[0], Effect::Close { client: c } if *c == client(1)));
        assert_eq!(subscription_changes(&effects), [(false, TONE, client(1))]);
        assert_eq!(desired(&effects), []);
        assert!(!hub.view().clients[0].online);
    }

    #[test]
    fn volume_is_source_times_zone() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        admin(
            &mut hub,
            AdminRequest::SetZoneVolume {
                zone: KITCHEN,
                volume: Volume::new(60),
            },
            t0,
        );
        let effects = admin(
            &mut hub,
            AdminRequest::SetSourceVolume {
                source: TONE,
                volume: Volume::new(40),
            },
            t0,
        );
        assert_eq!(desired(&effects)[0].1.volume, Volume::new(24));
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ControlSource {
                source: TONE,
                control: SourceControl::SetVolume(v),
            } if *v == Volume::new(40)
        )));
    }

    #[test]
    fn unknown_ids_change_nothing() {
        let t0 = Instant::now();
        let (mut hub, _) = routed(t0);
        let before = format!("{:?}", hub.view());
        let nobody = client(9);
        let nowhere = ZoneId(9);
        let nothing = SourceId(9);
        let requests = [
            AdminRequest::MoveClient {
                client: nobody.clone(),
                zone: Some(KITCHEN),
            },
            AdminRequest::MoveClient {
                client: client(1),
                zone: Some(nowhere),
            },
            AdminRequest::RouteZone {
                zone: nowhere,
                source: Some(TONE),
            },
            AdminRequest::RouteZone {
                zone: KITCHEN,
                source: Some(nothing),
            },
            AdminRequest::SetZoneVolume {
                zone: nowhere,
                volume: Volume::new(1),
            },
            AdminRequest::SetSourceVolume {
                source: nothing,
                volume: Volume::new(1),
            },
            AdminRequest::RenameClient {
                client: nobody.clone(),
                name: "x".to_owned(),
            },
            AdminRequest::SendCommand {
                client: nobody,
                command: Command::Identify,
            },
        ];
        for request in requests {
            let label = format!("{request:?}");
            let effects = admin(&mut hub, request, t0);
            assert!(
                matches!(effects.as_slice(), [Effect::StateChanged(_)]),
                "{label}: {effects:?}"
            );
            assert_eq!(format!("{:?}", hub.view()), before, "{label}");
        }
    }
}
