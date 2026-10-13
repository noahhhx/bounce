//! Everything the server needs to debug a device nobody can SSH into
//! comfortably.

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bounce_common::ids::BootId;
use bounce_common::protocol::Diagnostics;
use bounce_common::telemetry::{FaultKind, Level, LogBatch, LogRecord, LogSeq, MetricsSample};
use bounce_common::time::SinceBoot;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::clock::{self, ClockSync};
use crate::player::PlaybackCounters;

/// A bounded ring of records not yet acked by the server. Kept in RAM:
/// writing every log line to an SD card wears it out. When full, the oldest
/// record is evicted and counted in `dropped`.
pub struct LogBuffer {
    boot: BootId,
    records: VecDeque<LogRecord>,
    next_seq: LogSeq,
    capacity: usize,
    dropped: u64,
    /// From the desired state. Records below it are never buffered.
    level: Level,
}

pub const LOG_CAPACITY: usize = 4096;

impl LogBuffer {
    #[must_use]
    pub fn new(boot: BootId, capacity: usize) -> Self {
        Self {
            boot,
            records: VecDeque::new(),
            next_seq: LogSeq(1),
            capacity,
            dropped: 0,
            level: Level::Info,
        }
    }

    pub fn push(
        &mut self,
        at: SinceBoot,
        level: Level,
        target: String,
        message: String,
        fields: Vec<(String, String)>,
    ) {
        if level < self.level {
            return;
        }
        let seq = self.next_seq;
        self.next_seq = LogSeq(seq.0 + 1);
        self.records.push_back(LogRecord {
            seq,
            at,
            level,
            target,
            message,
            fields,
        });
        if self.records.len() > self.capacity {
            self.records.pop_front();
            self.dropped += 1;
        }
    }

    /// Everything unacked, oldest first, up to `max` records. Resending the
    /// same batch is harmless; the server dedupes by sequence.
    #[must_use]
    pub fn batch(&self, max: usize) -> Option<LogBatch> {
        if self.records.is_empty() {
            return None;
        }
        Some(LogBatch {
            boot: self.boot,
            records: self.records.iter().take(max).cloned().collect(),
            dropped: self.dropped,
        })
    }

    pub fn ack(&mut self, boot: BootId, up_to: LogSeq) {
        if boot != self.boot {
            return;
        }
        while self.records.front().is_some_and(|r| r.seq <= up_to) {
            self.records.pop_front();
        }
    }

    pub fn set_level(&mut self, level: Level) {
        self.level = level;
    }

    /// The newest records, for a crash report.
    #[must_use]
    pub fn tail(&self, n: usize) -> Vec<LogRecord> {
        let skip = self.records.len().saturating_sub(n);
        self.records.iter().skip(skip).cloned().collect()
    }
}

pub fn lock(buffer: &Mutex<LogBuffer>) -> MutexGuard<'_, LogBuffer> {
    buffer.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `tracing_subscriber::Layer` that formats each event into a
/// [`LogRecord`] and pushes it into the shared buffer.
pub struct ForwardLayer {
    buffer: Arc<Mutex<LogBuffer>>,
}

const MAX_TEXT: usize = 1024;

impl ForwardLayer {
    #[must_use]
    pub fn new(buffer: Arc<Mutex<LogBuffer>>) -> Self {
        Self { buffer }
    }
}

impl<S: Subscriber> Layer<S> for ForwardLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let at = clock::now();
        let meta = event.metadata();
        let mut visitor = Fields::default();
        event.record(&mut visitor);
        lock(&self.buffer).push(
            at,
            level(*meta.level()),
            meta.target().to_owned(),
            visitor.message,
            visitor.fields,
        );
    }
}

fn level(level: tracing::Level) -> Level {
    match level {
        tracing::Level::TRACE => Level::Trace,
        tracing::Level::DEBUG => Level::Debug,
        tracing::Level::INFO => Level::Info,
        tracing::Level::WARN => Level::Warn,
        tracing::Level::ERROR => Level::Error,
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<(String, String)>,
}

impl Fields {
    fn add(&mut self, field: &Field, mut value: String) {
        value.truncate(value.floor_char_boundary(MAX_TEXT));
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_owned(), value));
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.add(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.add(field, format!("{value:?}"));
    }
}

/// Reads the Pi's vitals: `/proc/loadavg`, `/proc/meminfo`,
/// `/sys/class/thermal/thermal_zone0/temp`, `/proc/net/wireless`, and
/// `vcgencmd get_throttled` for undervoltage, the most common Pi Zero fault.
pub struct MetricsCollector {}

impl MetricsCollector {
    pub fn sample(
        &mut self,
        now: SinceBoot,
        playback: &PlaybackCounters,
        clock: &ClockSync,
    ) -> MetricsSample {
        todo!()
    }

    /// For [`bounce_common::protocol::Command::CollectDiagnostics`]. Includes
    /// `journalctl -b -1`, which covers what the panic hook can't catch: OOM
    /// kills, segfaults, power loss.
    pub fn diagnostics(&mut self, request: u64) -> Diagnostics {
        todo!()
    }
}

/// Persists a panic to disk from the panic hook, so the next boot can
/// report it. The only disk write telemetry does.
pub struct CrashRecorder {
    path: PathBuf,
}

impl CrashRecorder {
    pub fn install(path: PathBuf, logs: Arc<Mutex<LogBuffer>>) -> Self {
        todo!()
    }

    /// Reads and deletes the previous boot's crash file.
    #[must_use]
    pub fn take_previous(&self) -> Option<FaultKind> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    const BOOT: BootId = BootId(7);

    fn push(buffer: &mut LogBuffer, level: Level, message: &str) {
        let at = SinceBoot(Duration::ZERO);
        buffer.push(at, level, "t".into(), message.into(), Vec::new());
    }

    fn seqs(buffer: &LogBuffer) -> Vec<u64> {
        buffer.tail(usize::MAX).iter().map(|r| r.seq.0).collect()
    }

    #[test]
    fn ack_drops_at_and_below_and_sequences_keep_counting() {
        let mut buffer = LogBuffer::new(BOOT, 10);
        for message in ["a", "b", "c"] {
            push(&mut buffer, Level::Info, message);
        }
        assert_eq!(seqs(&buffer), [1, 2, 3]);

        buffer.ack(BOOT, LogSeq(2));
        assert_eq!(seqs(&buffer), [3]);
        buffer.ack(BOOT, LogSeq(2));
        assert_eq!(seqs(&buffer), [3]);

        push(&mut buffer, Level::Info, "d");
        assert_eq!(seqs(&buffer), [3, 4]);
        buffer.ack(BOOT, LogSeq(4));
        assert!(buffer.batch(10).is_none());
    }

    #[test]
    fn an_ack_for_another_boot_drops_nothing() {
        let mut buffer = LogBuffer::new(BOOT, 10);
        push(&mut buffer, Level::Info, "a");
        buffer.ack(BootId(8), LogSeq(1));
        assert_eq!(seqs(&buffer), [1]);
    }

    #[test]
    fn a_full_buffer_evicts_the_oldest_and_counts_it() {
        let mut buffer = LogBuffer::new(BOOT, 2);
        for message in ["a", "b", "c", "d"] {
            push(&mut buffer, Level::Info, message);
        }
        let batch = buffer.batch(10).unwrap();
        assert_eq!(batch.boot, BOOT);
        assert_eq!(batch.dropped, 2);
        let messages: Vec<_> = batch.records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, ["c", "d"]);
        assert_eq!(seqs(&buffer), [3, 4]);
    }

    #[test]
    fn records_below_the_level_are_not_buffered_and_take_no_sequence() {
        let mut buffer = LogBuffer::new(BOOT, 10);
        push(&mut buffer, Level::Debug, "skipped");
        push(&mut buffer, Level::Warn, "kept");
        buffer.set_level(Level::Debug);
        push(&mut buffer, Level::Debug, "now kept");
        push(&mut buffer, Level::Trace, "still skipped");
        let messages: Vec<_> = buffer.tail(10).into_iter().map(|r| r.message).collect();
        assert_eq!(messages, ["kept", "now kept"]);
        assert_eq!(seqs(&buffer), [1, 2]);
    }

    #[test]
    fn batch_is_the_oldest_up_to_max_and_tail_the_newest() {
        let mut buffer = LogBuffer::new(BOOT, 10);
        for message in ["a", "b", "c"] {
            push(&mut buffer, Level::Info, message);
        }
        let batch = buffer.batch(2).unwrap();
        assert_eq!(
            batch.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            buffer.tail(2).iter().map(|r| r.seq.0).collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn the_layer_turns_an_event_into_a_record() {
        let buffer = Arc::new(Mutex::new(LogBuffer::new(BOOT, 10)));
        let subscriber = tracing_subscriber::registry().with(ForwardLayer::new(buffer.clone()));
        let long = "x".repeat(MAX_TEXT + 10);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(n = 3, name = "kitchen", "hello {}", "there");
            tracing::debug!("below the level");
            tracing::warn!(%long, "long");
        });

        let records = lock(&buffer).tail(10);
        assert_eq!(records.len(), 2, "{records:?}");
        let hello = &records[0];
        assert_eq!(hello.level, Level::Info);
        assert_eq!(hello.target, module_path!());
        assert_eq!(hello.message, "hello there");
        assert_eq!(
            hello.fields,
            [("n".into(), "3".into()), ("name".into(), "kitchen".into())]
        );
        assert_eq!(records[1].fields[0].1.len(), MAX_TEXT);
    }
}
