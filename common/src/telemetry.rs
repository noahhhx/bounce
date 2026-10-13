use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{BootId, Version};
use crate::time::{ClockOffset, SinceBoot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Monotonic within one [`BootId`]. The server acks the highest sequence it
/// stored, and the client drops everything at or below it. A batch resent
/// after a reconnect is then a no-op instead of a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogSeq(pub u64);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub seq: LogSeq,
    pub at: SinceBoot,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogBatch {
    pub boot: BootId,
    pub records: Vec<LogRecord>,
    /// Records the client's ring buffer evicted before they could be sent.
    /// Reported so a gap in the server's view is visible, not silent.
    pub dropped: u64,
}

/// One periodic sample. Doubles as the heartbeat: the server marks a client
/// lost when samples stop, independent of whether the socket noticed.
///
/// Counters are totals since boot, not deltas, so a lost sample loses
/// resolution but never loses a count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSample {
    pub at: SinceBoot,
    pub system: SystemMetrics,
    pub playback: PlaybackMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub cpu_load_1m: f32,
    pub mem_available_kib: u64,
    pub soc_temp_millicelsius: i32,
    pub throttled: bool,
    /// Absent on wired clients.
    pub wifi_rssi_dbm: Option<i8>,
    pub undervoltage_events_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackMetrics {
    pub buffered: Duration,
    pub underruns_total: u64,
    pub late_chunks_total: u64,
    pub clock_offset: ClockOffset,
    pub clock_jitter: Duration,
}

/// Something the client knows is wrong, sent immediately rather than
/// waiting to be inferred from logs or metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fault {
    pub at: SinceBoot,
    pub kind: FaultKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FaultKind {
    OutputDeviceLost {
        device: String,
    },
    UnderrunBurst {
        count: u64,
        over: Duration,
    },
    ClockSyncLost,
    DecodeError {
        detail: String,
    },
    /// The previous boot panicked. The client persists the panic message and
    /// its last log records to disk in the panic hook, then sends them on the
    /// next connect. This is the only telemetry that survives a crash.
    PreviousBootPanicked {
        boot: BootId,
        version: Version,
        message: String,
        last_logs: Vec<LogRecord>,
    },
    UpdateRolledBack {
        from: Version,
        to: Version,
        reason: String,
    },
}
