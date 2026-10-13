use serde::{Deserialize, Serialize};

use crate::ids::StreamId;
use crate::time::ServerInstant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub codec: Codec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    PcmS16Le,
    Flac,
    Opus,
}

/// Every client renders the chunk at `play_at` on the server clock. That
/// one deadline is the whole sync model: no client-to-client coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioChunk {
    pub stream: StreamId,
    pub seq: u64,
    pub play_at: ServerInstant,
    pub data: Vec<u8>,
}
