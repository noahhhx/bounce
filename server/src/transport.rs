//! The seam between sessions and the network. Sessions see whole decoded
//! messages, never bytes or sockets. Moving to WebSocket or QUIC means one
//! new pair of impls here and a changed type in `main`; the encoding itself
//! lives in [`bounce_common::wire`].

use std::future::Future;
use std::io;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use bounce_common::protocol::{ClientMessage, ServerMessage};
use bounce_common::wire::{self, WireError, MAX_FRAME_LEN};

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

impl From<tokio::net::TcpListener> for TcpListener {
    fn from(inner: tokio::net::TcpListener) -> Self {
        Self { inner }
    }
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
        let (stream, addr) = self.inner.accept().await?;
        // Audio chunks are small and due on a deadline; Nagle would hold
        // them back waiting for an ack.
        stream.set_nodelay(true)?;
        let conn = TcpConnection {
            stream,
            read_buf: Vec::new(),
        };
        Ok((conn, addr))
    }
}

impl TcpConnection {
    /// Takes one whole frame off the front of `read_buf`, if there is one.
    fn take_frame(&mut self) -> Result<Option<ClientMessage>, WireError> {
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
        let message = wire::decode(&self.read_buf[4..end]);
        self.read_buf.drain(..end);
        message.map(Some)
    }
}

impl Connection for TcpConnection {
    async fn recv(&mut self) -> Result<ClientMessage, WireError> {
        loop {
            if let Some(message) = self.take_frame()? {
                return Ok(message);
            }
            self.read_buf.reserve(4096);
            // read_buf is cancel-safe: if this future is dropped, no bytes
            // were taken from the socket.
            let read = self.stream.read_buf(&mut self.read_buf).await;
            if read.map_err(WireError::Io)? == 0 {
                return Err(WireError::Closed);
            }
        }
    }

    async fn send(&mut self, message: &ServerMessage) -> Result<(), WireError> {
        let frame = wire::encode(message)?;
        self.stream.write_all(&frame).await.map_err(WireError::Io)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bounce_common::protocol::TimeSyncRequest;
    use bounce_common::time::SinceBoot;
    use tokio::net::TcpStream;

    use super::*;

    async fn pair() -> (TcpConnection, TcpStream) {
        let bound = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = bound.local_addr().unwrap();
        let mut listener = TcpListener::from(bound);
        let peer = TcpStream::connect(addr).await.unwrap();
        let (conn, _) = listener.accept().await.unwrap();
        (conn, peer)
    }

    #[tokio::test]
    async fn a_frame_split_across_cancelled_recvs_arrives_whole() {
        let (mut conn, mut peer) = pair().await;
        let sent = ClientMessage::TimeSync(TimeSyncRequest {
            client_sent: SinceBoot(Duration::from_micros(123_456_789)),
        });
        let frame = wire::encode(&sent).unwrap();
        let (head, tail) = frame.split_at(frame.len() / 2);
        let (head, tail) = (head.to_vec(), tail.to_vec());
        tokio::spawn(async move {
            peer.write_all(&head).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            peer.write_all(&tail).await.unwrap();
            peer
        });

        let mut cancelled = 0;
        let received = loop {
            tokio::select! {
                received = conn.recv() => break received.unwrap(),
                () = tokio::time::sleep(Duration::from_millis(10)) => cancelled += 1,
            }
        };
        assert!(
            cancelled > 0,
            "the timer never won, so nothing was cancelled"
        );
        assert_eq!(format!("{received:?}"), format!("{sent:?}"));
    }

    #[tokio::test]
    async fn an_oversize_length_prefix_is_rejected_before_its_body() {
        let (mut conn, mut peer) = pair().await;
        peer.write_all(&(MAX_FRAME_LEN + 1).to_le_bytes())
            .await
            .unwrap();
        let received = conn.recv().await;
        assert!(matches!(received, Err(WireError::FrameTooLarge(n)) if n == MAX_FRAME_LEN + 1));
    }

    #[tokio::test]
    async fn eof_is_closed() {
        let (mut conn, peer) = pair().await;
        drop(peer);
        assert!(matches!(conn.recv().await, Err(WireError::Closed)));
    }
}
