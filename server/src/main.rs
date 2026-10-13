use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{error, info, warn};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;

use bounce_common::ids::{ClientId, SourceId};
use bounce_common::protocol::ServerMessage;
use bounce_server::broadcast::Broadcaster;
use bounce_server::clock;
use bounce_server::config::{Config, READY_PREFIX};
use bounce_server::health::Thresholds;
use bounce_server::http;
use bounce_server::hub::{Effect, Hub, HubEvent};
use bounce_server::session::{self, AudioLane, Outbox, SessionEvent, SessionId};
use bounce_server::snapshot::{self, Snapshot};
use bounce_server::source::spotify::SpotifySource;
use bounce_server::source::test_tone::TestToneSource;
use bounce_server::source::{Pcm, Source, SourceConfig, SourceOutput};
use bounce_server::transport;
use bounce_server::updates::ReleaseCatalog;

/// Snapcast's default.
const PLAYOUT_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Routing {
    clients: HashMap<ClientId, Route>,
    broadcasters: HashMap<SourceId, UnboundedSender<Subscription>>,
}

struct Route {
    session: SessionId,
    outbox: Outbox,
}

enum Subscription {
    Subscribe(ClientId, AudioLane),
    Unsubscribe(ClientId),
}

impl Routing {
    fn session_event(&mut self, event: SessionEvent) -> HubEvent {
        match event {
            SessionEvent::Opened {
                session,
                hello,
                outbox,
            } => {
                let route = Route { session, outbox };
                self.clients.insert(hello.client.clone(), route);
                HubEvent::Connected { hello, session }
            }
            SessionEvent::Message { client, message } => HubEvent::Message { client, message },
            SessionEvent::Closed { client, session } => {
                if self
                    .clients
                    .get(&client)
                    .is_some_and(|r| r.session == session)
                {
                    self.clients.remove(&client);
                }
                HubEvent::Disconnected { client, session }
            }
        }
    }

    fn send(&self, to: &ClientId, message: ServerMessage) {
        if let Some(route) = self.clients.get(to) {
            route.outbox.control(message);
        }
    }

    fn close(&mut self, client: &ClientId) {
        self.clients.remove(client);
    }

    fn subscribe(&self, source: SourceId, client: ClientId) {
        let route = self.clients.get(&client);
        if let (Some(route), Some(broadcaster)) = (route, self.broadcasters.get(&source)) {
            let lane = route.outbox.audio_lane();
            let _ = broadcaster.send(Subscription::Subscribe(client, lane));
        }
    }

    fn unsubscribe(&self, source: SourceId, client: ClientId) {
        if let Some(broadcaster) = self.broadcasters.get(&source) {
            let _ = broadcaster.send(Subscription::Unsubscribe(client));
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = match Config::from_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("bounce-server: {error:?}");
            std::process::exit(2);
        }
    };

    let fleet_path = config.data_dir.join("fleet.json");
    let saved = load_fleet(&config.data_dir, &fleet_path);
    let session_listener = bind("--listen", config.listen).await;
    let http_listener = bind("--http", config.http).await;
    println!(
        "{READY_PREFIX} session={} http={}",
        local_addr(&session_listener),
        local_addr(&http_listener)
    );

    let (events_tx, mut events) = mpsc::unbounded_channel();
    let streams = Arc::new(AtomicU64::new(unix_micros()));
    let mut sources: HashMap<SourceId, Box<dyn Source>> = HashMap::new();
    let mut routing = Routing::default();
    for (id, source_config) in (0..).zip(config.sources) {
        let id = SourceId(id);
        let (pcm_tx, pcm_rx) = mpsc::unbounded_channel();
        let (subscriptions_tx, subscriptions_rx) = mpsc::unbounded_channel();
        tokio::spawn(broadcast(id, pcm_rx, subscriptions_rx));
        routing.broadcasters.insert(id, subscriptions_tx);
        let mut source: Box<dyn Source> = match source_config {
            SourceConfig::Spotify(c) => Box::new(SpotifySource::new(c)),
            SourceConfig::TestTone(c) => Box::new(TestToneSource::new(c)),
        };
        source.start(SourceOutput::new(
            id,
            events_tx.clone(),
            pcm_tx,
            streams.clone(),
        ));
        sources.insert(id, source);
    }

    let releases = config.data_dir.join("releases");
    let mut hub = Hub::new(
        sources
            .iter()
            .map(|(&id, source)| (id, source.kind(), source.name().to_owned())),
        ReleaseCatalog::new(releases.clone()),
        Thresholds::default(),
        saved,
        Instant::now(),
    );
    let (state, state_rx) = watch::channel(hub.view());
    let api = http::serve(http_listener, events_tx.clone(), state_rx, releases);
    tokio::spawn(async move {
        let Err(error) = api.await;
        eprintln!("bounce-server: http: {error}");
        std::process::exit(1);
    });
    let (sessions_tx, mut sessions) = mpsc::unbounded_channel();
    tokio::spawn(session::accept(
        transport::TcpListener::from(session_listener),
        sessions_tx,
    ));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tick.tick() => HubEvent::Tick,
            Some(event) = events.recv() => event,
            Some(event) = sessions.recv() => routing.session_event(event),
        };
        for effect in hub.handle(event, Instant::now()) {
            match effect {
                Effect::StateChanged(view) => {
                    state.send_replace(view);
                }
                Effect::ControlSource { source, control } => {
                    if let Some(source) = sources.get_mut(&source) {
                        source.control(control);
                    }
                }
                Effect::Send { to, message } => routing.send(&to, message),
                Effect::Close { client } => routing.close(&client),
                Effect::Subscribe { source, client } => routing.subscribe(source, client),
                Effect::Unsubscribe { source, client } => routing.unsubscribe(source, client),
                Effect::AlertRaised(alert) => info!("alert raised: {alert:?}"),
                Effect::AlertCleared(key) => info!("alert cleared: {key:?}"),
                Effect::Persist(snapshot) => {
                    if let Err(error) = snapshot::save(&fleet_path, &snapshot) {
                        error!("saving {}: {error}", fleet_path.display());
                    }
                }
            }
        }
    }
    stop(sources).await;
}

async fn stop(mut sources: HashMap<SourceId, Box<dyn Source>>) {
    let stopping: Vec<_> = sources.values_mut().filter_map(|s| s.stop()).collect();
    let all_stopped = async {
        for task in stopping {
            let _ = task.await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), all_stopped)
        .await
        .is_err()
    {
        warn!("sources still stopping after 5s; exiting anyway");
    }
}

async fn broadcast(
    source: SourceId,
    mut pcm: UnboundedReceiver<Pcm>,
    mut subscriptions: UnboundedReceiver<Subscription>,
) {
    let mut broadcaster = Broadcaster::new(PLAYOUT_DELAY);
    loop {
        tokio::select! {
            Some(change) = subscriptions.recv() => match change {
                Subscription::Subscribe(client, lane) => broadcaster.subscribe(client, lane),
                Subscription::Unsubscribe(client) => broadcaster.unsubscribe(&client),
            },
            pcm = pcm.recv() => match pcm {
                Some(Pcm::Start { stream, format }) => {
                    info!("source {}: stream {} {format:?}", source.0, stream.0);
                    broadcaster.start(stream, format);
                }
                Some(Pcm::Samples(samples)) => broadcaster.push_pcm(&samples, clock::now()),
                None => break,
            },
        }
    }
}

/// Exits rather than starting empty, because the first save would then
/// overwrite a fleet that is still on disk.
fn load_fleet(data_dir: &Path, path: &Path) -> Snapshot {
    if let Err(error) = std::fs::create_dir_all(data_dir) {
        eprintln!("bounce-server: {}: {error}", data_dir.display());
        std::process::exit(1);
    }
    snapshot::load(path).unwrap_or_else(|error| {
        eprintln!("bounce-server: {}: {error}", path.display());
        std::process::exit(1);
    })
}

fn unix_micros() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_micros()).unwrap_or(u64::MAX)
}

async fn bind(flag: &str, addr: SocketAddr) -> TcpListener {
    TcpListener::bind(addr).await.unwrap_or_else(|error| {
        eprintln!("bounce-server: {flag} {addr}: {error}");
        std::process::exit(1);
    })
}

fn local_addr(listener: &TcpListener) -> SocketAddr {
    listener
        .local_addr()
        .expect("a bound listener has an address")
}
