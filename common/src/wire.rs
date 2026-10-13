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
    todo!()
}

/// Takes the frame body, without the length prefix.
///
/// # Errors
///
/// [`WireError::Decode`] when the body isn't a valid `T`.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, WireError> {
    todo!()
}
