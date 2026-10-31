//! The basket of marked paths, counted against the tree, and the delete commit built from it.
use super::*;

/// What the context menu and `Del` can do with a node.
#[derive(Debug, PartialEq)]
pub enum MarkState {
    Mark,
    Unmark,
    Refused(&'static str),
}

/// What a commit would delete: the topmost marked paths of one view and what they hold.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitRequest {
    pub view: usize,
    pub paths: Vec<String>,
    pub bytes: u64,
}

/// The basket counted against the tree: derived, so rebuilt when the marks, the tree or the tiles change.
#[derive(Default)]
pub(super) struct BasketView {
    pub(super) key: Option<(u64, (usize, usize, u64), u64)>,
    pub(super) items: usize,
    /// Allocated bytes under the marked nodes; an upper bound, since a hard-linked file's other names free nothing.
    pub(super) bytes: u64,
    /// Indices into `map_tiles` to tint: tiles at or under a marked node.
    pub(super) tiles: Vec<usize>,
}

impl App {
    pub fn basket(&self) -> &Basket {
        &self.slot.basket
    }

    pub fn basket_active(&self) -> bool {
        self.basket_view.items > 0
    }

    /// "3 marked, up to 12.4 GB" for the status bar chip.
    pub fn basket_label(&self) -> Option<String> {
        self.basket_active().then(|| format!("{} marked, up to {}", self.basket_view.items, fmt::size(self.basket_view.bytes)))
    }

    /// Keeps the marks of `view`, whose state is being let go, for its next scan.
    pub fn stash_basket(&mut self, view: usize, from: &mut Slot) {
        if !from.basket.is_empty() {
            self.stash.insert(view, std::mem::take(&mut from.basket));
        }
    }

    pub fn clear_basket(&mut self) {
        self.slot.basket.clear();
        self.basket_rev += 1;
    }

    pub fn mark_state(&self, id: NodeId) -> MarkState {
        let Some(tree) = &self.slot.tree else { return MarkState::Refused("no drive is shown") };
        if self.kind() == ViewKind::Processes {
            return MarkState::Refused("processes cannot be deleted");
        }
        if self.slot.basket.contains(&tree.path(id)) {
            return MarkState::Unmark;
        }
        if self.cancelable() {
            return MarkState::Refused("the drive is being scanned");
        }
        basket::refusal(tree, id, &self.system_root).map_or(MarkState::Mark, MarkState::Refused)
    }

    /// Marks or unmarks `id`; a refusal goes to the status line.
    pub fn toggle_mark(&mut self, id: NodeId) {
        if let MarkState::Refused(why) = self.mark_state(id) {
            self.notice = format!("cannot mark: {why}");
            return;
        }
        let Some(path) = self.slot.tree.as_ref().map(|t| t.path(id)) else { return };
        self.slot.basket.toggle(&path);
        self.basket_rev += 1;
    }

    /// Marks or unmarks the node at `path`, found afresh: the tree may have changed since the caller chose it.
    pub fn toggle_mark_path(&mut self, path: &str) {
        match self.slot.tree.as_ref().and_then(|t| t.find(path)) {
            Some(id) => self.toggle_mark(id),
            None => self.notice = "that item is gone".into(),
        }
    }

    pub(super) fn toggle_mark_selected(&mut self) {
        if let Some(id) = self.slot.model.as_ref().and_then(|m| m.selected()) {
            self.toggle_mark(id);
        }
    }

    /// Rebuilds what the chip and the tiles show when the marks, the tree or the tiles changed.
    pub(super) fn refresh_basket(&mut self) {
        let Some(tree) = &self.slot.tree else {
            self.basket_view = BasketView::default();
            return;
        };
        if self.slot.basket.is_empty() {
            if self.basket_view.key.is_some() {
                self.basket_view = BasketView::default();
            }
            return;
        }
        let fingerprint = (tree.len(), tree.dead(), tree.rev());
        let key = (self.basket_rev, fingerprint, self.map_gen);
        if self.basket_view.key == Some(key) {
            return;
        }
        let ids = self.slot.basket.covered(tree);
        let set: HashSet<NodeId> = ids.iter().copied().collect();
        let under = |mut n: NodeId| {
            while !n.is_none() {
                if set.contains(&n) {
                    return true;
                }
                n = tree.parent[n];
            }
            false
        };
        let tiles = self.map_tiles.iter().enumerate().filter(|(_, t)| under(t.node)).map(|(i, _)| i).collect();
        self.basket_view = BasketView { key: Some(key), items: ids.len(), bytes: ids.iter().map(|&i| tree.subtree_size[i]).sum(), tiles };
    }

    pub fn deleting(&self) -> bool {
        self.deleting
    }

    /// Starts a delete if none runs and no scan does; otherwise says why not on the status line.
    pub fn begin_delete(&mut self) -> bool {
        if self.deleting {
            self.notice = "a delete is already running".into();
            return false;
        }
        if self.cancelable() {
            self.notice = "cannot delete while the drive is being scanned".into();
            return false;
        }
        self.deleting = true;
        true
    }

    pub fn set_deleting(&mut self, on: bool) {
        self.deleting = on;
    }

    /// What a commit would delete right now, or `None` (with the reason on the status line).
    pub fn commit_request(&mut self) -> Option<CommitRequest> {
        if self.deleting {
            self.notice = "a delete is already running".into();
            return None;
        }
        if self.cancelable() {
            self.notice = "cannot delete while the drive is being scanned".into();
            return None;
        }
        let (view, tree) = (self.active?, self.slot.tree.as_ref()?);
        self.slot.basket.prune(tree);
        self.basket_rev += 1;
        // A mark that cannot be deleted now (a rename gave it an unsafe name) is dropped and reported with the result.
        let (ids, refused): (Vec<NodeId>, Vec<NodeId>) = self.slot.basket.covered(tree).into_iter().partition(|&i| basket::refusal(tree, i, &self.system_root).is_none());
        self.skipped = refused.iter().map(|&i| (tree.path(i), basket::refusal(tree, i, &self.system_root).unwrap_or(""))).collect();
        let skipped: Vec<String> = self.skipped.iter().map(|(p, _)| p.clone()).collect();
        self.slot.basket.remove_under(&skipped);
        self.basket_rev += 1;
        if ids.is_empty() {
            self.clear_basket();
            self.notice = match self.skipped.first() {
                Some((_, why)) => format!("{} skipped: {why}", fmt::noun(self.skipped.len() as u64, "marked item", "marked items")),
                None => "the marked items are already gone".into(),
            };
            self.notice_detail = self.skipped.drain(..).map(|(p, why)| format!("{p}: {why}")).collect();
            return None;
        }
        let paths = ids.iter().map(|&i| tree.path(i)).collect();
        let bytes = ids.iter().map(|&i| tree.subtree_size[i]).sum();
        Some(CommitRequest { view, paths, bytes })
    }

    /// A delete ended: `gone` (deleted, `missing` of them already absent) leave the basket, `failed` (path, reason) stay.
    pub fn finish_delete(&mut self, gone: &[String], missing: usize, failed: &[(String, String)]) {
        self.deleting = false;
        self.slot.basket.remove_under(gone);
        self.basket_rev += 1;
        let deleted = gone.len() - missing.min(gone.len());
        let items = fmt::noun(deleted as u64, "item", "items");
        self.notice_ok = failed.is_empty() && self.skipped.is_empty();
        self.notice = match failed.first() {
            None if missing == 0 => format!("deleted {items}"),
            None => format!("deleted {items}, {missing} already gone"),
            Some((_, why)) => format!("{} of {} not deleted: {why}; still marked", failed.len(), gone.len() + failed.len()),
        };
        self.notice_detail = failed.iter().map(|(p, why)| format!("{p}: {why}")).collect();
        if let Some((_, why)) = self.skipped.first() {
            self.notice += &format!("; {} skipped: {why}", self.skipped.len());
            self.notice_detail.extend(self.skipped.drain(..).map(|(p, why)| format!("{p}: {why}")));
        }
    }
}
