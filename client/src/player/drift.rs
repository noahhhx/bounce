//! How far a sound card's clock has walked from the content cursor, and the
//! correction that brings it back.

use std::collections::VecDeque;
use std::time::Duration;

/// Tracks the error `device position - content cursor` in frames.
/// Positive: the device will play the next content frame late.
///
/// Readings are noisy upward: a driver that moves its hardware pointer once
/// per period reports a stale, larger delay until the next interrupt. So the
/// estimate is the minimum over a window, as `ClockSync` does with round
/// trips. Corrections are subtracted from the readings taken before them, so
/// none is counted twice.
pub struct DeviceClock {
    rate: u32,
    /// Each raw reading minus `applied` at the time it was taken.
    readings: VecDeque<i64>,
    /// Net frames inserted (+) or dropped (-) since the last reset.
    applied: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    None,
    /// Repeat one frame of the next chunk: the device is ahead of content.
    Stretch,
    /// Drop one frame of the next chunk: the device is behind content.
    Squeeze,
    /// Write this many frames of silence: the device is far ahead.
    Pad(u64),
    /// Move the content cursor this many frames later: the device is far
    /// behind.
    Skip(u64),
}

/// About half a second of steps, enough for the minimum to find a fresh
/// reading on a driver that moves its pointer once per period.
const WINDOW: usize = 100;
/// Enough readings to tell settled from still moving while priming, as
/// after `PipeWire`'s measured 60 ms start delay.
const PRIME_WINDOW: usize = 10;
/// The window minimum still reads up to a fraction of a period high, and
/// that adds to whatever the deadband lets through.
const DEADBAND: Duration = Duration::from_micros(250);
/// Past this a frame per chunk would take seconds to catch up, so jump.
const STEP: Duration = Duration::from_millis(20);

impl DeviceClock {
    #[must_use]
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            readings: VecDeque::with_capacity(WINDOW),
            applied: 0,
        }
    }

    pub fn observe(&mut self, error: i64) {
        if self.readings.len() == WINDOW {
            self.readings.pop_front();
        }
        self.readings.push_back(error - self.applied);
    }

    /// `priming` while nothing has been heard since the device started, so
    /// a jump costs nothing. Then a few readings that agree within the
    /// deadband are trusted at once, a few that still move or scatter may
    /// only jump a large error away, and a full window jumps anything past
    /// the deadband. Once playing, only a full window acts.
    #[must_use]
    pub fn correction(&self, priming: bool) -> Correction {
        let deadband = self.frames(DEADBAND);
        let step = self.frames(STEP);
        let (window, jump) = match (self.readings.len(), priming) {
            (WINDOW, true) => (WINDOW, deadband),
            (WINDOW, false) => (WINDOW, step),
            (len, true) if len >= PRIME_WINDOW => {
                let newest = || self.readings.iter().rev().take(PRIME_WINDOW);
                let spread = newest().max().unwrap_or(&0) - newest().min().unwrap_or(&0);
                (
                    PRIME_WINDOW,
                    if spread <= deadband { deadband } else { step },
                )
            }
            _ => return Correction::None,
        };
        let newest = self.readings.iter().rev().take(window);
        let error = newest.min().expect("the window holds readings") + self.applied;
        match error {
            e if e < -jump => Correction::Pad(e.unsigned_abs()),
            e if e > jump => Correction::Skip(e.unsigned_abs()),
            e if e < -deadband => Correction::Stretch,
            e if e > deadband => Correction::Squeeze,
            _ => Correction::None,
        }
    }

    /// Frames inserted (+) or dropped (-) by a stretch or squeeze.
    pub fn applied(&mut self, frames: i64) {
        self.applied += frames;
    }

    /// After a pad or skip every reading is stale.
    pub fn reset(&mut self) {
        self.readings.clear();
        self.applied = 0;
    }

    fn frames(&self, duration: Duration) -> i64 {
        i64::try_from(duration.as_micros() * u128::from(self.rate) / 1_000_000).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn clock_after(readings: &[i64]) -> DeviceClock {
        let mut clock = DeviceClock::new(RATE);
        for &error in readings {
            clock.observe(error);
        }
        clock
    }

    #[test]
    fn a_partial_window_asks_for_nothing_once_playing() {
        assert_eq!(
            clock_after(&[-5_000; WINDOW - 1]).correction(false),
            Correction::None
        );
        assert_eq!(
            clock_after(&[-5_000; WINDOW]).correction(false),
            Correction::Pad(5_000)
        );
    }

    #[test]
    fn the_estimate_is_the_smallest_reading() {
        let mut readings = vec![2_000; WINDOW];
        readings[40] = 10;
        assert_eq!(clock_after(&readings).correction(false), Correction::None);
        readings[40] = 100;
        assert_eq!(
            clock_after(&readings).correction(false),
            Correction::Squeeze
        );
    }

    #[test]
    fn applied_frames_are_not_counted_twice() {
        let mut clock = clock_after(&[-40; WINDOW]);
        for _ in 0..28 {
            clock.applied(1);
        }
        assert_eq!(
            clock.correction(false),
            Correction::None,
            "28 inserted frames bring -40 inside the deadband"
        );
        for _ in 0..WINDOW {
            clock.observe(-30);
        }
        assert_eq!(
            clock.correction(false),
            Correction::Stretch,
            "a reading taken after the corrections already includes them"
        );
    }

    #[test]
    fn priming_jumps_from_the_newest_readings() {
        let mut readings = vec![0; 50];
        readings.extend([2_000; PRIME_WINDOW]);
        assert_eq!(
            clock_after(&readings).correction(true),
            Correction::Skip(2_000),
            "a start-up error is jumped once a few readings agree"
        );
        assert_eq!(
            clock_after(&readings).correction(false),
            Correction::None,
            "but not once playing"
        );
        assert_eq!(
            clock_after(&[30; PRIME_WINDOW]).correction(true),
            Correction::Skip(30),
            "a few readings that agree place the first chunk exactly"
        );
        let scattered: Vec<i64> = (0..PRIME_WINDOW)
            .map(|i| if i % 2 == 0 { 30 } else { 530 })
            .collect();
        assert_eq!(
            clock_after(&scattered).correction(true),
            Correction::Squeeze,
            "a few scattered readings may be stale, so only a slew"
        );
        assert_eq!(
            clock_after(&[30; WINDOW]).correction(true),
            Correction::Skip(30),
            "a full window places the first chunk exactly"
        );
        assert_eq!(
            clock_after(&[30; WINDOW]).correction(false),
            Correction::Squeeze,
            "while playing the same error is slewed"
        );
    }
}
