//! The seam between the link and the network. Blocking, with no async
//! runtime, to keep the client small on a Pi Zero. Moving to WebSocket or
//! QUIC means one new pair of impls here; the encoding itself lives in
//! [`bounce_common::wire`].

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use bounce_common::protocol::{ClientMessage, ServerMessage};
use bounce_common::wire::WireError;

pub trait Connector {
    type Conn: Connection;

    /// # Errors
    ///
    /// Any IO error; the link backs off and retries.
    fn connect(&self, server: SocketAddr) -> io::Result<Self::Conn>;
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
    stream: std::net::TcpStream,
    read_buf: Vec<u8>,
}

impl Connector for Tcp {
    type Conn = TcpConnection;

    fn connect(&self, server: SocketAddr) -> io::Result<TcpConnection> {
        todo!()
    }
}

impl Connection for TcpConnection {
    fn recv(&mut self, timeout: Duration) -> Result<Option<ServerMessage>, WireError> {
        todo!()
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), WireError> {
        todo!()
    }
}
