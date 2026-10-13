//! Derives each client's problems from its record. Problems are recomputed,
//! not toggled by events, so a missed event can't leave an alert stuck on.
//! The hub diffs the result against the active set to raise and clear
//! alerts.

use std::mem::Discriminant;
use std::time::{Duration, Instant};

use bounce_common::ids::{ClientId, Version};

use crate::fleet::ClientRecord;

#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    Offline {
        for_: Duration,
    },
    Overheating {
        millicelsius: i32,
    },
    Throttled,
    Undervoltage,
    WeakWifi {
        rssi_dbm: i8,
    },
    Underrunning {
        per_minute: f32,
    },
    ClockUnstable {
        jitter: Duration,
    },
    OutputUnavailable {
        device: String,
    },
    RecentlyCrashed {
        version: Version,
        message: String,
    },
    UpdateFailed {
        version: Version,
        reason: String,
    },
    UpdateRolledBack {
        from: Version,
    },
    /// Online but not converging on its desired revision.
    NotApplyingConfig {
        for_: Duration,
    },
}

/// Alerts are keyed by variant, so a temperature that moves from 81 to 82
/// degrees updates one alert instead of raising a second.
pub type AlertKey = (ClientId, Discriminant<Problem>);

pub struct Thresholds {
    pub heartbeat_timeout: Duration,
    /// How long a client must be offline before it's a problem. A reboot
    /// for an update shouldn't page anyone.
    pub offline_grace: Duration,
    pub max_soc_millicelsius: i32,
    pub min_wifi_rssi_dbm: i8,
    pub max_underruns_per_minute: f32,
    pub max_clock_jitter: Duration,
    pub crash_memory: Duration,
    pub converge_timeout: Duration,
}

type Rule = fn(&ClientRecord, &Thresholds, Instant) -> Option<Problem>;

/// Adding a check means adding a function here. Nothing else changes.
pub const RULES: &[Rule] = &[
    offline,
    overheating,
    throttled,
    undervoltage,
    weak_wifi,
    underrunning,
    clock_unstable,
    output_unavailable,
    recently_crashed,
    update_failed,
    update_rolled_back,
    not_applying_config,
];

#[must_use]
pub fn evaluate(record: &ClientRecord, thresholds: &Thresholds, now: Instant) -> Vec<Problem> {
    RULES
        .iter()
        .filter_map(|rule| rule(record, thresholds, now))
        .collect()
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub client: ClientId,
    pub problem: Problem,
    pub raised_at: Instant,
}

fn offline(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn overheating(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn throttled(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn undervoltage(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn weak_wifi(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn underrunning(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn clock_unstable(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn output_unavailable(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn recently_crashed(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn update_failed(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn update_rolled_back(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}

fn not_applying_config(record: &ClientRecord, t: &Thresholds, now: Instant) -> Option<Problem> {
    todo!()
}
