use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::watch;

use bounce_common::ids::SourceId;
use bounce_server::broadcast::Broadcaster;
use bounce_server::clock;
use bounce_server::config::{Config, READY_PREFIX};
use bounce_server::health::Thresholds;
use bounce_server::hub::{Effect, Hub, HubEvent};
use bounce_server::source::spotify::SpotifySource;
use bounce_server::source::test_tone::TestToneSource;
use bounce_server::source::{Pcm, Source, SourceConfig, SourceOutput};
use bounce_server::updates::ReleaseCatalog;

/// Snapcast's default.
const PLAYOUT_DELAY: Duration = Duration::from_secs(1);

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
    for (id, source_config) in (0..).zip(config.sources) {
        let id = SourceId(id);
        let (pcm_tx, pcm_rx) = mpsc::unbounded_channel();
        tokio::spawn(broadcast(id, pcm_rx));
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

    let mut hub = Hub::new(
        sources
            .iter()
            .map(|(&id, source)| (id, source.kind(), source.name().to_owned())),
        ReleaseCatalog::new(config.data_dir.join("releases")),
        Thresholds::default(),
    );
    let (state, _) = watch::channel(hub.view());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tick.tick() => HubEvent::Tick,
            Some(event) = events.recv() => event,
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
                Effect::Send { .. }
                | Effect::Close { .. }
                | Effect::Subscribe { .. }
                | Effect::Unsubscribe { .. } => debug!("no session to apply {effect:?}"),
                Effect::AlertRaised(alert) => info!("alert raised: {alert:?}"),
                Effect::AlertCleared(key) => info!("alert cleared: {key:?}"),
            }
        }
    }
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

/// Nothing subscribes yet: only sessions produce an outbox.
async fn broadcast(source: SourceId, mut pcm: UnboundedReceiver<Pcm>) {
    let mut broadcaster = Broadcaster::new(PLAYOUT_DELAY);
    while let Some(pcm) = pcm.recv().await {
        match pcm {
            Pcm::Start { stream, format } => {
                info!("source {}: stream {} {format:?}", source.0, stream.0);
                broadcaster.start(stream, format);
            }
            Pcm::Samples(samples) => broadcaster.push_pcm(&samples, clock::now()),
        }
    }
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
