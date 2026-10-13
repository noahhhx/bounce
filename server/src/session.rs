use std::time::{Duration, SystemTime};

use log::{debug, info, warn};
use tokio::sync::mpsc;

use bounce_common::audio::AudioChunk;
use bounce_common::ids::ClientId;
use bounce_common::protocol::{
    ClientMessage, Hello, RejectReason, Rejected, ServerMessage, TimeSyncReply, PROTOCOL_VERSION,
};
use bounce_common::telemetry::LogBatch;
use bounce_common::time::SinceBoot;
use bounce_common::wire::WireError;

use crate::clock;
use crate::telemetry::{ClientClock, Telemetry};
use crate::transport::{Connection, Listener};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

pub enum SessionEvent {
    Opened {
        session: SessionId,
        hello: Hello,
        outbox: Outbox,
    },
    Message {
        client: ClientId,
        message: ClientMessage,
    },
    Closed {
        client: ClientId,
        session: SessionId,
    },
}

/// Two lanes, because the two kinds of traffic fail differently. Control
/// messages are few and must arrive, so that lane is unbounded. Audio is
/// constant and goes stale within a second, so that lane is bounded and
/// drops chunks when full. A slow client then falls behind instead of
/// growing server memory.
pub struct Outbox {
    control: mpsc::UnboundedSender<ServerMessage>,
    audio: AudioLane,
}

#[derive(Clone)]
pub struct AudioLane(mpsc::Sender<AudioChunk>);

impl Outbox {
    /// The audio lane holds more than a playout delay of chunks, so a late
    /// joiner gets every pending chunk the broadcaster replays to it.
    pub(crate) fn channel() -> (
        Self,
        mpsc::UnboundedReceiver<ServerMessage>,
        mpsc::Receiver<AudioChunk>,
    ) {
        let (control, control_rx) = mpsc::unbounded_channel();
        let (audio, audio_rx) = mpsc::channel(256);
        let outbox = Self {
            control,
            audio: AudioLane(audio),
        };
        (outbox, control_rx, audio_rx)
    }

    pub fn control(&self, message: ServerMessage) {
        let _ = self.control.send(message);
    }

    #[must_use]
    pub fn audio_lane(&self) -> AudioLane {
        self.audio.clone()
    }
}

impl AudioLane {
    pub fn send(&self, chunk: AudioChunk) {
        let _ = self.0.try_send(chunk);
    }
}

#[derive(Debug)]
pub enum HandshakeError {
    /// No `Hello` within the handshake timeout.
    Timeout,
    NotHello,
    Rejected,
    Connection(WireError),
}

pub async fn accept<L: Listener>(
    mut listener: L,
    events: mpsc::UnboundedSender<SessionEvent>,
    telemetry: Telemetry,
) {
    let mut next = 0;
    loop {
        let (conn, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                // Out of file descriptors would fail again immediately.
                warn!("accept: {error}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let session = SessionId(next);
        next += 1;
        debug!("session {}: connection from {addr}", session.0);
        tokio::spawn(serve(conn, session, events.clone(), telemetry.clone()));
    }
}

pub async fn serve<C: Connection>(
    conn: C,
    session: SessionId,
    events: mpsc::UnboundedSender<SessionEvent>,
    telemetry: Telemetry,
) {
    let (running, hello, outbox) = match Session::handshake(conn, session).await {
        Ok(opened) => opened,
        Err(error) => return info!("session {}: handshake failed: {error:?}", session.0),
    };
    info!("session {}: {:?} connected", session.0, hello.client);
    let _ = events.send(SessionEvent::Opened {
        session,
        hello,
        outbox,
    });
    running.run(&events, &telemetry).await;
}

struct Session<C: Connection> {
    id: SessionId,
    client: ClientId,
    conn: C,
    control_rx: mpsc::UnboundedReceiver<ServerMessage>,
    audio_rx: mpsc::Receiver<AudioChunk>,
}

impl<C: Connection> Session<C> {
    async fn handshake(
        mut conn: C,
        id: SessionId,
    ) -> Result<(Self, Hello, Outbox), HandshakeError> {
        let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.recv())
            .await
            .map_err(|_| HandshakeError::Timeout)?
            .map_err(HandshakeError::Connection)?;
        let ClientMessage::Hello(hello) = first else {
            return Err(HandshakeError::NotHello);
        };
        let server = PROTOCOL_VERSION;
        let reason = match hello.protocol.cmp(&server) {
            std::cmp::Ordering::Less => Some(RejectReason::ProtocolTooOld { server }),
            std::cmp::Ordering::Greater => Some(RejectReason::ProtocolTooNew { server }),
            std::cmp::Ordering::Equal => None,
        };
        if let Some(reason) = reason {
            let rejected = Rejected {
                reason,
                upgrade: None,
            };
            let _ = conn.send(&ServerMessage::Rejected(rejected)).await;
            return Err(HandshakeError::Rejected);
        }
        let (outbox, control_rx, audio_rx) = Outbox::channel();
        let session = Self {
            id,
            client: hello.client.clone(),
            conn,
            control_rx,
            audio_rx,
        };
        Ok((session, hello, outbox))
    }

    async fn run(mut self, events: &mpsc::UnboundedSender<SessionEvent>, telemetry: &Telemetry) {
        let mut client_clock = None;
        let ended = loop {
            tokio::select! {
                biased;
                message = self.control_rx.recv() => {
                    let Some(message) = message else {
                        break None;
                    };
                    if let Err(error) = self.conn.send(&message).await {
                        break Some(error);
                    }
                }
                Some(chunk) = self.audio_rx.recv() => {
                    if let Err(error) = self.conn.send(&ServerMessage::Audio(chunk)).await {
                        break Some(error);
                    }
                }
                received = self.conn.recv() => {
                    let received_at = clock::now();
                    let message = match received {
                        Ok(message) => message,
                        Err(error) => break Some(error),
                    };
                    match message {
                        ClientMessage::Logs(batch) => {
                            let boot = batch.boot;
                            let clock = client_clock.unwrap_or_else(|| assume_sent_now(&batch));
                            let client = self.client.clone();
                            let Some(up_to) = telemetry.append_logs(client, batch, clock).await
                            else {
                                continue;
                            };
                            if let Err(error) = self.conn.send(&ServerMessage::LogsAck { boot, up_to }).await {
                                break Some(error);
                            }
                        }
                        ClientMessage::TimeSync(request) => {
                            client_clock = Some(ClientClock {
                                since_boot: request.client_sent,
                                wall: SystemTime::now(),
                            });
                            let reply = TimeSyncReply {
                                client_sent: request.client_sent,
                                server_received: received_at,
                                server_sent: clock::now(),
                            };
                            if let Err(error) = self.conn.send(&ServerMessage::TimeSync(reply)).await {
                                break Some(error);
                            }
                            self.forward(events, ClientMessage::TimeSync(request));
                        }
                        message => self.forward(events, message),
                    }
                }
            }
        };
        match ended {
            None => info!("session {}: closed by the server", self.id.0),
            Some(WireError::Closed) => info!("session {}: closed by the client", self.id.0),
            Some(error) => info!("session {}: ended: {error:?}", self.id.0),
        }
        let _ = events.send(SessionEvent::Closed {
            client: self.client,
            session: self.id,
        });
    }

    /// Any message refreshes the hub's heartbeat, a `TimeSync` included.
    fn forward(&self, events: &mpsc::UnboundedSender<SessionEvent>, message: ClientMessage) {
        let client = self.client.clone();
        let _ = events.send(SessionEvent::Message { client, message });
    }
}

fn assume_sent_now(batch: &LogBatch) -> ClientClock {
    let newest = batch.records.last().map(|record| record.at);
    ClientClock {
        since_boot: newest.unwrap_or(SinceBoot(Duration::ZERO)),
        wall: SystemTime::now(),
    }
}

#[cfg(test)]
mod tests {
    use bounce_common::audio::Codec;
    use bounce_common::ids::{BootId, StreamId, Version};
    use bounce_common::protocol::{Command, TimeSyncRequest};
    use bounce_common::telemetry::{Level, LogRecord, LogSeq};
    use bounce_common::time::ServerInstant;
    use bounce_common::update::Target;

    use super::*;
    use crate::telemetry::{MemoryStore, Retention};

    struct FakeConn {
        incoming: mpsc::UnboundedReceiver<ClientMessage>,
        outgoing: mpsc::UnboundedSender<ServerMessage>,
    }

    impl Connection for FakeConn {
        async fn recv(&mut self) -> Result<ClientMessage, WireError> {
            self.incoming.recv().await.ok_or(WireError::Closed)
        }

        fn send(
            &mut self,
            message: &ServerMessage,
        ) -> impl std::future::Future<Output = Result<(), WireError>> {
            let sent = self.outgoing.send(message.clone());
            std::future::ready(sent.map_err(|_| WireError::Closed))
        }
    }

    struct Peer {
        to_server: mpsc::UnboundedSender<ClientMessage>,
        from_server: mpsc::UnboundedReceiver<ServerMessage>,
    }

    impl Peer {
        async fn next(&mut self) -> ServerMessage {
            within_a_second(self.from_server.recv()).await.unwrap()
        }
    }

    fn fake() -> (FakeConn, Peer) {
        let (to_server, incoming) = mpsc::unbounded_channel();
        let (outgoing, from_server) = mpsc::unbounded_channel();
        let conn = FakeConn { incoming, outgoing };
        let peer = Peer {
            to_server,
            from_server,
        };
        (conn, peer)
    }

    async fn within_a_second<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(1), future)
            .await
            .expect("timed out")
    }

    async fn next_event(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> SessionEvent {
        within_a_second(events.recv()).await.unwrap()
    }

    fn client() -> ClientId {
        ClientId::parse("b0c00000000000000000000000000001").unwrap()
    }

    fn hello(protocol: u16) -> ClientMessage {
        ClientMessage::Hello(Hello {
            protocol,
            client: client(),
            boot: BootId(1),
            version: Version {
                major: 0,
                minor: 1,
                patch: 0,
            },
            target: Target::X86_64,
            hostname: "pi".to_owned(),
            outputs: vec![],
            codecs: vec![Codec::PcmS16Le],
        })
    }

    async fn opened(session: u64) -> (Peer, mpsc::UnboundedReceiver<SessionEvent>, Outbox) {
        let (conn, peer) = fake();
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let telemetry = Telemetry::spawn(MemoryStore::new(Retention::default()));
        tokio::spawn(serve(conn, SessionId(session), events_tx, telemetry));
        peer.to_server.send(hello(PROTOCOL_VERSION)).unwrap();
        let SessionEvent::Opened {
            session: id,
            hello,
            outbox,
        } = next_event(&mut events).await
        else {
            panic!("expected Opened");
        };
        assert_eq!(id, SessionId(session));
        assert_eq!(hello.client, client());
        (peer, events, outbox)
    }

    #[tokio::test]
    async fn hello_opens_a_session_whose_outbox_reaches_the_client() {
        let (mut peer, _events, outbox) = opened(3).await;

        outbox.control(ServerMessage::Command(Command::Identify));
        assert!(matches!(
            peer.next().await,
            ServerMessage::Command(Command::Identify)
        ));

        outbox.audio_lane().send(AudioChunk {
            stream: StreamId(1),
            seq: 9,
            play_at: ServerInstant(0),
            data: vec![],
        });
        assert!(matches!(
            peer.next().await,
            ServerMessage::Audio(AudioChunk { seq: 9, .. })
        ));
    }

    #[tokio::test]
    async fn a_protocol_mismatch_is_rejected_with_no_upgrade() {
        for protocol in [PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
            let (conn, mut peer) = fake();
            peer.to_server.send(hello(protocol)).unwrap();
            let result = Session::handshake(conn, SessionId(0)).await;
            assert!(matches!(result, Err(HandshakeError::Rejected)));

            let ServerMessage::Rejected(rejected) = peer.next().await else {
                panic!("expected Rejected");
            };
            let expected = if protocol < PROTOCOL_VERSION {
                RejectReason::ProtocolTooOld {
                    server: PROTOCOL_VERSION,
                }
            } else {
                RejectReason::ProtocolTooNew {
                    server: PROTOCOL_VERSION,
                }
            };
            assert_eq!(format!("{:?}", rejected.reason), format!("{expected:?}"));
            assert!(rejected.upgrade.is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn no_hello_within_five_seconds_times_out() {
        let (conn, _peer) = fake();
        let result = Session::handshake(conn, SessionId(0)).await;
        assert!(matches!(result, Err(HandshakeError::Timeout)));
    }

    #[tokio::test]
    async fn logs_are_stored_and_acked_on_the_session_without_reaching_the_hub() {
        let (mut peer, mut events, _outbox) = opened(6).await;
        let record = |seq| LogRecord {
            seq: LogSeq(seq),
            at: SinceBoot(Duration::from_millis(seq)),
            level: Level::Info,
            target: "t".into(),
            message: "m".into(),
            fields: Vec::new(),
        };
        let batch = LogBatch {
            boot: BootId(1),
            records: vec![record(1), record(2)],
            dropped: 0,
        };
        for _ in 0..2 {
            peer.to_server
                .send(ClientMessage::Logs(batch.clone()))
                .unwrap();
            assert!(matches!(
                peer.next().await,
                ServerMessage::LogsAck {
                    boot: BootId(1),
                    up_to: LogSeq(2),
                }
            ));
        }

        let client_sent = SinceBoot(Duration::from_millis(9));
        peer.to_server
            .send(ClientMessage::TimeSync(TimeSyncRequest { client_sent }))
            .unwrap();
        assert!(matches!(
            next_event(&mut events).await,
            SessionEvent::Message {
                message: ClientMessage::TimeSync(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_time_sync_is_answered_with_server_stamps_and_forwarded() {
        let (mut peer, mut events, _outbox) = opened(4).await;
        let client_sent = SinceBoot(Duration::from_millis(7));
        let before = clock::now();
        peer.to_server
            .send(ClientMessage::TimeSync(TimeSyncRequest { client_sent }))
            .unwrap();

        let ServerMessage::TimeSync(reply) = peer.next().await else {
            panic!("expected a TimeSync reply");
        };
        assert_eq!(reply.client_sent, client_sent);
        assert!(before <= reply.server_received);
        assert!(reply.server_received <= reply.server_sent);
        assert!(reply.server_sent <= clock::now());

        assert!(matches!(
            next_event(&mut events).await,
            SessionEvent::Message {
                client: c,
                message: ClientMessage::TimeSync(_),
            } if c == client()
        ));
    }

    #[tokio::test]
    async fn a_peer_close_reports_closed_with_its_session() {
        let (peer, mut events, _outbox) = opened(7).await;
        drop(peer);
        assert!(matches!(
            next_event(&mut events).await,
            SessionEvent::Closed {
                client: c,
                session: SessionId(7),
            } if c == client()
        ));
    }

    #[tokio::test]
    async fn dropping_the_outbox_ends_the_session_while_its_audio_lane_lives() {
        let (_peer, mut events, outbox) = opened(5).await;
        let lane = outbox.audio_lane();
        drop(outbox);
        assert!(matches!(
            next_event(&mut events).await,
            SessionEvent::Closed {
                session: SessionId(5),
                ..
            }
        ));
        drop(lane);
    }
}
