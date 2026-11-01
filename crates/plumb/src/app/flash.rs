//! Rows that live updates just changed: tinted when they change, fading out over `FADE`.
use plumb_core::NodeId;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a change stays visible.
pub const FADE: Duration = Duration::from_secs(2);
/// More changed nodes than this in one refresh is a bulk change (a build, an unpack): tinting all of them says nothing.
const MAX_BATCH: usize = 256;

#[derive(Default)]
pub struct Flash {
    at: HashMap<NodeId, Instant>,
}

impl Flash {
    /// Notes that `ids` changed at `now` (a bulk change is not noted).
    pub fn mark(&mut self, ids: &[NodeId], now: Instant) {
        if ids.len() <= MAX_BATCH {
            self.at.extend(ids.iter().map(|&id| (id, now)));
        }
    }

    /// 1 when `id` just changed, falling to 0 at `FADE`; 0 for a node that did not change.
    pub fn alpha(&self, id: NodeId, now: Instant) -> f32 {
        self.at.get(&id).map_or(0.0, |&t| (1.0 - now.saturating_duration_since(t).as_secs_f32() / FADE.as_secs_f32()).max(0.0))
    }

    /// Forgets what has faded; true while anything is still visible.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.at.retain(|_, &mut t| now.saturating_duration_since(t) < FADE);
        self.active()
    }

    pub fn active(&self) -> bool {
        !self.at.is_empty()
    }

    pub fn clear(&mut self) {
        self.at.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_fades_linearly_over_two_seconds_and_is_then_forgotten() {
        let t0 = Instant::now();
        let mut f = Flash::default();
        let (a, b) = (NodeId(1), NodeId(2));
        f.mark(&[a], t0);
        assert!((f.alpha(a, t0) - 1.0).abs() < 1e-6 && f.alpha(b, t0) == 0.0);
        assert!((f.alpha(a, t0 + Duration::from_secs(1)) - 0.5).abs() < 1e-3);
        assert_eq!(f.alpha(a, t0 + FADE), 0.0);
        assert!(f.tick(t0 + Duration::from_millis(1999)) && f.active());
        assert!(!f.tick(t0 + FADE) && !f.active());
    }

    #[test]
    fn a_second_change_restarts_the_fade_and_a_bulk_change_marks_nothing() {
        let t0 = Instant::now();
        let mut f = Flash::default();
        f.mark(&[NodeId(1)], t0);
        f.mark(&[NodeId(1)], t0 + Duration::from_secs(1));
        assert!((f.alpha(NodeId(1), t0 + Duration::from_secs(1)) - 1.0).abs() < 1e-6);
        let bulk: Vec<NodeId> = (0..=MAX_BATCH as u32).map(NodeId).collect();
        f.mark(&bulk, t0);
        assert!(f.alpha(NodeId(7), t0) == 0.0, "a bulk change is not tinted");
    }
}
