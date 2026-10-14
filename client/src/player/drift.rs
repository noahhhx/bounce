use std::collections::VecDeque;
use std::time::Duration;

/// Readings are noisy upward: a driver that moves its hardware pointer once
/// per period reports a stale, larger delay until the next interrupt. So the
/// estimate is the minimum over a window.
pub struct DeviceClock {
    rate: u32,
    readings: VecDeque<i64>,
    inserted: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    None,
    RepeatFrame,
    DropFrame,
    PadSilence(u64),
    SkipAhead(u64),
}

/// Long enough for the minimum to catch a fresh reading on a driver that
/// moves its pointer once per period.
const WINDOW: usize = 100;
/// `PipeWire` was measured holding its delay still for 60 ms after a start.
const PRIME_WINDOW: usize = 10;
/// With delay readings in 1024-frame steps the error reached 0.67 ms at this
/// deadband and 1.15 ms at 0.5 ms.
const DEADBAND: Duration = Duration::from_micros(250);
const JUMP_PAST: Duration = Duration::from_millis(20);

impl DeviceClock {
    #[must_use]
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            readings: VecDeque::with_capacity(WINDOW),
            inserted: 0,
        }
    }

    pub fn observe(&mut self, frames_late: i64) {
        if self.readings.len() == WINDOW {
            self.readings.pop_front();
        }
        self.readings.push_back(frames_late - self.inserted);
    }

    #[must_use]
    pub fn correction(&self, priming: bool) -> Correction {
        let deadband = self.frames(DEADBAND);
        let step = self.frames(JUMP_PAST);
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
        let error = newest.min().expect("the window holds readings") + self.inserted;
        match error {
            e if e < -jump => Correction::PadSilence(e.unsigned_abs()),
            e if e > jump => Correction::SkipAhead(e.unsigned_abs()),
            e if e < -deadband => Correction::RepeatFrame,
            e if e > deadband => Correction::DropFrame,
            _ => Correction::None,
        }
    }

    pub fn inserted(&mut self, frames: i64) {
        self.inserted += frames;
    }

    pub fn reset(&mut self) {
        self.readings.clear();
        self.inserted = 0;
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
            Correction::PadSilence(5_000)
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
            Correction::DropFrame
        );
    }

    #[test]
    fn applied_frames_are_not_counted_twice() {
        let mut clock = clock_after(&[-40; WINDOW]);
        for _ in 0..28 {
            clock.inserted(1);
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
            Correction::RepeatFrame,
            "a reading taken after the corrections already includes them"
        );
    }

    #[test]
    fn priming_jumps_from_the_newest_readings() {
        let mut readings = vec![0; 50];
        readings.extend([2_000; PRIME_WINDOW]);
        assert_eq!(
            clock_after(&readings).correction(true),
            Correction::SkipAhead(2_000),
            "a start-up error is jumped once a few readings agree"
        );
        assert_eq!(
            clock_after(&readings).correction(false),
            Correction::None,
            "but not once playing"
        );
        assert_eq!(
            clock_after(&[30; PRIME_WINDOW]).correction(true),
            Correction::SkipAhead(30),
            "a few readings that agree place the first chunk exactly"
        );
        let scattered: Vec<i64> = (0..PRIME_WINDOW)
            .map(|i| if i % 2 == 0 { 30 } else { 530 })
            .collect();
        assert_eq!(
            clock_after(&scattered).correction(true),
            Correction::DropFrame,
            "a few scattered readings may be stale, so only a slew"
        );
        assert_eq!(
            clock_after(&[30; WINDOW]).correction(true),
            Correction::SkipAhead(30),
            "a full window places the first chunk exactly"
        );
        assert_eq!(
            clock_after(&[30; WINDOW]).correction(false),
            Correction::DropFrame,
            "while playing the same error is slewed"
        );
    }
}
