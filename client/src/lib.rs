//! Thread layout, with no async runtime so the client stays small:
//!
//! - The network thread runs the [`link::Link`] and the [`Device`]. It does
//!   every non-audio thing.
//! - The audio thread owns the [`player::Player`] and the ALSA handle. It
//!   never touches the network or blocks on a lock the network thread holds.
//!   It reads the clock offset and writes playback counters through atomics.
//! - The `tracing` layer writes into the shared [`telemetry::LogBuffer`]
//!   from whatever thread logged.

#![expect(
    unused_variables,
    dead_code,
    clippy::needless_pass_by_value,
    reason = "sketch: bodies are todo!()"
)]

pub mod clock;
pub mod link;
pub mod player;
pub mod telemetry;
pub mod transport;
pub mod updater;

use std::sync::{Arc, Mutex};

use bounce_common::protocol::{ClientMessage, DesiredState, ServerMessage};
use bounce_common::time::SinceBoot;

use crate::clock::ClockSync;
use crate::player::PlayerHandle;
use crate::telemetry::{LogBuffer, MetricsCollector};
use crate::updater::Updater;

/// Converges the device on the server's desired state and produces
/// everything the server hears from it. The link feeds it messages and
/// ticks, and sends what it returns.
pub struct Device {
    /// `None` until the first `Desired` after connecting. The player stays
    /// silent until then.
    desired: Option<DesiredState>,
    clock: ClockSync,
    player: PlayerHandle,
    updater: Updater,
    logs: Arc<Mutex<LogBuffer>>,
    metrics: MetricsCollector,
}

impl Device {
    pub fn on_message(&mut self, message: ServerMessage, now: SinceBoot) -> Vec<ClientMessage> {
        todo!()
    }

    /// Called every second: time sync, metrics sample, log batch flush,
    /// update progress. Each one checks its own interval.
    pub fn tick(&mut self, now: SinceBoot) -> Vec<ClientMessage> {
        todo!()
    }

    /// Sent right after `Hello`: the previous boot's crash or rollback, if
    /// any.
    pub fn startup_faults(&mut self) -> Vec<ClientMessage> {
        todo!()
    }
}
