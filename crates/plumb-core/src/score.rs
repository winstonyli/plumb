//! The Score column's definitions: what a file's "stale size" is, and how a process's steady load is made.
//!
//! Pure: no Windows types. `Tree` rolls the results up; `Procs` feeds the load.
use std::num::NonZeroU32;

/// A file younger than this (seconds) weighs nothing.
pub const STALE_FROM: u32 = 30 * 86_400;
/// A file at least this old (seconds) counts in full.
pub const STALE_FULL: u32 = 730 * 86_400;
/// The I/O rate (bytes a second) that counts as a full machine: a SATA SSD's sequential speed.
pub const IO_FULL: u64 = 500_000_000;
/// The time constant (seconds) of the CPU and I/O averages.
pub const TAU_SECS: f64 = 30.0;

/// How much of a file's size counts at `age_secs`, per mille.
pub fn weight(age_secs: u32) -> u32 {
    if age_secs <= STALE_FROM {
        0
    } else if age_secs >= STALE_FULL {
        1000
    } else {
        (u64::from(age_secs - STALE_FROM) * 1000 / u64::from(STALE_FULL - STALE_FROM)) as u32
    }
}

/// The bytes of a file that count as stale; unknown and future modification times count nothing.
pub fn file_score(size: u64, mtime: Option<NonZeroU32>, now: u32) -> u64 {
    let Some(age) = mtime.and_then(|m| now.checked_sub(m.get())) else { return 0 };
    (u128::from(size) * u128::from(weight(age)) / 1000) as u64
}

/// A process's CPU use as a percent of the whole machine.
pub fn cpu_share(cpu_tenths: u64, cores: u32) -> f64 {
    if cores == 0 { 0.0 } else { cpu_tenths as f64 / 10.0 / f64::from(cores) }
}

/// A process's private bytes as a percent of physical memory.
pub fn mem_share(private: u64, ram: u64) -> f64 {
    if ram == 0 { 0.0 } else { private as f64 * 100.0 / ram as f64 }
}

/// A process's I/O rate as a percent of [`IO_FULL`].
pub fn io_share(io_rate: u64) -> f64 {
    io_rate as f64 * 100.0 / IO_FULL as f64
}

/// The whole percent to show: it moves only when `x` is a full point away, and an idle `x` shows 0.
pub fn settle(shown: u64, x: f64) -> u64 {
    if x < 0.5 {
        0
    } else if (x - shown as f64).abs() >= 1.0 {
        x.round() as u64
    } else {
        shown
    }
}

/// One process's smoothed load: exponential averages of its CPU and I/O shares, and the whole percent shown.
#[derive(Default, Clone, Copy, Debug)]
pub struct Load {
    cpu: f64,
    io: f64,
    shown: u64,
}

impl Load {
    /// Folds in `dt_secs` of time at the given shares (percent) and returns the shown value: the largest of the
    /// averaged CPU, the averaged I/O and the memory share, through the dead band.
    pub fn step(&mut self, dt_secs: f64, cpu_pct: f64, io_pct: f64, mem_pct: f64) -> u64 {
        let a = 1.0 - (-dt_secs / TAU_SECS).exp();
        self.cpu += (cpu_pct - self.cpu) * a;
        self.io += (io_pct - self.io) * a;
        self.shown = settle(self.shown, self.cpu.max(self.io).max(mem_pct));
        self.shown
    }

    pub fn shown(&self) -> u64 {
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u32 = 86_400;
    const N: u32 = 2_000_000_000;

    #[test]
    fn weight_ramps_from_30_days_to_2_years() {
        assert_eq!((weight(0), weight(29 * DAY), weight(30 * DAY)), (0, 0, 0));
        assert_eq!(weight(365 * DAY), 478);
        assert_eq!(weight(730 * DAY), 1000);
        assert_eq!(weight(3650 * DAY), 1000);
        assert_eq!(weight(u32::MAX), 1000);
    }

    #[test]
    fn file_score_is_zero_for_unknown_and_future_mtimes() {
        assert_eq!(file_score(1000, None, N), 0);
        assert_eq!(file_score(1000, NonZeroU32::new(N + 5), N), 0);
    }

    #[test]
    fn file_score_does_not_overflow() {
        assert_eq!(file_score(u64::MAX, NonZeroU32::new(1), u32::MAX), u64::MAX);
        assert_eq!(file_score(1000, NonZeroU32::new(N - 365 * DAY), N), 478);
    }

    #[test]
    fn shares_are_percents_and_guard_zero_divisors() {
        assert_eq!(cpu_share(250, 4), 6.25);
        assert_eq!(cpu_share(250, 0), 0.0);
        assert_eq!(mem_share(512, 1024), 50.0);
        assert_eq!(mem_share(1, 0), 0.0);
        assert_eq!(io_share(250_000_000), 50.0);
    }

    #[test]
    fn the_dead_band_holds_small_moves_and_snaps_idle_to_zero() {
        assert_eq!(settle(5, 5.9), 5);
        assert_eq!(settle(5, 6.0), 6);
        assert_eq!(settle(5, 4.0), 4);
        assert_eq!(settle(1, 0.49), 0);
        assert_eq!(settle(1, 0.5), 1);
        assert_eq!(settle(0, 0.9), 0);
        assert_eq!(settle(0, 1.0), 1);
    }

    #[test]
    fn the_average_follows_the_hand_computed_sequence() {
        let mut l = Load::default();
        assert_eq!(l.step(1.0, 100.0, 0.0, 0.0), 3, "ema 3.278");
        assert_eq!(l.step(1.0, 100.0, 0.0, 0.0), 6, "ema 6.449");
        assert_eq!(l.step(0.0, 0.0, 0.0, 0.0), 6, "dt = 0 moves nothing");
    }

    #[test]
    fn a_constant_load_converges_and_an_idle_one_returns_to_zero() {
        let mut l = Load::default();
        for _ in 0..3000 {
            l.step(1.0, 40.0, 0.0, 0.0);
        }
        assert!((39..=40).contains(&l.shown()), "within a point of 40: {}", l.shown());
        for _ in 0..3000 {
            l.step(1.0, 0.0, 0.0, 0.0);
        }
        assert_eq!(l.shown(), 0);
    }

    #[test]
    fn memory_is_not_smoothed_and_the_largest_term_wins() {
        let mut l = Load::default();
        assert_eq!(l.step(1.0, 0.0, 0.0, 30.0), 30);
    }
}
