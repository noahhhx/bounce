//! Everything the server needs to debug a device nobody can SSH into
//! comfortably.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bounce_common::ids::BootId;
use bounce_common::protocol::Diagnostics;
use bounce_common::telemetry::{FaultKind, Level, LogBatch, LogRecord, LogSeq, MetricsSample};
use bounce_common::time::SinceBoot;

use crate::clock::ClockSync;
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

impl LogBuffer {
    pub fn push(&mut self, record: LogRecord) {
        todo!()
    }

    /// Everything unacked, oldest first, up to `max` records. Resending the
    /// same batch is harmless; the server dedupes by sequence.
    #[must_use]
    pub fn batch(&self, max: usize) -> Option<LogBatch> {
        todo!()
    }

    pub fn ack(&mut self, up_to: LogSeq) {
        todo!()
    }

    pub fn set_level(&mut self, level: Level) {
        self.level = level;
    }

    /// The newest records, for a crash report.
    #[must_use]
    pub fn tail(&self, n: usize) -> Vec<LogRecord> {
        todo!()
    }
}

/// A `tracing_subscriber::Layer` that formats each event into a
/// [`LogRecord`] and pushes it into the shared buffer. Installed alongside
/// the journald layer, so local `journalctl` keeps working.
pub struct ForwardLayer {
    buffer: Arc<Mutex<LogBuffer>>,
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
