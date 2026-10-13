//! The server's HTTP listener. It serves release artifacts to updating
//! clients and the JSON API that the web UI and the verify harness use:
//!
//! - `GET /api/state` returns the latest [`StateView`].
//! - `POST /api/admin` takes one [`crate::hub::AdminRequest`] as JSON and
//!   answers `202 Accepted` once the hub has it queued. The change shows in
//!   the next `/api/state`, not in the response.
//! - `GET <artifact_path>` serves a published release.

use std::convert::Infallible;
use std::io;
use std::path::PathBuf;

use serde::Serialize;
use tokio::sync::{mpsc, watch};

use bounce_common::ids::{ClientId, SourceId, StreamId, Version, ZoneId};
use bounce_common::protocol::{DesiredState, ReportedState};
use bounce_common::zone::{Volume, Zone};

use crate::hub::HubEvent;
use crate::source::SourceKind;

#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub clients: Vec<ClientView>,
    pub zones: Vec<Zone>,
    pub sources: Vec<SourceView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientView {
    pub id: ClientId,
    pub hostname: String,
    pub version: Version,
    pub online: bool,
    pub zone: Option<ZoneId>,
    pub desired: DesiredState,
    pub reported: Option<ReportedState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    pub id: SourceId,
    pub kind: SourceKind,
    pub name: String,
    pub volume: Volume,
    /// `None` while idle.
    pub stream: Option<StreamId>,
}

/// # Errors
///
/// Only when accepting on `listener` fails.
pub async fn serve(
    listener: tokio::net::TcpListener,
    hub: mpsc::Sender<HubEvent>,
    state: watch::Receiver<StateView>,
    artifact_dir: PathBuf,
) -> io::Result<Infallible> {
    todo!()
}
