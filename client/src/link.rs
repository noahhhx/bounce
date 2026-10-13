use std::io;
use std::net::SocketAddr;
use std::thread;
use std::time::Duration;

use bounce_common::protocol::{
    ClientMessage, Hello, RejectReason, ServerMessage, HEARTBEAT_TIMEOUT,
};
use bounce_common::time::SinceBoot;
use bounce_common::wire::WireError;
use tracing::info;

use crate::clock;
use crate::transport::{Connection, Connector};
use crate::Device;

enum LinkState<T> {
    Connecting,
    /// `Hello` sent, waiting for `Desired` or `Rejected`.
    Handshaking {
        conn: T,
    },
    Connected {
        conn: T,
        next_tick: SinceBoot,
        last_heard: SinceBoot,
    },
    Backoff {
        delay: Duration,
    },
}

#[derive(Debug)]
enum Disconnect {
    Connect(io::Error),
    Wire(WireError),
    Rejected(RejectReason),
    NoReplyToHello,
    UnexpectedFirstMessage,
    ServerSilent,
}

impl From<WireError> for Disconnect {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(1);

pub struct Link<C: Connector> {
    connector: C,
    server: SocketAddr,
    backoff: Backoff,
    hello: Hello,
    device: Device,
    failures: u32,
    jitter: XorShift,
    server_silence: Duration,
}

/// Exponential with jitter, capped. Jitter matters here: when the server
/// restarts, every client in the house reconnects at once.
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    fn delay(&self, failures: u32, random: u64) -> Duration {
        let step = self
            .initial
            .saturating_mul(1 << failures.min(20))
            .min(self.max);
        let half = step / 2;
        let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
        half + Duration::from_nanos(random % spread.saturating_add(1))
    }
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

impl<C: Connector> Link<C> {
    pub fn new(
        connector: C,
        server: SocketAddr,
        backoff: Backoff,
        hello: Hello,
        device: Device,
    ) -> Self {
        let seed = hello.boot.0.max(1);
        Self {
            connector,
            server,
            backoff,
            hello,
            device,
            failures: 0,
            jitter: XorShift(seed),
            server_silence: HEARTBEAT_TIMEOUT,
        }
    }

    pub fn run(mut self) -> ! {
        let mut state = LinkState::Connecting;
        loop {
            state = self.step(state).unwrap_or_else(|cause| {
                let delay = self.backoff.delay(self.failures, self.jitter.next());
                self.failures = self.failures.saturating_add(1);
                info!(?cause, ?delay, "backing off");
                LinkState::Backoff { delay }
            });
        }
    }

    fn step(&mut self, state: LinkState<C::Conn>) -> Result<LinkState<C::Conn>, Disconnect> {
        match state {
            LinkState::Connecting => {
                let mut conn = self
                    .connector
                    .connect(self.server)
                    .map_err(Disconnect::Connect)?;
                info!(server = %self.server, "connected");
                conn.send(&ClientMessage::Hello(self.hello.clone()))?;
                Ok(LinkState::Handshaking { conn })
            }
            LinkState::Handshaking { mut conn } => match conn.recv(HANDSHAKE_TIMEOUT)? {
                Some(ServerMessage::Desired(desired)) => {
                    info!(revision = desired.revision.0, "handshake accepted");
                    self.failures = 0;
                    self.device.on_connect();
                    let now = clock::now();
                    let replies = self.device.on_message(ServerMessage::Desired(desired), now);
                    send_all(&mut conn, replies)?;
                    Ok(LinkState::Connected {
                        conn,
                        next_tick: now,
                        last_heard: now,
                    })
                }
                Some(ServerMessage::Rejected(rejected)) => {
                    Err(Disconnect::Rejected(rejected.reason))
                }
                Some(_) => Err(Disconnect::UnexpectedFirstMessage),
                None => Err(Disconnect::NoReplyToHello),
            },
            LinkState::Connected {
                mut conn,
                mut next_tick,
                mut last_heard,
            } => {
                let now = clock::now();
                if now >= next_tick {
                    send_all(&mut conn, self.device.tick(now))?;
                    next_tick = SinceBoot(now.0 + TICK);
                }
                let received = conn.recv(next_tick.0.saturating_sub(now.0))?;
                let now = clock::now();
                match received {
                    Some(ServerMessage::Rejected(rejected)) => {
                        return Err(Disconnect::Rejected(rejected.reason));
                    }
                    Some(message) => {
                        last_heard = now;
                        send_all(&mut conn, self.device.on_message(message, now))?;
                    }
                    None if now.0.saturating_sub(last_heard.0) > self.server_silence => {
                        return Err(Disconnect::ServerSilent);
                    }
                    None => {}
                }
                Ok(LinkState::Connected {
                    conn,
                    next_tick,
                    last_heard,
                })
            }
            LinkState::Backoff { delay } => {
                thread::sleep(delay);
                Ok(LinkState::Connecting)
            }
        }
    }
}

fn send_all(conn: &mut impl Connection, messages: Vec<ClientMessage>) -> Result<(), WireError> {
    messages.iter().try_for_each(|message| conn.send(message))
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    use bounce_common::audio::Codec;
    use bounce_common::ids::{BootId, ClientId, Version};
    use bounce_common::protocol::{Rejected, ReportedState, Revision, PROTOCOL_VERSION};
    use bounce_common::update::Target;
    use bounce_common::wire;

    use super::*;
    use crate::player::Output;
    use crate::tests::desired;
    use crate::transport::Tcp;

    const PATIENCE: Duration = Duration::from_secs(5);

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client: ClientId::parse("b0c00000000000000000000000000001").unwrap(),
            boot: BootId(77),
            version: Version {
                major: 0,
                minor: 0,
                patch: 0,
            },
            target: Target::X86_64,
            hostname: "test".into(),
            outputs: Vec::new(),
            codecs: vec![Codec::PcmS16Le],
        }
    }

    fn start(configure: impl FnOnce(&mut Link<Tcp>) + Send + 'static) -> TcpListener {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.local_addr().unwrap();
        thread::spawn(move || {
            let backoff = Backoff {
                initial: Duration::from_millis(10),
                max: Duration::from_millis(50),
            };
            let mut link = Link::new(Tcp, server, backoff, hello(), Device::new(Output::Alsa));
            configure(&mut link);
            link.run()
        });
        listener
    }

    fn accept(listener: &TcpListener) -> Session {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + PATIENCE;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(PATIENCE)).unwrap();
                    return Session(stream);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("client never connected: {e}"),
            }
        }
    }

    struct Session(TcpStream);

    impl Session {
        fn recv(&mut self) -> ClientMessage {
            let mut prefix = [0; 4];
            self.0.read_exact(&mut prefix).expect("client sent nothing");
            let mut body = vec![0; u32::from_le_bytes(prefix) as usize];
            self.0.read_exact(&mut body).unwrap();
            wire::decode(&body).unwrap()
        }

        fn send(&mut self, message: &ServerMessage) {
            self.0.write_all(&wire::encode(message).unwrap()).unwrap();
        }

        fn is_closed_by_client(&mut self) -> bool {
            let deadline = Instant::now() + PATIENCE;
            let mut sink = [0; 256];
            while Instant::now() < deadline {
                match self.0.read(&mut sink) {
                    Ok(0) => return true,
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
            false
        }
    }

    fn expect_hello(session: &mut Session) -> Hello {
        match session.recv() {
            ClientMessage::Hello(hello) => hello,
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn converges_then_reconnects_with_the_same_boot() {
        let listener = start(|_| {});

        let mut first = accept(&listener);
        let boot = expect_hello(&mut first).boot;
        first.send(&ServerMessage::Desired(desired(3)));
        assert!(matches!(
            first.recv(),
            ClientMessage::Reported(ReportedState {
                applied: Revision(3),
                ..
            })
        ));
        assert!(matches!(first.recv(), ClientMessage::TimeSync(_)));
        assert!(matches!(first.recv(), ClientMessage::TimeSync(_)));
        drop(first);
        let dropped = Instant::now();

        let mut second = accept(&listener);
        assert!(dropped.elapsed() < Duration::from_secs(1));
        assert_eq!(expect_hello(&mut second).boot, boot);
        second.send(&ServerMessage::Desired(desired(4)));
        assert!(matches!(
            second.recv(),
            ClientMessage::Reported(ReportedState {
                applied: Revision(4),
                ..
            })
        ));
    }

    #[test]
    fn rejected_backs_off_and_retries() {
        let listener = start(|_| {});

        let mut first = accept(&listener);
        expect_hello(&mut first);
        first.send(&ServerMessage::Rejected(Rejected {
            reason: RejectReason::DuplicateClientId,
            upgrade: None,
        }));
        assert!(first.is_closed_by_client());

        let mut second = accept(&listener);
        expect_hello(&mut second);
    }

    #[test]
    fn a_silent_server_is_left() {
        let listener = start(|link| link.server_silence = Duration::from_millis(1500));

        let mut first = accept(&listener);
        expect_hello(&mut first);
        first.send(&ServerMessage::Desired(desired(1)));
        assert!(matches!(first.recv(), ClientMessage::Reported(_)));
        assert!(first.is_closed_by_client());

        let mut second = accept(&listener);
        expect_hello(&mut second);
    }

    #[test]
    fn backoff_doubles_up_to_max_and_jitters_in_the_upper_half() {
        let backoff = Backoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
        };
        assert_eq!(backoff.delay(0, 0), Duration::from_millis(500));
        assert_eq!(backoff.delay(3, 0), Duration::from_secs(4));
        assert_eq!(backoff.delay(10, 0), Duration::from_secs(15));
        assert_eq!(backoff.delay(u32::MAX, 0), Duration::from_secs(15));
        assert_eq!(backoff.delay(10, 15_000_000_000), Duration::from_secs(30));

        let mut jitter = XorShift(77);
        let delays: Vec<_> = (0..8).map(|_| backoff.delay(2, jitter.next())).collect();
        assert!(delays
            .iter()
            .all(|d| (Duration::from_secs(2)..=Duration::from_secs(4)).contains(d)));
        assert!(delays.windows(2).any(|w| w[0] != w[1]), "{delays:?}");
    }
}
