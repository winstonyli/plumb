//! Last-write time as a whole day, and how faded a tile of that age is drawn.

use std::num::NonZeroU32;

/// 100 ns FILETIME ticks in a day.
pub const DAY_TICKS: u64 = 864_000_000_000;
/// Days from 1601-01-01 to 1970-01-01.
pub const UNIX_EPOCH_DAYS: u32 = 134_774;
/// Age (days) at which the fade is complete.
pub const AGE_FULL_DAYS: f32 = 3650.0;
/// How far toward the background the oldest tile is mixed (0 = not at all, 1 = vanishes).
pub const FADE_MAX: f32 = 0.65;

/// The day a FILETIME falls on, or `None` if it is under one day (0 means "unset").
pub fn day_of(filetime: u64) -> Option<NonZeroU32> {
    NonZeroU32::new((filetime / DAY_TICKS) as u32) // u64::MAX is only 21 million days: it always fits
}

/// The day (since 1601) that Unix time `secs` falls on.
pub fn day_from_unix(secs: u64) -> u32 {
    UNIX_EPOCH_DAYS + u32::try_from(secs / 86_400).unwrap_or(u32::MAX - UNIX_EPOCH_DAYS)
}

/// 0.0 for a file written today (or in the future) or of unknown age, rising on a log scale to 1.0 at
/// [`AGE_FULL_DAYS`] and staying there.
pub fn shade(now: u32, mtime: Option<NonZeroU32>) -> f32 {
    let Some(m) = mtime else { return 0.0 };
    let age = now.saturating_sub(m.get()) as f32;
    ((1.0 + age).ln() / (1.0 + AGE_FULL_DAYS).ln()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(n)
    }

    #[test]
    fn filetimes_under_a_day_are_unknown() {
        assert_eq!(day_of(0), None);
        assert_eq!(day_of(DAY_TICKS - 1), None);
        assert_eq!(day_of(DAY_TICKS), d(1));
        assert_eq!(day_of(u64::MAX), d(21_350_398));
    }

    #[test]
    fn unix_time_maps_onto_the_1601_day_count() {
        assert_eq!(day_from_unix(0), 134_774);
        assert_eq!(day_from_unix(86_399), 134_774);
        assert_eq!(day_from_unix(86_400), 134_775);
    }

    #[test]
    fn shade_is_zero_for_unknown_and_fresh_rises_and_stops() {
        let now = 150_000;
        assert_eq!(shade(now, None), 0.0);
        assert_eq!(shade(now, d(now)), 0.0);
        assert_eq!(shade(now, d(now + 5)), 0.0); // a future time
        let (week, year, decade, ancient) =
            (shade(now, d(now - 7)), shade(now, d(now - 365)), shade(now, d(now - 3650)), shade(now, d(100)));
        assert!(0.0 < week && week < year && year < decade);
        assert!((decade - 1.0).abs() < 1e-6);
        assert_eq!(ancient, 1.0);
    }
}
