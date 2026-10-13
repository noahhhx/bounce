//! Spotify Connect via librespot.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use librespot::connect::{ConnectConfig, Spirc};
use librespot::core::cache::Cache;
use librespot::core::{Session, SessionConfig};
use librespot::discovery::{DeviceType, Discovery};
use librespot::metadata::audio::{AudioItem, UniqueFields};
use librespot::playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot::playback::config::{self, PlayerConfig};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;
use librespot::playback::mixer::softmixer::SoftMixer;
use librespot::playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot::playback::player::{Player, PlayerEvent};
use librespot::playback::{NUM_CHANNELS, SAMPLE_RATE};
use log::{error, warn};
use sha1::{Digest, Sha1};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use bounce_common::zone::Volume;

use super::{Source, SourceControl, SourceEvent, SourceKind, SourceOutput, TrackMetadata};

pub struct SpotifyConfig {
    /// The name shown in the Spotify app's device picker. One device for the
    /// whole house, e.g. "Bounce".
    pub device_name: String,
    pub bitrate: Bitrate,
    /// Credentials, persisted so a restart doesn't need the phone to
    /// re-pair.
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum Bitrate {
    Kbps96,
    Kbps160,
    Kbps320,
}

impl From<Bitrate> for config::Bitrate {
    fn from(bitrate: Bitrate) -> Self {
        match bitrate {
            Bitrate::Kbps96 => Self::Bitrate96,
            Bitrate::Kbps160 => Self::Bitrate160,
            Bitrate::Kbps320 => Self::Bitrate320,
        }
    }
}

pub struct SpotifySource {
    config: SpotifyConfig,
    running: Option<Running>,
}

struct Running {
    controls: UnboundedSender<SourceControl>,
    task: JoinHandle<()>,
}

impl SpotifySource {
    #[must_use]
    pub fn new(config: SpotifyConfig) -> Self {
        Self {
            config,
            running: None,
        }
    }
}

impl Source for SpotifySource {
    fn kind(&self) -> SourceKind {
        SourceKind::Spotify
    }

    fn name(&self) -> &str {
        &self.config.device_name
    }

    fn start(&mut self, out: SourceOutput) {
        let (controls, controls_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(
            self.config.device_name.clone(),
            self.config.bitrate,
            self.config.cache_dir.clone(),
            out,
            controls_rx,
        ));
        self.running = Some(Running { controls, task });
    }

    fn control(&mut self, control: SourceControl) {
        if let Some(running) = &self.running {
            let _ = running.controls.send(control);
        }
    }

    /// Closing the control channel tells the task to shut down.
    fn stop(&mut self) -> Option<JoinHandle<()>> {
        self.running.take().map(|running| running.task)
    }
}

type SpircTask = Pin<Box<dyn Future<Output = ()> + Send>>;

async fn run(
    name: String,
    bitrate: Bitrate,
    cache_dir: PathBuf,
    out: SourceOutput,
    mut controls: UnboundedReceiver<SourceControl>,
) {
    let cache = match Cache::new(Some(&cache_dir), None, None, None) {
        Ok(cache) => cache,
        Err(e) => return error!("spotify cache at {}: {e}", cache_dir.display()),
    };
    let session_config = SessionConfig {
        device_id: device_id(&name),
        ..SessionConfig::default()
    };
    let mut discovery = match Discovery::builder(
        session_config.device_id.clone(),
        session_config.client_id.clone(),
    )
    .name(name.clone())
    .device_type(DeviceType::Speaker)
    .launch()
    {
        Ok(discovery) => discovery,
        Err(e) => return error!("spotify discovery: {e}"),
    };
    let connect_config = ConnectConfig {
        name,
        device_type: DeviceType::Speaker,
        ..ConnectConfig::default()
    };

    // Zones scale volume downstream, so the player gets a volume getter that
    // never attenuates. The mixer only tracks the app's slider for Spirc.
    let mixer: Arc<dyn Mixer> = match SoftMixer::open(MixerConfig::default()) {
        Ok(mixer) => Arc::new(mixer),
        Err(e) => return error!("spotify mixer: {e}"),
    };
    let player_config = PlayerConfig {
        bitrate: bitrate.into(),
        ..PlayerConfig::default()
    };
    let sink_out = out.clone();
    let player = Player::new(
        player_config,
        Session::new(session_config.clone(), Some(cache.clone())),
        Box::new(NoOpVolume),
        move || Box::new(PacedSink::new(sink_out)),
    );
    let mut player_events = player.get_player_event_channel();

    let mut spirc: Option<(Spirc, SpircTask)> = None;
    let mut pending = cache.credentials();
    loop {
        if let Some(credentials) = pending.take() {
            if let Some((old, old_task)) = spirc.take() {
                let _ = old.shutdown();
                tokio::spawn(old_task);
            }
            let session = Session::new(session_config.clone(), Some(cache.clone()));
            player.set_session(session.clone());
            match Spirc::new(
                connect_config.clone(),
                session,
                credentials,
                player.clone(),
                mixer.clone(),
            )
            .await
            {
                Ok((handle, task)) => spirc = Some((handle, Box::pin(task))),
                Err(e) => warn!("spotify connect failed, waiting for the app to pair: {e}"),
            }
        }

        tokio::select! {
            credentials = discovery.next() => match credentials {
                Some(credentials) => pending = Some(credentials),
                None => return error!("spotify discovery stopped"),
            },
            control = controls.recv() => match (control, &spirc) {
                (Some(SourceControl::SetVolume(volume)), Some((handle, _))) => {
                    let _ = handle.set_volume(to_spotify_volume(volume));
                }
                (Some(_), None) => {}
                (None, _) => break,
            },
            Some(event) = player_events.recv() => {
                if let Some(event) = map_event(event) {
                    out.event(event);
                }
            }
            () = async { if let Some((_, task)) = &mut spirc { task.await } }, if spirc.is_some() => {
                warn!("spotify connect session ended; the app can pair again");
                spirc = None;
            }
        }
    }

    if let Some((handle, task)) = spirc {
        let _ = handle.shutdown();
        task.await;
    }
    discovery.shutdown().await;
}

/// Stable per name, as librespot's own binary derives it, so the app keeps
/// recognising the device across restarts.
fn device_id(name: &str) -> String {
    format!("{:x}", Sha1::digest(name.as_bytes()))
}

fn map_event(event: PlayerEvent) -> Option<SourceEvent> {
    Some(match event {
        PlayerEvent::TrackChanged { audio_item } => SourceEvent::Track(track(*audio_item)),
        PlayerEvent::VolumeChanged { volume } => {
            SourceEvent::VolumeChanged(from_spotify_volume(volume))
        }
        _ => return None,
    })
}

fn track(item: AudioItem) -> TrackMetadata {
    let (artists, album) = match item.unique_fields {
        UniqueFields::Track { artists, album, .. } => {
            (artists.0.into_iter().map(|a| a.name).collect(), album)
        }
        UniqueFields::Episode { show_name, .. } => (vec![show_name.clone()], show_name),
        UniqueFields::Local { artists, album, .. } => {
            (artists.into_iter().collect(), album.unwrap_or_default())
        }
    };
    TrackMetadata {
        title: item.name,
        artists,
        album,
        cover_url: item
            .covers
            .into_iter()
            .max_by_key(|c| c.width)
            .map(|c| c.url),
        duration: Duration::from_millis(item.duration_ms.into()),
    }
}

fn from_spotify_volume(volume: u16) -> Volume {
    let percent = (u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX);
    Volume::new(u8::try_from(percent).unwrap_or(u8::MAX))
}

fn to_spotify_volume(volume: Volume) -> u16 {
    let raw = u32::from(volume.percent()) * u32::from(u16::MAX) / 100;
    u16::try_from(raw).unwrap_or(u16::MAX)
}

/// How far ahead of the wall clock the sink may run. librespot writes as
/// fast as the sink accepts, and ALSA would block at the device rate; this
/// sink has no device, so it sleeps to the same rate itself.
const MAX_LEAD: Duration = Duration::from_millis(150);

/// librespot sends player events over a channel from its player thread, so
/// the event loop can hear one after the sink has written its samples.
struct PacedSink {
    out: SourceOutput,
    since: Instant,
    written: Duration,
}

impl PacedSink {
    fn new(out: SourceOutput) -> Self {
        Self {
            out,
            since: Instant::now(),
            written: Duration::ZERO,
        }
    }
}

impl Sink for PacedSink {
    /// librespot stops the sink on pause and starts it on resume, so the
    /// clock restarts here and a resume does not burst to catch up.
    fn start(&mut self) -> SinkResult<()> {
        self.since = Instant::now();
        self.written = Duration::ZERO;
        self.out.start_stream(SAMPLE_RATE, NUM_CHANNELS);
        Ok(())
    }

    /// Called for a pause as well as a stop, with no way to tell them apart,
    /// so a pause ends the stream and the resume starts a new one.
    fn stop(&mut self) -> SinkResult<()> {
        self.out.stop_stream();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Err(SinkError::InvalidParams("passthrough is off".into()));
        };
        self.out.pcm(&converter.f64_to_s16(&samples));
        let frames = u32::try_from(samples.len() / usize::from(NUM_CHANNELS)).unwrap_or(u32::MAX);
        self.written += Duration::from_secs(1) * frames / SAMPLE_RATE;
        if let Some(ahead) = self.written.checked_sub(self.since.elapsed() + MAX_LEAD) {
            std::thread::sleep(ahead);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use bounce_common::ids::SourceId;

    use super::*;
    use crate::hub::HubEvent;
    use crate::source::Pcm;

    #[test]
    fn sink_plays_at_real_time_and_resumes_without_bursting() {
        let (events, _events) = mpsc::unbounded_channel();
        let (pcm, mut received) = mpsc::unbounded_channel();
        let streams = Arc::new(AtomicU64::new(0));
        let mut sink = PacedSink::new(SourceOutput::new(SourceId(0), events, pcm, streams));
        let mut converter = Converter::new(None);
        let mut play_one_second = || {
            let begun = Instant::now();
            sink.start().unwrap();
            for _ in 0..10 {
                let tenth = vec![0.25; 2 * 4410];
                sink.write(AudioPacket::Samples(tenth), &mut converter)
                    .unwrap();
            }
            begun.elapsed()
        };

        let first = play_one_second();
        std::thread::sleep(Duration::from_millis(500));
        let after_pause = play_one_second();

        let real_time =
            Duration::from_secs(1).saturating_sub(MAX_LEAD)..Duration::from_millis(1400);
        assert!(real_time.contains(&first), "first second took {first:?}");
        assert!(
            real_time.contains(&after_pause),
            "second after a pause took {after_pause:?}"
        );
        let samples: usize = std::iter::from_fn(|| received.try_recv().ok())
            .map(|pcm| match pcm {
                Pcm::Start { .. } => 0,
                Pcm::Samples(chunk) => chunk.len(),
            })
            .sum();
        assert_eq!(samples, 2 * 2 * 44_100);
    }

    #[test]
    fn each_sink_run_is_a_stream_that_starts_before_its_samples() {
        let (events, mut events_rx) = mpsc::unbounded_channel();
        let (pcm, mut pcm_rx) = mpsc::unbounded_channel();
        let streams = Arc::new(AtomicU64::new(7));
        let mut sink = PacedSink::new(SourceOutput::new(SourceId(0), events, pcm, streams));
        let mut converter = Converter::new(None);

        sink.start().unwrap();
        sink.write(AudioPacket::Samples(vec![0.25; 2]), &mut converter)
            .unwrap();
        sink.stop().unwrap();
        sink.start().unwrap();
        sink.write(AudioPacket::Samples(vec![0.25; 2]), &mut converter)
            .unwrap();

        let pcm: Vec<String> = std::iter::from_fn(|| pcm_rx.try_recv().ok())
            .map(|pcm| match pcm {
                Pcm::Start { stream, .. } => format!("start {}", stream.0),
                Pcm::Samples(_) => "samples".to_owned(),
            })
            .collect();
        assert_eq!(pcm, ["start 7", "samples", "start 8", "samples"]);
        let events: Vec<String> = std::iter::from_fn(|| events_rx.try_recv().ok())
            .map(|event| match event {
                HubEvent::Source {
                    event: SourceEvent::Started { stream, .. },
                    ..
                } => format!("started {}", stream.0),
                HubEvent::Source {
                    event: SourceEvent::Stopped,
                    ..
                } => "stopped".to_owned(),
                _ => "other".to_owned(),
            })
            .collect();
        assert_eq!(events, ["started 7", "stopped", "started 8"]);
    }

    #[test]
    fn volume_round_trips_through_spotify_scale() {
        for percent in 0..=100 {
            let volume = Volume::new(percent);
            assert_eq!(from_spotify_volume(to_spotify_volume(volume)), volume);
        }
        assert_eq!(to_spotify_volume(Volume::new(100)), u16::MAX);
    }
}
