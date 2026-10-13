//! Turns one source's PCM into timestamped [`AudioChunk`]s and fans them
//! out to every client in a zone routed to that source. One broadcaster per
//! source is what keeps zones on the same source in sync. This is the hot
//! path, so chunks go straight to each session's [`Outbox`] and do not pass
//! through the hub.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bounce_common::audio::{AudioChunk, AudioFormat, Codec};
use bounce_common::ids::{ClientId, StreamId};
use bounce_common::time::ServerInstant;

use crate::session::Outbox;

/// How far the next chunk's slot may drift from `now + playout_delay`
/// before the timeline restarts there. Larger than the lead a source may
/// run ahead of real time, such as the Spotify sink's 150 ms.
const MAX_SLIP: Duration = Duration::from_millis(250);

pub struct Broadcaster {
    stream: StreamId,
    format: AudioFormat,
    /// How far ahead of now each chunk's `play_at` is set. Bigger survives
    /// worse Wi-Fi; smaller makes play/pause feel responsive. Snapcast
    /// defaults to 1s.
    playout_delay: Duration,
    next_seq: u64,
    timeline: Option<Timeline>,
    /// Chunks whose `play_at` has not passed yet, replayed to late joiners.
    pending: VecDeque<AudioChunk>,
    subscribers: HashMap<ClientId, Outbox>,
}

/// Counted in frames from a fixed start, so chunks that are not a whole
/// number of microseconds long never accumulate rounding error.
#[derive(Clone, Copy)]
struct Timeline {
    start: ServerInstant,
    frames: u64,
}

impl Timeline {
    fn end(self, sample_rate: u32) -> ServerInstant {
        ServerInstant(self.start.0 + self.frames * 1_000_000 / u64::from(sample_rate))
    }
}

impl Broadcaster {
    #[must_use]
    pub fn new(stream: StreamId, format: AudioFormat, playout_delay: Duration) -> Self {
        Self {
            stream,
            format,
            playout_delay,
            next_seq: 0,
            timeline: None,
            pending: VecDeque::new(),
            subscribers: HashMap::new(),
        }
    }

    /// Encodes, stamps, and sends to every subscriber. Each chunk plays
    /// right where the previous one ends, so uneven arrivals stay gapless.
    /// The timeline restarts at `now + playout_delay` on the first chunk and
    /// whenever the source drifts more than [`MAX_SLIP`] from it, as after
    /// a pause. A subscriber whose outbox is full gets dropped chunks,
    /// never backpressure, so one slow client can't stall a room.
    pub fn push_pcm(&mut self, samples: &[i16], now: ServerInstant) {
        let rate = self.format.sample_rate;
        let target = ServerInstant(now.0 + micros(self.playout_delay));
        let timeline = match self.timeline {
            Some(t) if t.end(rate).0.abs_diff(target.0) <= micros(MAX_SLIP) => t,
            _ => Timeline {
                start: target,
                frames: 0,
            },
        };
        let chunk = AudioChunk {
            stream: self.stream,
            seq: self.next_seq,
            play_at: timeline.end(rate),
            data: self.encode(samples),
        };
        let frames = samples.len() / usize::from(self.format.channels);
        self.timeline = Some(Timeline {
            frames: timeline.frames + frames as u64,
            ..timeline
        });
        self.next_seq += 1;

        while self.pending.front().is_some_and(|c| c.play_at <= now) {
            self.pending.pop_front();
        }
        for outbox in self.subscribers.values() {
            outbox.audio(chunk.clone());
        }
        self.pending.push_back(chunk);
    }

    /// A late joiner receives the chunks still ahead of their deadline, so
    /// it starts in sync instead of waiting a full playout delay.
    pub fn subscribe(&mut self, client: ClientId, outbox: Outbox) {
        for chunk in &self.pending {
            outbox.audio(chunk.clone());
        }
        self.subscribers.insert(client, outbox);
    }

    pub fn unsubscribe(&mut self, client: &ClientId) {
        self.subscribers.remove(client);
    }

    fn encode(&self, samples: &[i16]) -> Vec<u8> {
        match self.format.codec {
            Codec::PcmS16Le => samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
            Codec::Flac | Codec::Opus => todo!(),
        }
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    const TEN_MS: [i16; 2 * 480] = [0; 2 * 480];

    fn broadcaster() -> Broadcaster {
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 2,
            codec: Codec::PcmS16Le,
        };
        Broadcaster::new(StreamId(7), format, Duration::from_secs(1))
    }

    fn client(n: u8) -> ClientId {
        ClientId::try_from(format!("{n:032x}")).unwrap()
    }

    fn at_ms(ms: u64) -> ServerInstant {
        ServerInstant(ms * 1_000)
    }

    fn subscribe(b: &mut Broadcaster, n: u8) -> mpsc::Receiver<AudioChunk> {
        let (outbox, _, audio) = Outbox::channel();
        b.subscribe(client(n), outbox);
        audio
    }

    fn drain(audio: &mut mpsc::Receiver<AudioChunk>) -> Vec<AudioChunk> {
        std::iter::from_fn(|| audio.try_recv().ok()).collect()
    }

    #[test]
    fn uneven_arrivals_play_back_to_back() {
        let mut b = broadcaster();
        let mut audio = subscribe(&mut b, 1);
        for ms in [0, 2, 31, 33, 180] {
            b.push_pcm(&TEN_MS, at_ms(ms));
        }
        let play_at: Vec<u64> = drain(&mut audio).iter().map(|c| c.play_at.0).collect();
        assert_eq!(
            play_at,
            [1_000_000, 1_010_000, 1_020_000, 1_030_000, 1_040_000]
        );
    }

    #[test]
    fn restarts_the_timeline_after_a_stall() {
        let mut b = broadcaster();
        let mut audio = subscribe(&mut b, 1);
        b.push_pcm(&TEN_MS, at_ms(0));
        b.push_pcm(&TEN_MS, at_ms(2_000));
        let chunks: Vec<(u64, u64)> = drain(&mut audio)
            .iter()
            .map(|c| (c.seq, c.play_at.0))
            .collect();
        assert_eq!(chunks, [(0, 1_000_000), (1, 3_000_000)]);
    }

    #[test]
    fn sends_little_endian_pcm_on_its_stream() {
        let mut b = broadcaster();
        let mut audio = subscribe(&mut b, 1);
        b.push_pcm(&[1, -2], at_ms(0));
        let chunk = drain(&mut audio).pop().unwrap();
        assert_eq!(chunk.stream, StreamId(7));
        assert_eq!(chunk.data, [0x01, 0x00, 0xfe, 0xff]);
    }

    #[test]
    fn late_joiner_gets_only_chunks_not_yet_due() {
        let mut b = broadcaster();
        for i in 0..150 {
            b.push_pcm(&TEN_MS, at_ms(i * 10));
        }
        let mut audio = subscribe(&mut b, 1);
        let seqs: Vec<u64> = drain(&mut audio).iter().map(|c| c.seq).collect();
        assert_eq!(seqs, (50..150).collect::<Vec<_>>());
    }

    #[test]
    fn a_full_outbox_drops_chunks_and_holds_back_no_one() {
        let mut b = broadcaster();
        let mut stalled = subscribe(&mut b, 1);
        let mut keeping_up = subscribe(&mut b, 2);
        let mut received = 0;
        for i in 0..300 {
            b.push_pcm(&TEN_MS, at_ms(i * 10));
            received += drain(&mut keeping_up).len();
        }
        assert_eq!(received, 300);
        let stalled_seqs: Vec<u64> = drain(&mut stalled).iter().map(|c| c.seq).collect();
        assert_eq!(stalled_seqs, (0..256).collect::<Vec<_>>());
    }

    #[test]
    fn unsubscribed_client_gets_nothing_more() {
        let mut b = broadcaster();
        let mut audio = subscribe(&mut b, 1);
        b.push_pcm(&TEN_MS, at_ms(0));
        b.unsubscribe(&client(1));
        b.push_pcm(&TEN_MS, at_ms(10));
        assert_eq!(drain(&mut audio).len(), 1);
    }
}
