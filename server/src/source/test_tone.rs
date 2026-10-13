//! Starts playing as soon as it starts, so routing a zone to it is all it
//! takes to hear something.
//!
//! The signal is a quiet sine with a full-scale 5 ms click at every whole
//! second of stream time.

use tokio::task::JoinHandle;

use super::{Source, SourceControl, SourceKind, SourceOutput};

pub struct TestToneConfig {
    pub name: String,
    pub frequency_hz: u32,
    pub sample_rate: u32,
}

pub struct TestToneSource {
    config: TestToneConfig,
}

impl TestToneSource {
    #[must_use]
    pub fn new(config: TestToneConfig) -> Self {
        Self { config }
    }
}

impl Source for TestToneSource {
    fn kind(&self) -> SourceKind {
        SourceKind::TestTone
    }

    fn start(&mut self, out: SourceOutput) {
        todo!("emit Started, then PCM paced to real time until stop")
    }

    fn control(&mut self, control: SourceControl) {
        todo!("scale the generated signal")
    }

    fn stop(&mut self) -> Option<JoinHandle<()>> {
        todo!()
    }
}
