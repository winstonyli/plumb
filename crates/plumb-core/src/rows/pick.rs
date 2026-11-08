//! The picked rows: the set that Shift and Ctrl clicks and Shift+arrows build, beside the focus row `selected` (the
//! cursor, which the rest of the program reads as "the selection"). Picks only ever name rows that exist, so
//! `picked_n` is the number the user sees and `targets` needs no visibility test.
use super::*;

/// Clears `id`'s bit and counts it out, for a caller that holds `picked` apart from the rest of the model.
pub(super) fn unpick(picked: &mut Vec<u64>, n: &mut usize, id: NodeId) {
    if bits::get(picked, id.idx()) {
        bits::set(picked, id.idx(), false);
        *n -= 1;
    }
}

impl RowModel {
    /// Picks only `id` and makes it the focus and the anchor.
    pub fn select(&mut self, id: NodeId) {
        self.clear_picks();
        self.set_pick(id, true);
        self.selected = Some(id);
        self.anchor = Some(id);
    }

    /// Picks the rows from the anchor to `id` by row order, inclusive, and moves the focus to `id`. The anchor stays;
    /// a missing one (none, or its row is collapsed or filtered away) is replaced by the focus, or by `id` when
    /// there is no focus either.
    pub fn extend_to(&mut self, id: NodeId) {
        let live = |m: &Self, n: Option<NodeId>| n.filter(|&n| m.row_index(n).is_some());
        let anchor = live(self, self.anchor).or(live(self, self.selected)).unwrap_or(id);
        let (Some(a), Some(b)) = (self.row_index(anchor), self.row_index(id)) else {
            self.select(id);
            return;
        };
        self.clear_picks();
        for i in a.min(b)..=a.max(b) {
            self.set_pick(self.rows[i].id, true);
        }
        self.selected = Some(id);
        self.anchor = Some(anchor);
    }

    /// Flips `id`'s pick and makes it the focus and the anchor. Unpicking the last row leaves nothing picked.
    pub fn toggle_pick(&mut self, id: NodeId) {
        self.set_pick(id, !self.is_picked(id));
        self.selected = Some(id);
        self.anchor = Some(id);
    }

    /// Picks every row. The focus and the anchor stay; with no focus it goes to the first row.
    pub fn select_all(&mut self) {
        let Some(first) = self.rows.first().map(|r| r.id) else { return };
        self.clear_picks();
        for i in 0..self.rows.len() {
            self.set_pick(self.rows[i].id, true);
        }
        if self.selected.is_none() {
            self.selected = Some(first);
        }
    }

    /// The picked rows, in row order.
    pub fn targets(&self) -> Vec<NodeId> {
        if self.picked_n == 0 {
            return Vec::new();
        }
        self.rows.iter().map(|r| r.id).filter(|&id| self.is_picked(id)).collect()
    }

    pub fn picked_count(&self) -> usize {
        self.picked_n
    }

    pub fn is_picked(&self, id: NodeId) -> bool {
        bits::get(&self.picked, id.idx())
    }

    /// Selects the focus row's parent, if that has a row.
    pub fn select_parent(&mut self, tree: &Tree) {
        let Some(s) = self.selected else { return };
        let p = tree.parent[s];
        if !p.is_none() && self.row_index(p).is_some() {
            self.select(p);
        }
    }

    fn set_pick(&mut self, id: NodeId, on: bool) {
        if self.is_picked(id) != on {
            bits::set(&mut self.picked, id.idx(), on);
            if on {
                self.picked_n += 1;
            } else {
                self.picked_n -= 1;
            }
        }
    }

    fn clear_picks(&mut self) {
        if self.picked_n > 0 {
            self.picked.fill(0);
            self.picked_n = 0;
        }
    }

    /// Keeps only the picks that have a row, after the rows were rebuilt.
    pub(super) fn prune_picks(&mut self) {
        if self.picked_n == 0 {
            return;
        }
        let mut keep = vec![0u64; self.picked.len()];
        let mut n = 0;
        for r in &self.rows {
            if self.is_picked(r.id) {
                bits::set(&mut keep, r.id.idx(), true);
                n += 1;
            }
        }
        self.picked = keep;
        self.picked_n = n;
    }

    /// After a rebuild moved the focus off a row that went (to an ancestor, or to nothing) the set follows it: the new
    /// focus alone, or nothing. `before` is the focus from before the rebuild.
    pub(super) fn follow_focus(&mut self, before: Option<NodeId>) {
        if self.selected == before {
            return;
        }
        match self.selected {
            Some(f) => self.select(f),
            None => {
                self.clear_picks();
                self.anchor = None;
            }
        }
    }
}
