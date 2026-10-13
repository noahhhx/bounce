//! Client logs and metrics history, queryable from the web UI.
//!
//! Records arrive stamped with time since client boot, because the device's
//! wall clock may be wrong. The store converts to wall time on receipt
//! using the session's clock offset, keeping the original stamp so ordering
//! within a boot is exact even if the conversion is off.

use std::collections::{HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::time::SystemTime;

use bounce_common::ids::{BootId, ClientId};
use bounce_common::telemetry::{Level, LogBatch, LogRecord, LogSeq, MetricsSample};

/// [`MemoryStore`] is the only implementation for now. A disk-backed one
/// (`SQLite`, files) implements this trait and nothing else changes. Queries
/// return owned values for that reason: a store that reads from disk has
/// nothing to borrow from.
pub trait TelemetryStore: Send {
    /// Skips records at or below the highest sequence already stored for
    /// this boot, so a batch resent after a reconnect doesn't duplicate.
    /// Returns the sequence to ack.
    fn append_logs(&mut self, client: &ClientId, batch: LogBatch, received: SystemTime) -> LogSeq;

    fn append_metrics(&mut self, client: &ClientId, sample: MetricsSample, received: SystemTime);

    fn query_logs(&self, query: &LogQuery) -> Vec<StoredLog>;

    fn metrics(&self, client: &ClientId, between: RangeInclusive<SystemTime>) -> Vec<StoredSample>;
}

#[derive(Debug, Clone)]
pub struct StoredLog {
    pub boot: BootId,
    pub wall: SystemTime,
    pub record: LogRecord,
}

#[derive(Debug, Clone)]
pub struct StoredSample {
    pub wall: SystemTime,
    pub sample: MetricsSample,
}

pub struct LogQuery {
    pub clients: Vec<ClientId>,
    pub min_level: Level,
    pub between: RangeInclusive<SystemTime>,
    pub text: Option<String>,
    pub limit: usize,
}

/// Fixed-size rings per client. Lost on server restart.
pub struct MemoryStore {
    logs: HashMap<ClientId, VecDeque<StoredLog>>,
    metrics: HashMap<ClientId, VecDeque<StoredSample>>,
    retention: Retention,
}

pub struct Retention {
    pub logs_per_client: usize,
    pub samples_per_client: usize,
}

impl TelemetryStore for MemoryStore {
    fn append_logs(&mut self, client: &ClientId, batch: LogBatch, received: SystemTime) -> LogSeq {
        todo!()
    }

    fn append_metrics(&mut self, client: &ClientId, sample: MetricsSample, received: SystemTime) {
        todo!()
    }

    fn query_logs(&self, query: &LogQuery) -> Vec<StoredLog> {
        todo!()
    }

    fn metrics(&self, client: &ClientId, between: RangeInclusive<SystemTime>) -> Vec<StoredSample> {
        todo!()
    }
}
