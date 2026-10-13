//! The seam between sessions and the network. Sessions see whole decoded
//! messages, never bytes or sockets. Moving to WebSocket or QUIC means one
//! new pair of impls here and a changed type in `main`; the encoding itself
//! lives in [`bounce_common::wire`].

use std::future::Future;
use std::io;
use std::net::SocketAddr;

use bounce_common::protocol::{ClientMessage, ServerMessage};
use bounce_common::wire::WireError;

pub trait Listener: Send {
    type Conn: Connection;

    fn accept(&mut self) -> impl Future<Output = io::Result<(Self::Conn, SocketAddr)>> + Send;
}

pub trait Connection: Send + 'static {
    /// Must be cancel-safe: a session selects over this and its outbox, and
    /// a partly read frame has to survive the other branch winning.
    fn recv(&mut self) -> impl Future<Output = Result<ClientMessage, WireError>> + Send;

    fn send(
        &mut self,
        message: &ServerMessage,
    ) -> impl Future<Output = Result<(), WireError>> + Send;
}

pub struct TcpListener {
    inner: tokio::net::TcpListener,
}

/// Length-prefixed frames over TCP with `TCP_NODELAY`. Partial reads
/// accumulate in `read_buf`, which is what makes `recv` cancel-safe.
pub struct TcpConnection {
    stream: tokio::net::TcpStream,
    read_buf: Vec<u8>,
}

impl Listener for TcpListener {
    type Conn = TcpConnection;

    async fn accept(&mut self) -> io::Result<(TcpConnection, SocketAddr)> {
        todo!()
    }
}

impl Connection for TcpConnection {
    async fn recv(&mut self) -> Result<ClientMessage, WireError> {
        todo!()
    }

    async fn send(&mut self, message: &ServerMessage) -> Result<(), WireError> {
        todo!()
    }
}
