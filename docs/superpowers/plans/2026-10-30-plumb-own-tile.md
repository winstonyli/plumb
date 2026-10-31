---
status: draft
---

# Own-memory tile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A folder's own bytes (a parent process's own memory, CPU or I/O) are drawn as a tile inside its frame, so the children no longer fill a frame that is sized for more than they hold.

**Architecture:** `treemap::Ctx::fill` today lays out `subtree_size - size` among the children and drops the folder's own `size`, while the frame itself is sized by the full `subtree_size`. It now lays out `subtree_size` among the children plus one more item for the folder itself, a new `Kind::Own` tile (`node` = the folder, like `Rest`). The item goes through the same size cutoff as a child; below it, it merges into `Rest`. `dir` stands for the own item in the item list, since a folder is never its own child. Nothing changes on disk views: directories there have `size` 0 (`plumb-scan/src/listing.rs:5`).

**Tech Stack:** Rust; `plumb-core::treemap`, `plumb` (`app.rs`, `draw.rs`).

**Spec:** `ROADMAP.md` v0.10 (the first of its three parts); layout rules in `docs/superpowers/specs/2026-10-14-plumb-treemap-design.md`; the survey that chose this design is in the 2026-10-30 session (option j1: own tile inside the frame).

## Global Constraints

- Disk views must not change: the existing `plumb-core` treemap tests and `plumb/tests/render.rs` stay green and no disk tile list gains an `Own` tile.
- No compatibility shims; the subtraction at `treemap.rs:187` is deleted, not kept behind a flag.
- Build with `CARGO_TARGET_DIR=target/alt` while the user's `plumb.exe` runs; delete `target/alt` afterwards. Windows only.
- Edits use the Edit tool; commits are small and topical after `git diff --stat`; nothing is pushed or tagged.

## Review Focus

- A folder whose own bytes are tiny next to its children (the normal case): no `Own` tile, the bytes land in `Rest`, and the group still tiles the frame.
- A folder with own bytes and no children (a process whose last child just left, for one snapshot): one `Own` tile fills the frame (before: an empty frame).
- The map rooted at a folder that has own bytes: an `Own` tile at depth 0 with `node` = the root, and no outline when the root is selected.
- Clicking an `Own` tile selects the folder's row; hovering names the folder itself, not the folder's whole subtree.
- A folder whose own bytes are zero (every disk folder): tile list identical to before.

---

### Task 1: Lay out the own bytes (`plumb-core`)

**Files:**
- Modify: `crates/plumb-core/src/treemap.rs` (`Kind`, `Ctx::fill`)
- Test: `crates/plumb-core/src/treemap/tests.rs`

**Interfaces:**
- Produces: `Kind::Own` (variant); `Kind::is_pseudo(self) -> bool` (true for `Rest` and `Own`: a tile that is not a node's own tile); an `Own` tile has `node` = the folder, `bytes` = its own bytes, `count` 1, `depth` = the depth of the folder's children, `class` `Other`, `mtime` `None`.

- [ ] **Step 1: Write the failing tests.** Append to `treemap/tests.rs`:

```rust
#[test]
fn a_folders_own_bytes_get_an_own_tile_beside_its_children() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let p = b.push_str(root, "parent.exe", 600_000, NodeFlags::DIR);
    let c1 = file(&mut b, p, "a.exe", 200_000);
    let c2 = file(&mut b, p, "b.exe", 200_000);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 }, 1.0);
    assert_eq!((tiles[0].node, tiles[0].kind), (p, Kind::Frame));
    let f = tiles[0].rect;
    let kids = kids(&tiles, &tree, 0);
    assert_eq!(kids.len(), 3);
    let own = kids.iter().map(|&i| tiles[i]).find(|t| t.kind == Kind::Own).expect("an own tile");
    assert_eq!((own.node, own.bytes, own.count, own.depth), (p, 600_000, 1, 1));
    assert!(tiles.iter().any(|t| t.node == c1) && tiles.iter().any(|t| t.node == c2));
    check_group(&tiles, &kids, Rect { x: f.x + 1.0, y: f.y + 1.0, w: f.w - 2.0, h: f.h - 2.0 });
}

#[test]
fn tiny_own_bytes_merge_into_the_rest_tile() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let p = b.push_str(root, "parent.exe", 1, NodeFlags::DIR);
    file(&mut b, p, "big.exe", 1_000_000_000);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 }, 1.0);
    assert!(tiles.iter().all(|t| t.kind != Kind::Own));
    let rest = tiles.iter().find(|t| t.kind == Kind::Rest).expect("a rest tile");
    assert_eq!((rest.node, rest.count, rest.bytes), (p, 1, 1));
}

#[test]
fn a_folder_with_own_bytes_and_no_children_is_one_own_tile() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let p = b.push_str(root, "parent.exe", 5_000, NodeFlags::DIR);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, 1.0);
    assert_eq!(tiles.len(), 2);
    assert_eq!((tiles[0].node, tiles[0].kind), (p, Kind::Frame));
    assert_eq!((tiles[1].node, tiles[1].kind, tiles[1].bytes), (p, Kind::Own, 5_000));
}

#[test]
fn a_root_with_own_bytes_gets_an_own_tile_at_depth_zero() {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "root", 300, NodeFlags::DIR);
    let c = file(&mut b, root, "c", 300);
    let tree = b.finish().unwrap();
    let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    let tiles = layout(&tree, root, r, 1.0);
    assert_eq!(tiles.len(), 2);
    let own = tiles.iter().find(|t| t.kind == Kind::Own).expect("an own tile");
    assert_eq!((own.node, own.depth), (root, 0));
    assert!(tiles.iter().any(|t| t.node == c && t.kind == Kind::File));
    check_group(&tiles, &[0, 1], r);
}
```

Also in `kids()` (`tests.rs:107`) change `if t.kind == Kind::Rest {` to `if t.kind.is_pseudo() {`.

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-core treemap 2>&1 | tail -20`
Expected: compile error, `no variant named Own` and `no method named is_pseudo`.

- [ ] **Step 3: Implement.** In `treemap.rs`, add to `Kind` after `Rest`:

```rust
    /// A folder's own bytes, as opposed to what is inside it (a process's own memory). `node` is the folder.
    Own,
```

and below the enum:

```rust
impl Kind {
    /// A tile that is not the whole of a node's subtree: `Rest` and `Own` have no row of their own.
    pub fn is_pseudo(self) -> bool {
        matches!(self, Kind::Rest | Kind::Own)
    }
}
```

In `Ctx::fill`, replace the body up to and including the emit loop with:

```rust
        let t = self.tree;
        let total = t.subtree_size[dir];
        let a = area.area();
        if total == 0 || a.is_nan() || a <= 0.0 {
            return;
        }
        // One O(k) pass: the items big enough to get MIN_PX2 are kept, the rest are summed. The folder's own
        // bytes are an item like a child; `dir` stands for them, as a folder is never its own child.
        let min_area = MIN_PX2 * self.px * self.px;
        let mut kept: Vec<(NodeId, u64)> = Vec::new();
        let (mut rest_bytes, mut rest_n) = (0u64, 0u32);
        for c in std::iter::once(dir).chain(t.children(dir)) {
            let w = if c == dir { t.size[dir] } else { t.subtree_size[c] };
            if w == 0 {
                continue;
            }
            if w as f64 * a >= total as f64 * min_area {
                kept.push((c, w));
            } else {
                rest_bytes += w;
                rest_n += 1;
            }
        }
        kept.sort_unstable_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        if rest_n > 0 {
            kept.push((NodeId::NONE, rest_bytes)); // NONE stands for the Rest tile, laid out last
        }
        let weights: Vec<f64> = kept.iter().map(|k| k.1 as f64).collect();
        for (&(c, w), b) in kept.iter().zip(squarify(&weights, area)) {
            if c.is_none() {
                out.push(Tile { node: dir, rect: b.rect(), depth, class: Class::Other, kind: Kind::Rest, bytes: w, count: rest_n, mtime: None });
            } else if c == dir {
                out.push(Tile { node: dir, rect: b.rect(), depth, class: Class::Other, kind: Kind::Own, bytes: w, count: 1, mtime: None });
            } else {
                self.emit(c, b, depth, out);
            }
        }
```

(Keep the function's `fn fill(...)` signature and doc comment; update the doc to "Lays out the children of `dir`, and its own bytes, in `area`.")

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-core 2>&1 | tail -15`
Expected: all pass, including the pre-existing treemap tests.

- [ ] **Step 5: Commit.**
```bash
git add crates/plumb-core/src/treemap.rs crates/plumb-core/src/treemap/tests.rs
git commit -m "Treemap: a folder's own bytes are an Own tile beside its children"
```

---

### Task 2: Select, hover and draw an Own tile (`plumb`)

**Files:**
- Modify: `crates/plumb/src/app.rs` (`tile_tip` ~242, `process_tip` ~700, `map_selected_tile` ~1300), `crates/plumb/src/draw.rs` (`tile`, ~899)
- Test: `crates/plumb/src/app.rs` (tests module, next to `a_rest_tile_says_how_many_items_it_holds`)

**Interfaces:**
- Consumes: `Kind::Own`, `Kind::is_pseudo` from Task 1.
- Produces: `select_from_map` selects an `Own` tile's folder (no change needed: it only refuses `Rest`); `map_selected_tile` skips pseudo tiles; tooltips: disk `"<path> itself  (<size>)"`, Processes `"<name> itself  (PID n)\n<size> <metric> in the process itself"`.

- [ ] **Step 1: Write the failing tests.** In `app.rs` tests, after `a_rest_tile_says_how_many_items_it_holds`:

```rust
    #[test]
    fn an_own_tile_tip_names_the_folder_itself() {
        let tree = plumb_core::synth::flat_tree(3);
        let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Own, bytes: 2048, count: 1, mtime: None };
        let tip = tile_tip(&tree, &t, 150_000);
        assert!(tip.contains(&tree.name_string(NodeId(0))) && tip.contains("itself"), "{tip}");
    }

    #[test]
    fn a_parent_processs_own_memory_has_a_tile_that_selects_it_but_is_not_outlined() {
        use plumb_core::procs::{Metric, Procs, Sample};
        let mut a = app();
        a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
        a.begin_scan(2);
        let (mut procs, mut tree) = Procs::new(Metric::Memory);
        let s = |pid, parent, name: &str, memory| Sample { pid, parent, created: 5, name: name.into(), memory, cpu_100ns: 0, io_bytes: 0 };
        procs.apply(&mut tree, &[s(77, 1, "parent.exe", 400_000), s(78, 77, "child.exe", 100_000)], 0);
        let parent = tree.find("Processes\\parent.exe").expect("the parent is under the root");
        a.finish_scan(tree, "x".into());
        a.layout.map_on = true;
        a.update_map(1.0);
        let i = a.map_tiles().iter().position(|t| t.kind == Kind::Own && t.node == parent).expect("an own tile for the parent");
        assert_eq!(a.map_tiles()[i].bytes, 400_000);
        assert_eq!(a.select_from_map(i), Some(parent), "a click on the own tile selects the process");
        let sel = a.map_selected_tile().expect("the process is outlined");
        assert_eq!(a.map_tiles()[sel].kind, Kind::Frame, "by its frame, not by the own tile");
        let tip = a.process_tip(a.tree.as_ref().unwrap(), &a.map_tiles()[i]);
        assert!(tip.contains("parent.exe") && tip.contains("itself"), "{tip}");
    }
```

The render test (`tests/render.rs`) needs no edit: its Processes samples have no parent with own memory, and it keeps guarding that nothing else about the frames changed.

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb --lib own_tile 2>&1 | tail -15`
Expected: the tip test fails on "itself"; the selection test fails or the match in `tile_tip`/`draw.rs::tile` is non-exhaustive (compile error `non-exhaustive patterns: Own`).

- [ ] **Step 3: Implement.**

`tile_tip` (`app.rs`): add the arm before `_`:

```rust
        Kind::Own => format!("{} itself  ({})", fmt::display_path(&tree.path(t.node)), fmt::size(t.bytes)),
```

`process_tip`: after the `Kind::Rest` early return add:

```rust
        if t.kind == Kind::Own {
            let pid = tree.aux.get(t.node.idx()).map_or(0, |a| a[2]);
            return format!(
                "{}  (PID {pid})\n{} {} in the process itself",
                tree.name_string(t.node), self.kind().size_text(self.proc_metric, t.bytes), metric_label(self.proc_metric).to_lowercase()
            );
        }
```

`map_selected_tile`: change `t.kind != Kind::Rest` to `!t.kind.is_pseudo()` and update its doc to "a node inside a `Rest` tile has none, and `Own` is not the node's tile".

`select_from_map` keeps refusing only `Rest` (an `Own` tile selects its folder); update its doc comment to say so.

`draw.rs::tile`: add the arm after `Kind::Rest`:

```rust
            Kind::Own => {
                // The folder's own bytes: inside its frame, set off from the children's class colours.
                let inner = if r.w >= 4.0 && r.h >= 4.0 { Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 } } else { r };
                self.fill(inner, D2D1_COLOR_F { a: 0.35, ..t.dim });
            }
```

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb 2>&1 | tail -20`
Expected: pass, with the render test still writing its frames.

- [ ] **Step 5: Check the existing frames.** Run `CARGO_TARGET_DIR=target/alt cargo test -p plumb --test render` and confirm it passes. A frame that shows a parent process may gain an Own tile; any other frame change is a bug.

- [ ] **Step 6: Commit.**
```bash
git add crates/plumb/src/app.rs crates/plumb/src/draw.rs
git commit -m "Processes map: hover, select and draw a parent's own memory"
```

---

### Task 3: Docs and the parked entry

**Files:**
- Modify: `PARKED.md` (delete the "A process's own memory is invisible in the map" entry), `docs/superpowers/specs/2026-10-14-plumb-treemap-design.md` (Tile kinds and invariants), `ROADMAP.md` only if wording is stale.
- Modify: this plan's front matter `status: done`.

- [ ] **Step 1: Delete the PARKED entry** for the invisible own memory. Do not touch the other two entries that v0.10 pulls (they stay parked until done).
- [ ] **Step 2: Update the treemap spec.** In the "Tile" definition line (`... kind: File | Frame | Solid | Rest ...`) add `Own` with its note ("a folder's own bytes, `node` is the folder; present only where a folder has its own `size`, today the Processes view"), and in the invariants say the child tiles including `Rest` and `Own` sum to the inner area.
- [ ] **Step 3: Grep for stale wording.**
Run: `grep -rn "own memory\|subtree_size - \|Kind::Rest\|File | Frame | Solid | Rest" PARKED.md ROADMAP.md CLAUDE.md docs crates --include=*.md --include=*.rs | grep -v "docs/superpowers/plans/"`
Expected: no remaining claim that a parent's own memory is invisible.
- [ ] **Step 4: Commit.**
```bash
git add PARKED.md docs
git commit -m "Docs: own-memory tile landed; treemap spec and PARKED follow"
```
