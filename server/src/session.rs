//! One task per client. Reads `Hello`, registers with the hub, then pumps
//! messages both ways until the connection drops or the hub closes it.
//! Logs go to the telemetry store and the session acks them; everything
//! else goes to the hub.

use tokio::sync::mpsc;

use bounce_common::audio::AudioChunk;
use bounce_common::ids::ClientId;
use bounce_common::protocol::ServerMessage;

use crate::transport::Connection;

/// The write side of a session, shared with the hub's effect runner and
/// the broadcaster of whatever source the client's zone is playing.
///
/// Two lanes, because the two kinds of traffic fail differently. Control
/// messages are few and must arrive, so that lane is unbounded. Audio is
/// constant and goes stale within a second, so that lane is bounded and
/// drops chunks when full. A slow client then falls behind instead of
/// growing server memory.
#[derive(Clone)]
pub struct Outbox {
    control: mpsc::UnboundedSender<ServerMessage>,
    audio: mpsc::Sender<AudioChunk>,
}

impl Outbox {
    pub fn control(&self, message: ServerMessage) {
        todo!()
    }

    pub fn audio(&self, chunk: AudioChunk) {
        todo!("try_send, count drops")
    }
}

#[derive(Debug)]
pub enum HandshakeError {
    /// No `Hello` within the handshake timeout.
    Timeout,
    Malformed,
    /// Sent `Rejected` and closed. Includes an upgrade when one exists.
    Rejected,
}

pub struct Session<C: Connection> {
    client: ClientId,
    conn: C,
    control_rx: mpsc::UnboundedReceiver<ServerMessage>,
    audio_rx: mpsc::Receiver<AudioChunk>,
}

impl<C: Connection> Session<C> {
    /// Waits for `Hello` and checks the protocol version. A version mismatch
    /// still gets a `Rejected` carrying a release when one exists, because
    /// that is how an old client gets new enough to be accepted.
    ///
    /// # Errors
    ///
    /// Every [`HandshakeError`] variant ends the connection.
    pub async fn handshake(conn: C) -> Result<(Self, Outbox), HandshakeError> {
        todo!()
    }

    /// Selects over the connection and both outbox lanes, control first.
    pub async fn run(self) {
        todo!()
    }
}
