//! The toast: what an action reports (a delete's outcome, a refused mark, a failed shell call), shown over the list for a while.
//! Its time runs only while the pointer is off it, and counts from the first tick after it is shown, so the app needs no clock of its own.
use crate::outcome::Outcome;
use std::time::{Duration, Instant};

/// How long good news stays.
pub const OK_FOR: Duration = Duration::from_secs(4);
/// How long a problem stays: long enough to read a reason.
pub const PROBLEM_FOR: Duration = Duration::from_secs(10);
/// Detail lines shown under the heading line; the rest is "and N more".
pub const DETAIL_SHOWN: usize = 3;

#[derive(Clone, Debug, Default)]
pub struct Toast {
    pub outcome: Outcome,
    left: Duration,
    last: Option<Instant>,
}

impl Toast {
    /// Replaces what is shown; its time starts at the next tick.
    pub fn show(&mut self, outcome: Outcome) {
        self.left = if outcome.ok { OK_FOR } else { PROBLEM_FOR };
        self.outcome = outcome;
        self.last = None;
    }

    pub fn dismiss(&mut self) {
        *self = Self::default();
    }

    pub fn active(&self) -> bool {
        !self.outcome.is_empty()
    }

    /// Spends the time since the last tick unless the pointer is `hot` on the toast; true while it is still showing.
    pub fn tick(&mut self, now: Instant, hot: bool) -> bool {
        if let Some(last) = self.last.filter(|_| !hot) {
            self.left = self.left.saturating_sub(now.saturating_duration_since(last));
            if self.left.is_zero() {
                self.dismiss();
            }
        }
        if self.active() {
            self.last = Some(now);
        }
        self.active()
    }

    /// The lines drawn: the heading line, then up to `DETAIL_SHOWN` detail lines and "and N more".
    pub fn lines(&self) -> Vec<String> {
        if !self.active() {
            return Vec::new();
        }
        let d = &self.outcome.detail;
        let mut v = vec![self.outcome.line.clone()];
        v.extend(d.iter().take(DETAIL_SHOWN).cloned());
        if d.len() > DETAIL_SHOWN {
            v.push(format!("and {} more", d.len() - DETAIL_SHOWN));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problem_outlasts_good_news_and_time_starts_at_the_first_tick() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut t = Toast::default();
        t.show(Outcome::ok("deleted 2 items"));
        assert!(t.tick(t0, false), "the first tick only starts the clock");
        assert!(t.tick(t0 + s(3), false));
        assert!(!t.tick(t0 + s(4), false) && !t.active());
        t.show(Outcome::problem("could not mark x: y"));
        assert!(t.tick(t0, false) && t.tick(t0 + s(9), false));
        assert!(!t.tick(t0 + s(10), false));
    }

    #[test]
    fn the_pointer_on_the_toast_holds_its_time() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut t = Toast::default();
        t.show(Outcome::ok("copied"));
        t.tick(t0, false);
        assert!(t.tick(t0 + s(30), true), "held while the pointer is on it");
        assert!(t.tick(t0 + s(33), false), "3 s of 4 spent after it leaves");
        assert!(!t.tick(t0 + s(35), false));
    }

    #[test]
    fn a_new_toast_replaces_the_old_and_dismiss_clears_it() {
        let mut t = Toast::default();
        t.show(Outcome::problem("a"));
        t.show(Outcome::ok("b"));
        assert_eq!(t.lines(), ["b"]);
        t.dismiss();
        assert!(!t.active() && t.lines().is_empty());
    }

    #[test]
    fn detail_is_cut_after_three_lines() {
        let mut t = Toast::default();
        let detail = (1..=5).map(|i| format!("d{i}")).collect();
        t.show(Outcome { line: "5 of 5 not marked: x".into(), ok: false, detail });
        assert_eq!(t.lines(), ["5 of 5 not marked: x", "d1", "d2", "d3", "and 2 more"]);
    }
}
