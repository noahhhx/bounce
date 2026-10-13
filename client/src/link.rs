//! Finding the server and staying connected to it.

use std::net::SocketAddr;
use std::time::Duration;

use bounce_common::protocol::Hello;
use bounce_common::time::SinceBoot;

use crate::transport::Connector;

pub enum LinkState {
    /// Browsing for [`bounce_common::protocol::MDNS_SERVICE`].
    Discovering,
    Connecting {
        server: SocketAddr,
    },
    /// `Hello` sent, waiting for `Desired` or `Rejected`.
    Handshaking {
        server: SocketAddr,
    },
    Connected {
        server: SocketAddr,
        since: SinceBoot,
    },
    Backoff {
        until: SinceBoot,
        attempt: u32,
    },
}

pub struct Link<C: Connector> {
    connector: C,
    state: LinkState,
    /// Skips discovery, for networks where multicast is filtered.
    configured_server: Option<SocketAddr>,
    backoff: Backoff,
}

/// Exponential with jitter, capped. Jitter matters here: when the server
/// restarts, every client in the house reconnects at once.
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl<C: Connector> Link<C> {
    /// Runs forever on the network thread. Plays nothing while disconnected,
    /// but keeps buffering logs, so the server sees what happened during the
    /// outage.
    pub fn run(self, hello: Hello) -> ! {
        todo!()
    }
}
