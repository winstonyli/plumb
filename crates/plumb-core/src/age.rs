//! Last-write time as unix seconds in a `u32` (good until 2106), and how faded a tile of that age is drawn. Days, ages
//! and dates are worked out from the seconds where they are shown, by the local calendar.

use std::num::NonZeroU32;

/// 100 ns FILETIME ticks in a day.
pub const DAY_TICKS: u64 = 864_000_000_000;
/// 100 ns FILETIME ticks in a second.
pub const SEC_TICKS: u64 = 10_000_000;
/// Days from 1601-01-01 to 1970-01-01.
pub const UNIX_EPOCH_DAYS: u32 = 134_774;
/// Age (days) at which the fade is complete.
pub const AGE_FULL_DAYS: f32 = 3650.0;
/// How far toward the background the oldest tile is mixed (0 = not at all, 1 = vanishes).
pub const FADE_MAX: f32 = 0.65;

/// The unix seconds a FILETIME falls on, or `None` if it is unset (0). A time before 1970 reads as 1 (the oldest there
/// is) and one after 2106 as the largest.
pub fn secs_of(filetime: u64) -> Option<NonZeroU32> {
    if filetime == 0 {
        return None;
    }
    let secs = (filetime / SEC_TICKS).saturating_sub(u64::from(UNIX_EPOCH_DAYS) * 86_400);
    NonZeroU32::new(u32::try_from(secs).unwrap_or(u32::MAX).max(1))
}

/// The FILETIME of unix time `secs`.
pub fn filetime_of(secs: u32) -> u64 {
    (u64::from(secs) + u64::from(UNIX_EPOCH_DAYS) * 86_400) * SEC_TICKS
}

/// Now, in unix seconds.
pub fn now_secs() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}

/// 0.0 for a file written now (or in the future) or of unknown age, rising on a log scale to 1.0 at
/// [`AGE_FULL_DAYS`] and staying there.
pub fn shade(now: u32, mtime: Option<NonZeroU32>) -> f32 {
    let Some(m) = mtime else { return 0.0 };
    let age = now.saturating_sub(m.get()) as f32 / 86_400.0;
    ((1.0 + age).ln() / (1.0 + AGE_FULL_DAYS).ln()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(n)
    }

    #[test]
    fn a_filetime_maps_onto_unix_seconds_and_back_with_the_ends_clamped() {
        assert_eq!(secs_of(0), None, "unset");
        assert_eq!(secs_of(filetime_of(1)), d(1));
        assert_eq!(secs_of(filetime_of(1_700_000_000) + SEC_TICKS - 1), d(1_700_000_000), "whole seconds");
        assert_eq!(secs_of(5 * DAY_TICKS), d(1), "before 1970 is the oldest");
        assert_eq!(secs_of(filetime_of(0)), d(1), "1970-01-01 00:00:00 itself too: zero is not a time");
        assert_eq!(secs_of(u64::MAX), d(u32::MAX), "after 2106 is the largest");
    }

    #[test]
    fn shade_is_zero_for_unknown_and_fresh_rises_and_stops() {
        let now = 2_000_000_000;
        let ago = |days: u32| d(now - days * 86_400);
        assert_eq!(shade(now, None), 0.0);
        assert_eq!(shade(now, d(now)), 0.0);
        assert_eq!(shade(now, d(now + 5)), 0.0); // a future time
        let (week, year, decade, ancient) = (shade(now, ago(7)), shade(now, ago(365)), shade(now, ago(3650)), shade(now, d(100)));
        assert!(0.0 < week && week < year && year < decade);
        assert!((decade - 1.0).abs() < 1e-6);
        assert_eq!(ancient, 1.0);
    }
}
