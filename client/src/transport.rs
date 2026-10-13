//! The seam between the link and the network. Blocking, with no async
//! runtime, to keep the client small on a Pi Zero. Moving to WebSocket or
//! QUIC means one new pair of impls here; the encoding itself lives in
//! [`bounce_common::wire`].

use std::io::{self, Read, Write};
use std::net::{SocketAddr, SocketAddrV6, TcpStream};
use std::time::{Duration, Instant};

use bounce_common::protocol::{ClientMessage, ServerMessage, MDNS_SERVICE};
use bounce_common::wire::{self, WireError, MAX_FRAME_LEN};
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};
use tracing::info;

use crate::config::Server;

pub trait Connector {
    type Conn: Connection;

    /// # Errors
    ///
    /// Any IO error; the link backs off and retries.
    fn connect(&self, server: SocketAddr) -> io::Result<Self::Conn>;
}

pub trait Resolver {
    /// # Errors
    ///
    /// When no server is found; the link backs off and retries.
    fn resolve(&self) -> io::Result<SocketAddr>;
}

pub trait Connection {
    /// `Ok(None)` when `timeout` passes without a complete message, so the
    /// link can run its ticks between reads on one thread.
    ///
    /// # Errors
    ///
    /// Any [`WireError`] ends the connection.
    fn recv(&mut self, timeout: Duration) -> Result<Option<ServerMessage>, WireError>;

    /// # Errors
    ///
    /// Any [`WireError`] ends the connection.
    fn send(&mut self, message: &ClientMessage) -> Result<(), WireError>;
}

pub struct Tcp;

/// Length-prefixed frames over TCP with `TCP_NODELAY`. A read timeout can
/// land mid-frame, so partial frames accumulate in `read_buf`.
pub struct TcpConnection {
    stream: TcpStream,
    read_buf: Vec<u8>,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A server that stops reading would otherwise block `send` forever once
/// the socket buffer fills, and the link could never notice it is gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

impl Connector for Tcp {
    type Conn = TcpConnection;

    fn connect(&self, server: SocketAddr) -> io::Result<TcpConnection> {
        let stream = TcpStream::connect_timeout(&server, CONNECT_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(TcpConnection {
            stream,
            read_buf: Vec::new(),
        })
    }
}

/// Responders answer within 120 ms (RFC 6762 section 6). This leaves room
/// for the browser's retransmits when a query or answer is lost.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

impl Resolver for Server {
    fn resolve(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Fixed(server) => Ok(*server),
            Self::Discover => discover(DISCOVERY_TIMEOUT),
        }
    }
}

/// A fresh daemon per lookup, so a server that changed address is never
/// answered from a stale cache, and no mDNS thread runs while connected.
fn discover(timeout: Duration) -> io::Result<SocketAddr> {
    let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
    let found = browse(&daemon, timeout);
    let _ = daemon.shutdown();
    found
}

fn browse(daemon: &ServiceDaemon, timeout: Duration) -> io::Result<SocketAddr> {
    let events = daemon.browse(MDNS_SERVICE).map_err(io::Error::other)?;
    let deadline = Instant::now() + timeout;
    while let Ok(event) = events.recv_deadline(deadline) {
        let ServiceEvent::ServiceResolved(service) = event else {
            continue;
        };
        let port = service.get_port();
        let offered = service
            .get_addresses()
            .iter()
            .map(|ip| socket_addr(ip, port));
        if let Some(server) = prefer_ipv4(offered) {
            info!(name = service.get_fullname(), %server, "discovered server");
            return Ok(server);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no {MDNS_SERVICE} answered within {timeout:?}"),
    ))
}

fn socket_addr(ip: &ScopedIp, port: u16) -> SocketAddr {
    match ip {
        ScopedIp::V6(v6) => SocketAddrV6::new(*v6.addr(), port, 0, v6.scope_id().index).into(),
        ip => SocketAddr::new(ip.to_ip_addr(), port),
    }
}

fn prefer_ipv4(offered: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    offered
        .into_iter()
        .min_by_key(|addr| (addr.is_ipv6(), *addr))
}

impl TcpConnection {
    fn take_frame(&mut self) -> Result<Option<ServerMessage>, WireError> {
        let Some(prefix) = self.read_buf.first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix);
        if len > MAX_FRAME_LEN {
            return Err(WireError::FrameTooLarge(len));
        }
        let end = 4 + len as usize;
        if self.read_buf.len() < end {
            return Ok(None);
        }
        let message = wire::decode(&self.read_buf[4..end])?;
        self.read_buf.drain(..end);
        Ok(Some(message))
    }
}

impl Connection for TcpConnection {
    fn recv(&mut self, timeout: Duration) -> Result<Option<ServerMessage>, WireError> {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0; 16 * 1024];
        loop {
            if let Some(message) = self.take_frame()? {
                return Ok(Some(message));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(WireError::Io)?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(WireError::Closed),
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(WireError::Io(e)),
            }
        }
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), WireError> {
        let frame = wire::encode(message)?;
        self.stream.write_all(&frame).map_err(WireError::Io)
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bounce_common::protocol::TimeSyncReply;
    use bounce_common::time::{ServerInstant, SinceBoot};

    use super::*;

    fn pair() -> (TcpConnection, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let conn = Tcp.connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (conn, server)
    }

    fn reply_frame() -> Vec<u8> {
        wire::encode(&ServerMessage::TimeSync(TimeSyncReply {
            client_sent: SinceBoot(Duration::from_millis(5)),
            server_received: ServerInstant(10),
            server_sent: ServerInstant(11),
        }))
        .unwrap()
    }

    #[test]
    fn frame_split_across_reads_waits_for_its_second_half() {
        let (mut conn, mut server) = pair();
        let frame = reply_frame();
        let (head, tail) = frame.split_at(frame.len() / 2);

        server.write_all(head).unwrap();
        let first = conn.recv(Duration::from_millis(100)).unwrap();
        assert!(first.is_none(), "half a frame decoded as {first:?}");

        server.write_all(tail).unwrap();
        let second = conn.recv(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            second,
            Some(ServerMessage::TimeSync(TimeSyncReply {
                server_sent: ServerInstant(11),
                ..
            }))
        ));
    }

    #[test]
    fn two_frames_in_one_read_come_out_one_at_a_time() {
        let (mut conn, mut server) = pair();
        server
            .write_all(&[reply_frame(), reply_frame()].concat())
            .unwrap();
        assert!(conn.recv(Duration::from_secs(5)).unwrap().is_some());
        assert!(conn.recv(Duration::ZERO).unwrap().is_some());
        assert!(conn.recv(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn server_closing_is_closed() {
        let (mut conn, server) = pair();
        drop(server);
        assert!(matches!(
            conn.recv(Duration::from_secs(5)),
            Err(WireError::Closed)
        ));
    }

    #[test]
    fn discovery_prefers_ipv4_when_both_are_offered() {
        let v6 = SocketAddr::from(([0xfe80, 0, 0, 0, 0, 0, 0, 1], 7700));
        let v4 = SocketAddr::from(([192, 168, 1, 5], 7700));
        assert_eq!(prefer_ipv4([v6, v4]), Some(v4));
        assert_eq!(prefer_ipv4([v6]), Some(v6));
        assert_eq!(prefer_ipv4([]), None);
    }

    #[test]
    fn oversize_prefix_is_frame_too_large() {
        let (mut conn, mut server) = pair();
        server
            .write_all(&(MAX_FRAME_LEN + 1).to_le_bytes())
            .unwrap();
        assert!(matches!(
            conn.recv(Duration::from_secs(5)),
            Err(WireError::FrameTooLarge(n)) if n == MAX_FRAME_LEN + 1
        ));
    }
}
