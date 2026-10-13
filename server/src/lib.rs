//! Task layout, on tokio because librespot needs it:
//!
//! - One [`hub::Hub`] owns all fleet, zone, source, health and release
//!   state, and is the only writer to it. Everything else sends it
//!   [`hub::HubEvent`]s and acts on the [`hub::Effect`]s it returns. No
//!   locks.
//! - One [`session`] task per connected client, generic over
//!   [`transport::Connection`].
//! - One [`source::Source`] and one [`broadcast::Broadcaster`] per source.
//!   Today that is a single Spotify Connect device; the web UI routes it to
//!   zones. Audio goes source to broadcaster to session outboxes, never
//!   through the hub.
//! - One task owns the [`telemetry::TelemetryStore`] and ingests logs and
//!   metrics, which are too high-volume to route through the hub.

#![expect(
    unused_variables,
    dead_code,
    clippy::needless_pass_by_value,
    reason = "sketch: bodies are todo!()"
)]

pub mod broadcast;
pub mod fleet;
pub mod health;
pub mod hub;
pub mod session;
pub mod source;
pub mod telemetry;
pub mod transport;
pub mod updates;
