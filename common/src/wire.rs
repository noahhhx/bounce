//! How a message becomes bytes. Every transport on both sides goes through
//! these two functions, so changing the encoding is a change to this file
//! alone.
//!
//! A frame is a little-endian `u32` length followed by a postcard-encoded
//! message. postcard is compact and cheap to decode on a Pi Zero, and its
//! format is stable across releases, which the frozen [`crate::protocol`]
//! core depends on.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Larger than any audio chunk or log batch. A length above this means the
/// stream is corrupt or isn't speaking this protocol.
pub const MAX_FRAME_LEN: u32 = 1 << 20;

#[derive(Debug)]
pub enum WireError {
    Io(std::io::Error),
    FrameTooLarge(u32),
    Decode(postcard::Error),
    Closed,
}

/// Returns the full frame, length prefix included.
///
/// # Errors
///
/// Only if the message encodes to more than [`MAX_FRAME_LEN`] bytes.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, WireError> {
    // Only a broken protocol type fails to serialize. `Decode` is the one
    // variant that keeps the postcard error.
    let body = postcard::to_allocvec(message).map_err(WireError::Decode)?;
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    if len > MAX_FRAME_LEN {
        return Err(WireError::FrameTooLarge(len));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Takes the frame body, without the length prefix.
///
/// # Errors
///
/// [`WireError::Decode`] when the body isn't a valid `T`.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, WireError> {
    postcard::from_bytes(body).map_err(WireError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioChunk, Codec};
    use crate::ids::{BootId, ClientId, StreamId, Version};
    use crate::protocol::{ClientMessage, Hello, OutputDevice, ServerMessage};
    use crate::time::ServerInstant;
    use crate::update::Target;

    fn assert_round_trips<T>(message: &T)
    where
        T: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        let frame = encode(message).unwrap();
        let prefix = u32::from_le_bytes(frame[..4].try_into().unwrap());
        assert_eq!(prefix as usize, frame.len() - 4);
        let back: T = decode(&frame[4..]).unwrap();
        assert_eq!(format!("{back:?}"), format!("{message:?}"));
    }

    fn chunk(len: usize) -> AudioChunk {
        AudioChunk {
            stream: StreamId(7),
            seq: 42,
            play_at: ServerInstant(123_456),
            data: vec![0xAB; len],
        }
    }

    #[test]
    fn server_message_round_trips() {
        assert_round_trips(&ServerMessage::Audio(chunk(1000)));
    }

    #[test]
    fn client_message_round_trips() {
        let hello = Hello {
            protocol: 1,
            client: ClientId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            boot: BootId(9),
            version: Version {
                major: 1,
                minor: 2,
                patch: 3,
            },
            target: Target::Aarch64,
            hostname: "kitchen".into(),
            outputs: vec![OutputDevice {
                id: "hw:CARD=x".into(),
                description: "DAC".into(),
            }],
            codecs: vec![Codec::PcmS16Le, Codec::Opus],
        };
        assert_round_trips(&ClientMessage::Hello(hello));
    }

    #[test]
    fn oversize_message_is_frame_too_large() {
        let message = ServerMessage::Audio(chunk(MAX_FRAME_LEN as usize));
        assert!(matches!(
            encode(&message),
            Err(WireError::FrameTooLarge(n)) if n > MAX_FRAME_LEN
        ));
    }

    #[test]
    fn garbage_is_decode_error() {
        let result = decode::<ServerMessage>(&[0xFF; 16]);
        assert!(matches!(result, Err(WireError::Decode(_))));
    }
}
