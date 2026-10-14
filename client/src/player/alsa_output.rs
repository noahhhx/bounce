use std::io::ErrorKind;
use std::time::Duration;

use alsa::device_name::{Hint, HintIter};
use alsa::pcm::{Access, Format, HwParams, State, PCM};
use alsa::{Direction, ValueOr};
use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::SinceBoot;

use super::sink::{Clock, Frames, OpenError, Output, Sink, SinkError, WRITE_AHEAD};
use crate::clock;

/// Opens any ALSA playback PCM by name, such as `default` or
/// `hw:CARD=sndrpihifiberry`.
pub struct AlsaOutput;

pub struct AlsaSink {
    pcm: PCM,
    channels: usize,
    rate: u32,
}

const PERIOD_US: u32 = 10_000;
static SILENCE: [i16; 4_096] = [0; 4_096];

impl Output for AlsaOutput {
    type Sink = AlsaSink;

    fn devices(&self) -> Vec<OutputDevice> {
        match HintIter::new_str(None, "pcm") {
            Ok(hints) => advertised(hints),
            Err(error) => {
                tracing::warn!(%error, "cannot list ALSA devices");
                advertised(std::iter::empty())
            }
        }
    }

    fn open(&self, device: &str, _: StreamId, format: AudioFormat) -> Result<AlsaSink, OpenError> {
        open(device, format).map_err(|error| OpenError(format!("ALSA {device}: {error}")))
    }
}

fn open(device: &str, format: AudioFormat) -> Result<AlsaSink, String> {
    let pcm = PCM::new(device, Direction::Playback, false).map_err(|e| e.to_string())?;
    configure(&pcm, format).map_err(|e| e.to_string())?;
    let rate = pcm
        .hw_params_current()
        .and_then(|hw| hw.get_rate())
        .map_err(|e| e.to_string())?;
    if rate != format.sample_rate {
        return Err(format!("plays {rate} Hz, not {} Hz", format.sample_rate));
    }
    Ok(AlsaSink {
        pcm,
        channels: usize::from(format.channels),
        rate,
    })
}

fn configure(pcm: &PCM, format: AudioFormat) -> alsa::Result<()> {
    let hw = HwParams::any(pcm)?;
    hw.set_access(Access::RWInterleaved)?;
    hw.set_format(Format::s16())?;
    hw.set_channels(u32::from(format.channels))?;
    hw.set_rate_resample(true)?;
    hw.set_rate(format.sample_rate, ValueOr::Nearest)?;
    hw.set_period_time_near(PERIOD_US, ValueOr::Nearest)?;
    let buffer_us = u32::try_from(2 * WRITE_AHEAD.as_micros()).expect("a short buffer");
    hw.set_buffer_time_near(buffer_us, ValueOr::Nearest)?;
    pcm.hw_params(&hw)?;
    let period = pcm.hw_params_current()?.get_period_size()?;
    let sw = pcm.sw_params_current()?;
    sw.set_start_threshold(1)?;
    sw.set_avail_min(period)?;
    pcm.sw_params(&sw)
}

impl Sink for AlsaSink {
    fn clock(&mut self, now: SinceBoot) -> Result<Clock, SinkError> {
        let delay = self.pcm.delay().map_err(|e| self.fail(e))?;
        let since_step = clock::now().0.saturating_sub(now.0);
        let frames = u64::try_from(delay).unwrap_or(0);
        let delay = Duration::from_nanos(frames * 1_000_000_000 / u64::from(self.rate));
        Ok(Clock::Device {
            queued: delay + since_step,
        })
    }

    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError> {
        match frames {
            Frames::Audio { pcm, .. } => self.write_all(pcm),
            Frames::Silence { frames } => {
                let mut left = usize::try_from(frames).expect("fits in memory") * self.channels;
                while left > 0 {
                    let samples = left.min(SILENCE.len() / self.channels * self.channels);
                    self.write_all(&SILENCE[..samples])?;
                    left -= samples;
                }
                Ok(())
            }
        }
    }
}

impl AlsaSink {
    fn write_all(&self, mut samples: &[i16]) -> Result<(), SinkError> {
        let io = self.pcm.io_i16().map_err(|e| self.fail(e))?;
        while !samples.is_empty() {
            match io.writei(samples) {
                Ok(frames) => samples = &samples[frames * self.channels..],
                Err(e) if interrupted(e) => {}
                Err(e) => return Err(self.fail(e)),
            }
        }
        Ok(())
    }

    /// `try_recover` re-prepares after an underrun or a suspend. After an
    /// unplug it fails, or the PCM reports itself disconnected.
    fn fail(&self, error: alsa::Error) -> SinkError {
        if self.pcm.state() != State::Disconnected && self.pcm.try_recover(error, true).is_ok() {
            SinkError::Underrun
        } else {
            SinkError::Lost(error.to_string())
        }
    }
}

impl Drop for AlsaSink {
    fn drop(&mut self) {
        let _ = self.pcm.drop();
    }
}

fn interrupted(error: alsa::Error) -> bool {
    std::io::Error::from_raw_os_error(error.errno()).kind() == ErrorKind::Interrupted
}

/// `default` comes first because the server plays the first device a client
/// advertises.
fn advertised(hints: impl Iterator<Item = Hint>) -> Vec<OutputDevice> {
    const PLUMBING: [&str; 5] = ["surround", "dmix", "dsnoop", "iec958", "usbstream"];
    let mut devices = vec![OutputDevice {
        id: "default".into(),
        description: "Default ALSA output".into(),
    }];
    for hint in hints {
        let Some(id) = hint.name else { continue };
        let plumbing = PLUMBING.iter().any(|prefix| id.starts_with(prefix));
        if hint.direction == Some(Direction::Capture) || plumbing || id == "null" {
            continue;
        }
        let description = hint
            .desc
            .map_or_else(String::new, |d| d.lines().collect::<Vec<_>>().join(" "));
        match devices.iter_mut().find(|d| d.id == id) {
            Some(known) if id == "default" => known.description = description,
            Some(_) => {}
            None => devices.push(OutputDevice { id, description }),
        }
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTS: &[(&str, &str, bool)] = &[
        ("null", "Discard all samples (playback) or generate zero samples (capture)", true),
        ("pipewire", "PipeWire Sound Server", true),
        ("default", "Default ALSA Output (currently PipeWire Media Server)", true),
        ("hdmi:CARD=Generic,DEV=0", "HD-Audio Generic, HDMI 0\nHDMI Audio Output", true),
        ("hdmi:CARD=Generic,DEV=1", "HD-Audio Generic, HDMI 1\nHDMI Audio Output", true),
        ("hdmi:CARD=Generic,DEV=2", "HD-Audio Generic, HDMI 2\nHDMI Audio Output", true),
        ("hdmi:CARD=Generic,DEV=3", "HD-Audio Generic, HDMI 3\nHDMI Audio Output", true),
        ("sysdefault:CARD=Generic_1", "HD-Audio Generic, ALC285 Analog\nDefault Audio Device", true),
        ("front:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\nFront output / input", true),
        ("surround21:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n2.1 Surround output to Front and Subwoofer speakers", true),
        ("surround40:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n4.0 Surround output to Front and Rear speakers", true),
        ("surround41:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n4.1 Surround output to Front, Rear and Subwoofer speakers", true),
        ("surround50:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n5.0 Surround output to Front, Center and Rear speakers", true),
        ("surround51:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n5.1 Surround output to Front, Center, Rear and Subwoofer speakers", true),
        ("surround71:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC285 Analog\n7.1 Surround output to Front, Center, Side, Rear and Woofer speakers", true),
    ];

    fn from(hints: &[(&str, &str, bool)]) -> Vec<OutputDevice> {
        advertised(hints.iter().map(|&(id, description, playback)| Hint {
            name: Some(id.to_owned()),
            desc: Some(description.to_owned()),
            direction: (!playback).then_some(Direction::Capture),
        }))
    }

    #[test]
    fn offers_default_first_then_each_card_once() {
        let devices = from(HINTS);
        let ids: Vec<_> = devices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "default",
                "pipewire",
                "hdmi:CARD=Generic,DEV=0",
                "hdmi:CARD=Generic,DEV=1",
                "hdmi:CARD=Generic,DEV=2",
                "hdmi:CARD=Generic,DEV=3",
                "sysdefault:CARD=Generic_1",
                "front:CARD=Generic_1,DEV=0",
            ]
        );
        assert_eq!(
            devices[0].description,
            "Default ALSA Output (currently PipeWire Media Server)"
        );
        assert_eq!(
            devices[2].description,
            "HD-Audio Generic, HDMI 0 HDMI Audio Output"
        );
    }

    #[test]
    fn capture_only_and_duplicate_hints_are_dropped() {
        let devices = from(&[
            ("hw:CARD=Mic,DEV=0", "USB mic", false),
            ("hw:CARD=Dac,DEV=0", "USB DAC", true),
            ("hw:CARD=Dac,DEV=0", "USB DAC again", true),
        ]);
        let ids: Vec<_> = devices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["default", "hw:CARD=Dac,DEV=0"]);
        assert_eq!(devices[1].description, "USB DAC");
    }

    #[test]
    fn without_hints_only_default_is_offered() {
        let ids: Vec<_> = from(&[]).into_iter().map(|d| d.id).collect();
        assert_eq!(ids, ["default"]);
    }
}
