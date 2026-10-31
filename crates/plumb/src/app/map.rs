//! The treemap as application state: which node it shows, its tiles, and selecting from it.
use super::*;

impl App {
    /// The tree was replaced, so every remembered id is meaningless.
    pub(super) fn map_reset(&mut self) {
        self.map_seen = None;
        self.map_root = None;
        self.map_dirty = true;
        self.map_hover = None;
    }

    /// Selects the node of map tile `i` (not `Rest`, which has none; `Own` selects its folder) and brings its row into view; the map root stays.
    pub(super) fn select_from_map(&mut self, i: usize) -> Option<NodeId> {
        let t = *self.map_tiles.get(i)?;
        if t.kind == Kind::Rest {
            return None;
        }
        let (tree, m) = (self.tree.as_ref()?, self.model.as_mut()?);
        m.reveal(tree, t.node);
        self.map_seen = Some(t.node);
        let s = m.scroll_to_selected(self.scroll, self.layout.rows_visible());
        self.set_scroll(s);
        Some(t.node)
    }

    /// Whether `map_root` still names a live node of the current tree.
    pub(super) fn live_map_root(&self) -> Option<NodeId> {
        let t = self.tree.as_ref()?;
        let sel = self.model.as_ref().and_then(|m| m.selected());
        self.map_root.filter(|&r| r.idx() < t.len() && !t.flags[r].contains(NodeFlags::DEAD)).or_else(|| treemap::root_for(t, sel))
    }

    /// Rebuilds the tiles if the tree, the root, the pane's rectangle or the scale changed since they were built.
    /// `px` is the size of one physical pixel in DIPs (`96 / dpi`). Call before painting.
    pub fn update_map(&mut self, px: f32) {
        self.update_tiles(px);
        self.refresh_basket();
    }

    pub(super) fn update_tiles(&mut self, px: f32) {
        self.follow_selection();
        let (rect, root) = (self.layout.map(), self.live_map_root());
        let (Some(tree), Some(rect), Some(root)) = (&self.tree, rect, root) else {
            self.map_tiles.clear();
            self.map_key = None;
            self.map_classes.clear();
            self.map_hover = None;
            return;
        };
        let key = (root, [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits(), rect.h.to_bits(), px.to_bits()]);
        if !self.map_dirty && self.map_key == Some(key) {
            return;
        }
        self.map_tiles = if self.kind() == ViewKind::Processes {
            let by_location = |id: NodeId| {
                let pid = tree.aux.get(id.idx()).map_or(0, |a| a[2]) as u32;
                location_class(self.proc_info.get(&pid).and_then(|i| i.path.as_deref()), &self.system_root)
            };
            treemap::layout_by(tree, root, rect, px, &by_location)
        } else {
            treemap::layout(tree, root, rect, px)
        };
        self.map_classes = treemap::class_bytes(&self.map_tiles);
        self.map_now = today();
        (self.map_key, self.map_dirty, self.map_hover) = (Some(key), false, None);
        self.map_gen += 1;
    }

    pub fn map_tiles(&self) -> &[Tile] {
        &self.map_tiles
    }

    /// The tile of the selected node, if it has one (a node inside a `Rest` tile has none, and `Own` is not the node's tile).
    pub fn map_selected_tile(&self) -> Option<usize> {
        let sel = self.model.as_ref()?.selected()?;
        self.map_tiles.iter().position(|t| t.node == sel && !t.kind.is_pseudo())
    }
}
