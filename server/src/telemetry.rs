//! Client logs and metrics history, queryable from the web UI.
//!
//! Records arrive stamped with time since client boot, because the device's
//! wall clock may be wrong. The store converts to wall time on receipt.

use std::collections::{HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::time::SystemTime;

use tokio::sync::{mpsc, oneshot};

use bounce_common::ids::{BootId, ClientId};
use bounce_common::telemetry::{Level, LogBatch, LogRecord, LogSeq, MetricsSample};
use bounce_common::time::SinceBoot;

/// [`MemoryStore`] is the only implementation for now. A disk-backed one
/// (`SQLite`, files) implements this trait and nothing else changes. Queries
/// return owned values for that reason: a store that reads from disk has
/// nothing to borrow from.
pub trait TelemetryStore: Send {
    /// Skips records at or below the highest sequence already stored for
    /// this boot, so a batch resent after a reconnect doesn't duplicate.
    /// Returns the sequence to ack.
    fn append_logs(&mut self, client: &ClientId, batch: LogBatch, clock: ClientClock) -> LogSeq;

    fn append_metrics(&mut self, client: &ClientId, sample: MetricsSample, received: SystemTime);

    /// Oldest first, keeping the newest `limit` matches.
    fn query_logs(&self, query: &LogQuery) -> Vec<StoredLog>;

    fn metrics(&self, client: &ClientId, between: RangeInclusive<SystemTime>) -> Vec<StoredSample>;
}

#[derive(Debug, Clone, Copy)]
pub struct ClientClock {
    pub since_boot: SinceBoot,
    pub wall: SystemTime,
}

impl ClientClock {
    #[must_use]
    pub fn wall(&self, at: SinceBoot) -> SystemTime {
        let gap = at.0.abs_diff(self.since_boot.0);
        if at >= self.since_boot {
            self.wall + gap
        } else {
            self.wall - gap
        }
    }
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

impl LogQuery {
    fn matches(&self, log: &StoredLog) -> bool {
        log.record.level >= self.min_level
            && self.between.contains(&log.wall)
            && self
                .text
                .as_ref()
                .is_none_or(|text| log.record.message.contains(text.as_str()))
    }
}

/// Fixed-size rings per client. Lost on server restart.
pub struct MemoryStore {
    logs: HashMap<ClientId, ClientLogs>,
    metrics: HashMap<ClientId, VecDeque<StoredSample>>,
    retention: Retention,
}

#[derive(Default)]
struct ClientLogs {
    records: VecDeque<StoredLog>,
    stored_up_to: Option<(BootId, LogSeq)>,
}

pub struct Retention {
    pub logs_per_client: usize,
    pub samples_per_client: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            logs_per_client: 10_000,
            samples_per_client: 3_600,
        }
    }
}

impl MemoryStore {
    #[must_use]
    pub fn new(retention: Retention) -> Self {
        Self {
            logs: HashMap::new(),
            metrics: HashMap::new(),
            retention,
        }
    }
}

impl TelemetryStore for MemoryStore {
    fn append_logs(&mut self, client: &ClientId, batch: LogBatch, clock: ClientClock) -> LogSeq {
        let logs = self.logs.entry(client.clone()).or_default();
        let mut high = logs
            .stored_up_to
            .filter(|(boot, _)| *boot == batch.boot)
            .map(|(_, seq)| seq);
        for record in batch.records {
            if high.is_some_and(|high| record.seq <= high) {
                continue;
            }
            high = Some(record.seq);
            logs.records.push_back(StoredLog {
                boot: batch.boot,
                wall: clock.wall(record.at),
                record,
            });
            if logs.records.len() > self.retention.logs_per_client {
                logs.records.pop_front();
            }
        }
        let Some(high) = high else {
            return LogSeq(0);
        };
        logs.stored_up_to = Some((batch.boot, high));
        high
    }

    fn append_metrics(&mut self, client: &ClientId, sample: MetricsSample, received: SystemTime) {
        todo!()
    }

    fn query_logs(&self, query: &LogQuery) -> Vec<StoredLog> {
        let mut matches: Vec<StoredLog> = query
            .clients
            .iter()
            .filter_map(|client| self.logs.get(client))
            .flat_map(|logs| logs.records.iter().filter(|log| query.matches(log)))
            .cloned()
            .collect();
        matches.sort_by_key(|log| log.wall);
        matches.split_off(matches.len().saturating_sub(query.limit))
    }

    fn metrics(&self, client: &ClientId, between: RangeInclusive<SystemTime>) -> Vec<StoredSample> {
        todo!()
    }
}

#[derive(Clone)]
pub struct Telemetry(mpsc::UnboundedSender<Request>);

enum Request {
    AppendLogs {
        client: ClientId,
        batch: LogBatch,
        clock: ClientClock,
        reply: oneshot::Sender<LogSeq>,
    },
    QueryLogs {
        query: LogQuery,
        reply: oneshot::Sender<Vec<StoredLog>>,
    },
}

impl Telemetry {
    pub fn spawn(mut store: impl TelemetryStore + 'static) -> Self {
        let (requests_tx, mut requests) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                match request {
                    Request::AppendLogs {
                        client,
                        batch,
                        clock,
                        reply,
                    } => {
                        let _ = reply.send(store.append_logs(&client, batch, clock));
                    }
                    Request::QueryLogs { query, reply } => {
                        let _ = reply.send(store.query_logs(&query));
                    }
                }
            }
        });
        Self(requests_tx)
    }

    pub async fn append_logs(
        &self,
        client: ClientId,
        batch: LogBatch,
        clock: ClientClock,
    ) -> Option<LogSeq> {
        let (reply, answer) = oneshot::channel();
        let request = Request::AppendLogs {
            client,
            batch,
            clock,
            reply,
        };
        self.0.send(request).ok()?;
        answer.await.ok()
    }

    pub async fn query_logs(&self, query: LogQuery) -> Option<Vec<StoredLog>> {
        let (reply, answer) = oneshot::channel();
        self.0.send(Request::QueryLogs { query, reply }).ok()?;
        answer.await.ok()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn client(n: u8) -> ClientId {
        ClientId::parse(&format!("b0c000000000000000000000000000{n:02x}")).unwrap()
    }

    fn record(seq: u64, level: Level) -> LogRecord {
        LogRecord {
            seq: LogSeq(seq),
            at: SinceBoot(Duration::from_secs(seq)),
            level,
            target: "t".into(),
            message: format!("record {seq}"),
            fields: Vec::new(),
        }
    }

    fn batch(boot: u64, seqs: impl IntoIterator<Item = u64>) -> LogBatch {
        LogBatch {
            boot: BootId(boot),
            records: seqs.into_iter().map(|s| record(s, Level::Info)).collect(),
            dropped: 0,
        }
    }

    fn clock() -> ClientClock {
        ClientClock {
            since_boot: SinceBoot(Duration::from_secs(10)),
            wall: UNIX_EPOCH + Duration::from_secs(1_000),
        }
    }

    fn all(clients: Vec<ClientId>) -> LogQuery {
        LogQuery {
            clients,
            min_level: Level::Trace,
            between: UNIX_EPOCH..=UNIX_EPOCH + Duration::from_secs(1_000_000),
            text: None,
            limit: usize::MAX,
        }
    }

    fn stored(store: &MemoryStore, query: &LogQuery) -> Vec<(u64, u64)> {
        store
            .query_logs(query)
            .iter()
            .map(|log| (log.boot.0, log.record.seq.0))
            .collect()
    }

    fn store(logs_per_client: usize) -> MemoryStore {
        MemoryStore::new(Retention {
            logs_per_client,
            samples_per_client: 0,
        })
    }

    #[test]
    fn a_resent_batch_is_acked_again_and_not_duplicated() {
        let mut store = store(100);
        let c = client(1);
        assert_eq!(store.append_logs(&c, batch(1, 1..=3), clock()), LogSeq(3));
        assert_eq!(store.append_logs(&c, batch(1, 1..=3), clock()), LogSeq(3));
        assert_eq!(store.append_logs(&c, batch(1, 2..=5), clock()), LogSeq(5));
        assert_eq!(
            stored(&store, &all(vec![c])),
            [(1, 1), (1, 2), (1, 3), (1, 4), (1, 5)]
        );
    }

    #[test]
    fn a_new_boot_starts_its_sequence_fresh() {
        let mut store = store(100);
        let c = client(1);
        store.append_logs(&c, batch(1, 1..=5), clock());
        let rebooted = ClientClock {
            wall: clock().wall + Duration::from_secs(60),
            ..clock()
        };
        assert_eq!(store.append_logs(&c, batch(2, 1..=2), rebooted), LogSeq(2));
        assert_eq!(
            stored(&store, &all(vec![c]))[5..],
            [(2, 1), (2, 2)],
            "the new boot's records follow the old boot's"
        );
    }

    #[test]
    fn an_empty_batch_acks_nothing() {
        let mut store = store(100);
        assert_eq!(
            store.append_logs(&client(1), batch(1, []), clock()),
            LogSeq(0)
        );
    }

    #[test]
    fn each_client_keeps_its_newest_records_up_to_retention() {
        let mut store = store(3);
        store.append_logs(&client(1), batch(1, 1..=5), clock());
        store.append_logs(&client(2), batch(9, 1..=1), clock());
        assert_eq!(
            stored(&store, &all(vec![client(1)])),
            [(1, 3), (1, 4), (1, 5)]
        );
        assert_eq!(stored(&store, &all(vec![client(2)])), [(9, 1)]);
        assert!(store.query_logs(&all(vec![client(3)])).is_empty());
    }

    #[test]
    fn wall_time_comes_from_the_client_clock() {
        let mut store = store(100);
        store.append_logs(&client(1), batch(1, [4, 12]), clock());
        let walls: Vec<_> = store
            .query_logs(&all(vec![client(1)]))
            .iter()
            .map(|log| log.wall.duration_since(UNIX_EPOCH).unwrap().as_secs())
            .collect();
        assert_eq!(walls, [994, 1_002]);
    }

    #[test]
    fn a_query_filters_then_keeps_the_newest_up_to_its_limit() {
        let mut store = store(100);
        let c = client(1);
        let mut mixed = batch(1, 1..=6);
        mixed.records[1].level = Level::Debug;
        store.append_logs(&c, mixed, clock());

        let query = LogQuery {
            min_level: Level::Info,
            limit: 3,
            ..all(vec![c.clone()])
        };
        assert_eq!(stored(&store, &query), [(1, 4), (1, 5), (1, 6)]);

        let query = LogQuery {
            text: Some("record 2".into()),
            ..all(vec![c.clone()])
        };
        assert_eq!(stored(&store, &query), [(1, 2)]);

        let first_two = UNIX_EPOCH..=clock().wall(SinceBoot(Duration::from_secs(2)));
        let query = LogQuery {
            between: first_two,
            ..all(vec![c])
        };
        assert_eq!(stored(&store, &query), [(1, 1), (1, 2)]);
    }

    #[tokio::test]
    async fn the_owning_task_answers_appends_and_queries() {
        let telemetry = Telemetry::spawn(store(100));
        let acked = telemetry.append_logs(client(1), batch(1, 1..=2), clock());
        assert_eq!(acked.await, Some(LogSeq(2)));
        let logs = telemetry.query_logs(all(vec![client(1)])).await.unwrap();
        assert_eq!(logs.len(), 2);
    }
}
