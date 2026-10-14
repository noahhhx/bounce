use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use bounce_common::audio::AudioFormat;
use bounce_common::ids::StreamId;
use bounce_common::protocol::OutputDevice;
use bounce_common::time::{ServerInstant, SinceBoot};

use super::sink::{Clock, Frames, OpenError, Output, Sink, SinkError};

/// Writes in real time, paced by the same deadlines, so underrun padding
/// and volume land in the file exactly as they would at the DAC.
///
/// Each stream gets `<dir>/stream-<id>.wav`, 16-bit PCM behind the canonical
/// 44-byte header (sizes rewritten after every write, so the file is
/// readable while the client runs), and `<dir>/stream-<id>.timing`, one
/// `<first frame> <play_at us> <wall us>` line per chunk written, where
/// `wall us` is when that frame reaches the listener on the host wall clock.
/// Starting a stream whose files exist appends to them.
pub struct WavOutput {
    pub dir: PathBuf,
    pub wall_origin_us: u64,
}

pub struct WavStream {
    wav: File,
    timing: File,
    frames: u64,
    block_align: u16,
    wall_origin_us: u64,
}

const HEADER_LEN: u64 = 44;

impl Output for WavOutput {
    type Sink = WavStream;

    fn devices(&self) -> Vec<OutputDevice> {
        vec![OutputDevice {
            id: "wav".into(),
            description: format!("WAV files in {}", self.dir.display()),
        }]
    }

    fn open(
        &self,
        _device: &str,
        stream: StreamId,
        format: AudioFormat,
    ) -> Result<WavStream, OpenError> {
        self.create(stream, format)
            .map_err(|error| OpenError(format!("WAV in {}: {error}", self.dir.display())))
    }
}

impl WavOutput {
    fn create(&self, stream: StreamId, format: AudioFormat) -> io::Result<WavStream> {
        std::fs::create_dir_all(&self.dir)?;
        let base = self.dir.join(format!("stream-{}", stream.0));
        let wav = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.with_extension("wav"))?;
        let timing = OpenOptions::new()
            .append(true)
            .create(true)
            .open(base.with_extension("timing"))?;
        let block_align = 2 * u16::from(format.channels);
        let data_len = wav.metadata()?.len().saturating_sub(HEADER_LEN);
        let stream = WavStream {
            wav,
            timing,
            frames: data_len / u64::from(block_align),
            block_align,
            wall_origin_us: self.wall_origin_us,
        };
        stream.wav.set_len(HEADER_LEN + stream.data_len())?;
        stream.write_header(format)?;
        Ok(stream)
    }
}

impl Sink for WavStream {
    fn clock(&mut self, _now: SinceBoot) -> Result<Clock, SinkError> {
        Ok(Clock::Written)
    }

    fn write(&mut self, frames: Frames<'_>) -> Result<(), SinkError> {
        match frames {
            Frames::Audio {
                pcm,
                play_at,
                heard,
            } => self.chunk(pcm, play_at, heard),
            Frames::Silence { frames } => self.silence(frames),
        }
        .map_err(|error| SinkError::Lost(format!("WAV: {error}")))
    }
}

impl WavStream {
    fn chunk(&mut self, pcm: &[i16], play_at: ServerInstant, heard: SinceBoot) -> io::Result<()> {
        let first = self.frames;
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.append(&bytes)?;
        let wall = self.wall_origin_us + u64::try_from(heard.0.as_micros()).unwrap_or(u64::MAX);
        writeln!(self.timing, "{first} {} {wall}", play_at.0)
    }

    fn silence(&mut self, frames: u64) -> io::Result<()> {
        let len = usize::try_from(frames * u64::from(self.block_align)).expect("fits in memory");
        self.append(&vec![0; len])
    }

    fn append(&mut self, pcm: &[u8]) -> io::Result<()> {
        self.wav.write_all_at(pcm, HEADER_LEN + self.data_len())?;
        self.frames += pcm.len() as u64 / u64::from(self.block_align);
        let data_len = size_field(self.data_len());
        self.wav.write_all_at(&(data_len + 36).to_le_bytes(), 4)?;
        self.wav.write_all_at(&data_len.to_le_bytes(), 40)
    }

    fn data_len(&self) -> u64 {
        self.frames * u64::from(self.block_align)
    }

    fn write_header(&self, format: AudioFormat) -> io::Result<()> {
        let data_len = size_field(self.data_len());
        let byte_rate = format.sample_rate * u32::from(self.block_align);
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(data_len + 36).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&u16::from(format.channels).to_le_bytes());
        header.extend_from_slice(&format.sample_rate.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&self.block_align.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&data_len.to_le_bytes());
        self.wav.write_all_at(&header, 0)
    }
}

/// A WAV past 4 GiB of data, about six hours of 48 kHz stereo, keeps
/// playing with its size fields pinned at the maximum.
fn size_field(data_len: u64) -> u32 {
    u32::try_from(data_len)
        .unwrap_or(u32::MAX - 36)
        .min(u32::MAX - 36)
}
