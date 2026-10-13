//! Every client the server has ever seen. Persisted, so a client that is
//! off when the server restarts still shows as offline instead of vanishing.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use bounce_common::ids::{BootId, ClientId, Version, ZoneId};
use bounce_common::protocol::{DesiredState, Hello, Playback, ReportedState, Revision};
use bounce_common::telemetry::{Fault, Level, MetricsSample};
use bounce_common::zone::Volume;

use crate::session::SessionId;

#[derive(Default)]
pub struct Fleet {
    clients: HashMap<ClientId, ClientRecord>,
}

pub struct ClientRecord {
    /// From the most recent connect. Hardware and version can change across
    /// boots, so this is overwritten each time.
    pub hello: Hello,
    /// Set in the web UI. `None` shows the hostname.
    pub name: Option<String>,
    pub liveness: Liveness,
    pub zone: Option<ZoneId>,
    pub desired: DesiredState,
    /// `None` between a connect and the client's first report.
    pub reported: Option<ReportedState>,
    /// Recent samples, enough for the health rules to compute rates from the
    /// since-boot counters.
    pub metrics: VecDeque<(Instant, MetricsSample)>,
    pub faults: VecDeque<(Instant, Fault)>,
    pub release_pin: Option<Version>,
    /// Versions this client rolled back from. Never offered to it again,
    /// which stops an install, crash, roll back, install loop.
    pub bad_releases: BTreeSet<Version>,
}

pub enum Liveness {
    Online {
        /// A `Disconnected` from any other session is stale: it comes from
        /// the half-open socket this session replaced.
        session: SessionId,
        boot: BootId,
        connected_at: Instant,
        /// Any message counts. Metrics arrive every few seconds, so a gap
        /// longer than the heartbeat timeout means the client is wedged or
        /// its Wi-Fi is gone even if TCP hasn't noticed.
        last_heard: Instant,
    },
    Offline {
        since: Instant,
        cause: OfflineCause,
    },
}

pub enum OfflineCause {
    ConnectionClosed,
    HeartbeatTimeout,
    /// Loaded from disk after a server restart; the client hasn't been seen
    /// since.
    NotSeenSinceServerStart,
}

impl Fleet {
    #[must_use]
    pub fn get(&self, id: &ClientId) -> Option<&ClientRecord> {
        self.clients.get(id)
    }

    pub fn get_mut(&mut self, id: &ClientId) -> Option<&mut ClientRecord> {
        self.clients.get_mut(id)
    }

    /// Creates the record on first sight, otherwise refreshes `hello` and
    /// flips liveness to online. Returns the record so the hub can compute
    /// its desired state. A new record's desired state is a revision 0
    /// placeholder that the hub replaces before sending.
    pub fn connected(
        &mut self,
        hello: Hello,
        session: SessionId,
        now: Instant,
    ) -> &mut ClientRecord {
        let liveness = Liveness::Online {
            session,
            boot: hello.boot,
            connected_at: now,
            last_heard: now,
        };
        match self.clients.entry(hello.client.clone()) {
            Entry::Occupied(entry) => {
                let record = entry.into_mut();
                record.hello = hello;
                record.liveness = liveness;
                record
            }
            Entry::Vacant(entry) => entry.insert(ClientRecord {
                desired: DesiredState {
                    revision: Revision(0),
                    name: hello.hostname.clone(),
                    output: String::new(),
                    volume: Volume::new(0),
                    muted: false,
                    output_latency: Duration::ZERO,
                    log_level: Level::Info,
                    playback: Playback::Idle,
                    release: None,
                },
                hello,
                name: None,
                liveness,
                zone: None,
                reported: None,
                metrics: VecDeque::new(),
                faults: VecDeque::new(),
                release_pin: None,
                bad_releases: BTreeSet::new(),
            }),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ClientId, &ClientRecord)> {
        self.clients.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ClientId, &mut ClientRecord)> {
        self.clients.iter_mut()
    }
}

impl ClientRecord {
    #[must_use]
    pub fn is_online(&self) -> bool {
        matches!(self.liveness, Liveness::Online { .. })
    }
}
