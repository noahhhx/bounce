//! Turns one source's PCM into timestamped [`AudioChunk`]s and fans them
//! out to every client in a zone routed to that source. One broadcaster per
//! source is what keeps zones on the same source in sync. This is the hot
//! path, so chunks go straight to each session's [`Outbox`] and do not pass
//! through the hub.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bounce_common::audio::{AudioChunk, AudioFormat};
use bounce_common::ids::{ClientId, StreamId};
use bounce_common::time::ServerInstant;

use crate::session::Outbox;

pub struct Broadcaster {
    stream: StreamId,
    format: AudioFormat,
    /// How far ahead of now each chunk's `play_at` is set. Bigger survives
    /// worse Wi-Fi; smaller makes play/pause feel responsive. Snapcast
    /// defaults to 1s.
    playout_delay: Duration,
    next_seq: u64,
    /// Chunks whose `play_at` has not passed yet, replayed to late joiners.
    pending: VecDeque<AudioChunk>,
    subscribers: HashMap<ClientId, Outbox>,
}

impl Broadcaster {
    #[must_use]
    pub fn new(stream: StreamId, format: AudioFormat, playout_delay: Duration) -> Self {
        Self {
            stream,
            format,
            playout_delay,
            next_seq: 0,
            pending: VecDeque::new(),
            subscribers: HashMap::new(),
        }
    }

    /// Encodes, stamps `play_at = now + playout_delay`, and sends to every
    /// subscriber. A subscriber whose outbox is full gets dropped chunks,
    /// never backpressure, so one slow client can't stall a room.
    pub fn push_pcm(&mut self, samples: &[i16], now: ServerInstant) {
        todo!()
    }

    /// A late joiner receives the chunks still ahead of their deadline, so
    /// it starts in sync instead of waiting a full playout delay.
    pub fn subscribe(&mut self, client: ClientId, outbox: Outbox) {
        todo!()
    }

    pub fn unsubscribe(&mut self, client: &ClientId) {
        todo!()
    }

    fn encode(&self, samples: &[i16]) -> AudioChunk {
        todo!()
    }
}
