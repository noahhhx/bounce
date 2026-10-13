//! The server's HTTP listener. It serves release artifacts to updating
//! clients and the JSON API that the web UI and the verify harness use:
//!
//! - `GET /api/state` returns the latest [`StateView`].
//! - `POST /api/admin` takes one [`crate::hub::AdminRequest`] as JSON and
//!   answers `202 Accepted` once the hub has it queued. The change shows in
//!   the next `/api/state`, not in the response.
//! - `GET /api/logs?client=<32 hex>&limit=<n>` returns that client's newest
//!   `n` stored log records (default 100) as [`LogView`] JSON, newest last.
//! - `GET <artifact_path>` serves a published release.

use std::convert::Infallible;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::debug;
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use bounce_common::ids::{ClientId, SourceId, StreamId, Version, ZoneId};
use bounce_common::protocol::{DesiredState, ReportedState};
use bounce_common::telemetry::Level;
use bounce_common::zone::{Volume, Zone};

use crate::hub::{AdminRequest, HubEvent};
use crate::source::SourceKind;
use crate::telemetry::{LogQuery, StoredLog, Telemetry};

/// Far above any admin request. Caps what one request can make the server
/// buffer.
const MAX_ADMIN_BODY: usize = 64 * 1024;

const DEFAULT_LOG_LIMIT: usize = 100;

type Reply = Response<Full<Bytes>>;

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
pub struct LogView {
    /// Hex, since a JSON number loses the low bits of a random `u64`.
    pub boot: String,
    pub seq: u64,
    /// Microseconds since the Unix epoch, converted on receipt from the
    /// client's time since boot.
    pub wall_us: u64,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

impl From<StoredLog> for LogView {
    fn from(log: StoredLog) -> Self {
        let since_epoch = log.wall.duration_since(UNIX_EPOCH).unwrap_or_default();
        Self {
            boot: format!("{:016x}", log.boot.0),
            seq: log.record.seq.0,
            wall_us: u64::try_from(since_epoch.as_micros()).unwrap_or(u64::MAX),
            level: log.record.level,
            target: log.record.target,
            message: log.record.message,
            fields: log.record.fields,
        }
    }
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

/// One task per connection. A failed connection is logged and dropped.
///
/// # Errors
///
/// Only when accepting on `listener` fails.
pub async fn serve(
    listener: tokio::net::TcpListener,
    hub: mpsc::UnboundedSender<HubEvent>,
    state: watch::Receiver<StateView>,
    artifact_dir: PathBuf,
    telemetry: Telemetry,
) -> io::Result<Infallible> {
    let api = Api {
        hub,
        state,
        artifact_dir: Arc::from(artifact_dir),
        telemetry,
    };
    loop {
        let (stream, peer) = listener.accept().await?;
        let api = api.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| api.clone().respond(request));
            let connection = http1::Builder::new().serve_connection(TokioIo::new(stream), service);
            if let Err(error) = connection.await {
                debug!("http connection from {peer}: {error}");
            }
        });
    }
}

#[derive(Clone)]
struct Api {
    hub: mpsc::UnboundedSender<HubEvent>,
    state: watch::Receiver<StateView>,
    artifact_dir: Arc<Path>,
    telemetry: Telemetry,
}

impl Api {
    async fn respond(self, request: Request<Incoming>) -> Result<Reply, Infallible> {
        let (method, path) = (request.method().clone(), request.uri().path().to_owned());
        Ok(match (method, path.as_str()) {
            (Method::GET, "/api/state") => self.state(),
            (Method::GET, "/api/logs") => self.logs(request.uri().query()).await,
            (Method::POST, "/api/admin") => self.admin(request.into_body()).await,
            (Method::GET, path) => self.artifact(path).await,
            _ => text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        })
    }

    fn state(&self) -> Reply {
        match serde_json::to_vec(&*self.state.borrow()) {
            Ok(body) => reply(StatusCode::OK, "application/json", body),
            Err(error) => text(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        }
    }

    async fn logs(&self, query: Option<&str>) -> Reply {
        let (client, limit) = match logs_params(query.unwrap_or_default()) {
            Ok(params) => params,
            Err(error) => return text(StatusCode::BAD_REQUEST, error),
        };
        let query = LogQuery {
            clients: vec![client],
            min_level: Level::Trace,
            between: UNIX_EPOCH..=SystemTime::now(),
            text: None,
            limit,
        };
        let Some(logs) = self.telemetry.query_logs(query).await else {
            return text(StatusCode::SERVICE_UNAVAILABLE, "telemetry stopped");
        };
        let logs: Vec<LogView> = logs.into_iter().map(LogView::from).collect();
        match serde_json::to_vec(&logs) {
            Ok(body) => reply(StatusCode::OK, "application/json", body),
            Err(error) => text(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        }
    }

    async fn admin(&self, body: Incoming) -> Reply {
        let body = match Limited::new(body, MAX_ADMIN_BODY).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) if error.is::<LengthLimitError>() => {
                return text(StatusCode::PAYLOAD_TOO_LARGE, error.to_string());
            }
            Err(error) => return text(StatusCode::BAD_REQUEST, error.to_string()),
        };
        let request: AdminRequest = match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(error) => return text(StatusCode::BAD_REQUEST, error.to_string()),
        };
        if self.hub.send(HubEvent::Admin(request)).is_err() {
            return text(StatusCode::SERVICE_UNAVAILABLE, "hub stopped");
        }
        text(StatusCode::ACCEPTED, "accepted")
    }

    async fn artifact(&self, path: &str) -> Reply {
        let Some(relative) = artifact_path(path) else {
            return text(StatusCode::NOT_FOUND, "not found");
        };
        match tokio::fs::read(self.artifact_dir.join(relative)).await {
            Ok(bytes) => reply(StatusCode::OK, "application/octet-stream", bytes),
            Err(_) => text(StatusCode::NOT_FOUND, "not found"),
        }
    }
}

/// `client=<32 hex>&limit=<n>`, `limit` optional. Values are not
/// percent-decoded, since neither can contain an encoded character.
fn logs_params(query: &str) -> Result<(ClientId, usize), String> {
    let mut client = None;
    let mut limit = DEFAULT_LOG_LIMIT;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some(("client", raw)) => {
                client = Some(ClientId::parse(raw).ok_or("client must be 32 lowercase hex")?);
            }
            Some(("limit", raw)) => {
                limit = raw.parse().map_err(|_| "limit must be a whole number")?;
            }
            _ => return Err(format!("unknown query parameter {pair:?}")),
        }
    }
    Ok((client.ok_or("client=<32 hex> is required")?, limit))
}

/// Plain names only, so no request reaches outside the artifact dir.
fn artifact_path(path: &str) -> Option<&Path> {
    let relative = Path::new(path.strip_prefix('/')?);
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(relative)
}

fn text(status: StatusCode, body: impl Into<Bytes>) -> Reply {
    reply(status, "text/plain; charset=utf-8", body)
}

fn reply(status: StatusCode, content_type: &'static str, body: impl Into<Bytes>) -> Reply {
    let mut reply = Response::new(Full::new(body.into()));
    *reply.status_mut() = status;
    reply
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_paths_stay_inside_the_artifact_dir() {
        assert_eq!(
            artifact_path("/x86_64/bounce-client-0.2.0"),
            Some(Path::new("x86_64/bounce-client-0.2.0"))
        );
        for escape in [
            "/../etc/passwd",
            "/a/../../etc/passwd",
            "//etc/passwd",
            "/./a",
        ] {
            assert_eq!(artifact_path(escape), None, "{escape}");
        }
    }

    #[test]
    fn logs_params_need_a_client_and_take_an_optional_limit() {
        let id = "b0c00000000000000000000000000001";
        let client = ClientId::parse(id).unwrap();
        assert_eq!(
            logs_params(&format!("client={id}")),
            Ok((client.clone(), DEFAULT_LOG_LIMIT))
        );
        assert_eq!(
            logs_params(&format!("limit=5&client={id}")),
            Ok((client, 5))
        );
        for bad in [
            String::new(),
            "limit=5".into(),
            "client=B0C00000000000000000000000000001".into(),
            format!("client={id}&limit=-1"),
            format!("client={id}&level=Info"),
            format!("client={id}&limit"),
        ] {
            assert!(logs_params(&bad).is_err(), "{bad:?} accepted");
        }
    }
}
