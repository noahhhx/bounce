use std::io::Read;
use std::process::ExitCode;

use bounce_client::clock;
use bounce_client::config::{Config, OutputBackend};
use bounce_client::link::{Backoff, Link};
use bounce_client::player::{Output, WavOutput};
use bounce_client::transport::Tcp;
use bounce_client::Device;
use bounce_common::audio::Codec;
use bounce_common::ids::{BootId, Version};
use bounce_common::protocol::{Hello, OutputDevice, PROTOCOL_VERSION};
use bounce_common::update::Target;
use tracing::info;

#[cfg(target_arch = "aarch64")]
const TARGET: Target = Target::Aarch64;
#[cfg(all(target_arch = "arm", target_feature = "v7"))]
const TARGET: Target = Target::ArmV7;
#[cfg(all(target_arch = "arm", not(target_feature = "v7")))]
const TARGET: Target = Target::ArmV6;
#[cfg(target_arch = "x86_64")]
const TARGET: Target = Target::X86_64;

fn main() -> ExitCode {
    let _ = clock::now();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let config = match Config::from_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("bounce-client: {error:?}");
            return ExitCode::from(2);
        }
    };
    let Some(server) = config.server else {
        eprintln!(
            "bounce-client: --server <host:port> is required; finding the server over mDNS is not supported yet"
        );
        return ExitCode::from(2);
    };

    let hello = hello(&config);
    info!(client = ?hello.client, boot = hello.boot.0, %server, "starting");
    let output = match config.output {
        OutputBackend::Alsa => Output::Alsa,
        OutputBackend::Wav { dir } => Output::Wav(WavOutput { dir }),
    };
    Link::new(Tcp, server, Backoff::default(), hello, Device::new(output)).run()
}

fn hello(config: &Config) -> Hello {
    let output = match &config.output {
        OutputBackend::Alsa => OutputDevice {
            id: "default".into(),
            description: "ALSA default device".into(),
        },
        OutputBackend::Wav { dir } => OutputDevice {
            id: "wav".into(),
            description: format!("WAV files in {}", dir.display()),
        },
    };
    Hello {
        protocol: PROTOCOL_VERSION,
        client: config.client_id.clone(),
        boot: boot_id(),
        version: Version {
            major: version_part(env!("CARGO_PKG_VERSION_MAJOR")),
            minor: version_part(env!("CARGO_PKG_VERSION_MINOR")),
            patch: version_part(env!("CARGO_PKG_VERSION_PATCH")),
        },
        target: TARGET,
        hostname: hostname(),
        outputs: vec![output],
        codecs: vec![Codec::PcmS16Le],
    }
}

fn version_part(part: &str) -> u16 {
    part.parse().expect("crate version parts fit in u16")
}

/// Random rather than derived from the clock, which a Pi without an RTC
/// reports identically on every boot until NTP syncs.
fn boot_id() -> BootId {
    let mut bytes = [0; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut urandom| urandom.read_exact(&mut bytes))
        .expect("/dev/urandom is readable");
    BootId(u64::from_le_bytes(bytes))
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
