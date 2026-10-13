use std::net::SocketAddr;
use std::time::Duration;

use log::{info, warn};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use bounce_common::ids::SourceId;
use bounce_server::config::{Config, READY_PREFIX};
use bounce_server::hub::HubEvent;
use bounce_server::source::spotify::SpotifySource;
use bounce_server::source::test_tone::TestToneSource;
use bounce_server::source::{Source, SourceConfig, SourceOutput};

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
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    for (id, source_config) in (0..).zip(config.sources) {
        let id = SourceId(id);
        let (pcm_tx, pcm_rx) = mpsc::unbounded_channel();
        tokio::spawn(meter(id, pcm_rx));
        let mut source: Box<dyn Source> = match source_config {
            SourceConfig::Spotify(c) => Box::new(SpotifySource::new(c)),
            SourceConfig::TestTone(c) => Box::new(TestToneSource::new(c)),
        };
        source.start(SourceOutput::new(id, events_tx.clone(), pcm_tx));
        sources.push(source);
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(HubEvent::Source { source, event }) = events.recv() => {
                info!("source {}: {event:?}", source.0);
            }
        }
    }
    let stopping: Vec<_> = sources.iter_mut().filter_map(|s| s.stop()).collect();
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

/// Stands in for the broadcaster. Silent seconds are skipped so an idle
/// server doesn't log every second.
async fn meter(source: SourceId, mut pcm: UnboundedReceiver<Vec<i16>>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let (mut samples, mut peak) = (0, 0);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if samples > 0 {
                    info!("source {}: pcm samples={samples} peak={peak}", source.0);
                }
                (samples, peak) = (0, 0);
            }
            Some(chunk) = pcm.recv() => {
                samples += chunk.len();
                peak = chunk.iter().map(|s| s.unsigned_abs()).fold(peak, u16::max);
            }
        }
    }
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
