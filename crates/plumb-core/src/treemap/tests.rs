use super::*;

fn class_of(name: &str) -> Class {
    classify(&name.encode_utf16().collect::<Vec<u16>>())
}

#[test]
fn classification_by_last_extension_ignores_case_and_defaults_to_other() {
    for (name, want) in [
        ("movie.MP4", Class::Video),
        ("song.flac", Class::Audio),
        ("IMG_1.JpG", Class::Image),
        ("a.tar.gz", Class::Archive),
        ("report.docx", Class::Document),
        ("lib.rs", Class::Code),
        ("setup.exe", Class::Executable),
        ("disk.vhdx", Class::DiskImage),
        ("data.sqlite3", Class::Database),
        ("plumb.pdb", Class::Build),
        ("libx.rlib", Class::Build),
        ("map.wad", Class::Archive),
        ("noext", Class::Other),
        ("trailing.", Class::Other),
        (".hidden", Class::Other),
        ("x.y.", Class::Other),
        ("weird.\u{fc}n\u{ef}", Class::Other),
        ("a.verylongextension", Class::Other),
    ] {
        assert_eq!(class_of(name), want, "{name}");
    }
}

#[test]
fn all_lists_every_class_in_index_order() {
    for (i, c) in Class::ALL.iter().enumerate() {
        assert_eq!(c.index(), i);
        assert!(!c.label().is_empty());
    }
}
use crate::node::{NodeFlags, NodeId};
use crate::tree::{Tree, TreeBuilder};

fn file(b: &mut TreeBuilder, p: NodeId, name: &str, size: u64) -> NodeId {
    b.push_str(p, name, size, NodeFlags::empty())
}

fn dir(b: &mut TreeBuilder, p: NodeId, name: &str) -> NodeId {
    b.push_str(p, name, 0, NodeFlags::DIR)
}

/// root: a (a1.rs 400, a2.rs 300, a3.png 100, nest (n1.log 250, n2.db 50)), c.mp4 900, d.zip 500, e.txt 120, zero.bin 0.
fn sample() -> (Tree, NodeId) {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let a = dir(&mut b, root, "a");
    file(&mut b, a, "a1.rs", 400);
    file(&mut b, a, "a2.rs", 300);
    file(&mut b, a, "a3.png", 100);
    let nest = dir(&mut b, a, "nest");
    file(&mut b, nest, "n1.log", 250);
    file(&mut b, nest, "n2.db", 50);
    file(&mut b, root, "c.mp4", 900);
    file(&mut b, root, "d.zip", 500);
    file(&mut b, root, "e.txt", 120);
    file(&mut b, root, "zero.bin", 0);
    (b.finish().unwrap(), root)
}

const R: Rect = Rect { x: 10.0, y: 20.0, w: 400.0, h: 300.0 };

fn area(r: Rect) -> f64 {
    r.w as f64 * r.h as f64
}

fn inside(r: Rect, o: Rect) -> bool {
    let e = 1e-3;
    r.x >= o.x - e && r.y >= o.y - e && r.x + r.w <= o.x + o.w + e && r.y + r.h <= o.y + o.h + e
}

fn overlap(a: Rect, b: Rect) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 0.0 && h > 0.0 { w as f64 * h as f64 } else { 0.0 }
}

/// The tiles directly inside frame `i`.
fn kids(tiles: &[Tile], tree: &Tree, i: usize) -> Vec<usize> {
    let f = tiles[i];
    (i + 1..tiles.len())
        .filter(|&j| {
            let t = tiles[j];
            t.depth == f.depth + 1 && if t.kind == Kind::Rest { t.node == f.node } else { tree.parent[t.node] == f.node }
        })
        .collect()
}

/// A group of sibling tiles fills `region`, stays inside it, does not overlap, and has areas proportional to bytes.
fn check_group(tiles: &[Tile], group: &[usize], region: Rect) {
    assert!(!group.is_empty());
    let sum: f64 = group.iter().map(|&i| area(tiles[i].rect)).sum();
    assert!((sum - area(region)).abs() <= 1.0, "areas {sum} vs region {}", area(region));
    let per_byte = area(tiles[group[0]].rect) / tiles[group[0]].bytes as f64;
    for (n, &i) in group.iter().enumerate() {
        assert!(inside(tiles[i].rect, region), "{:?} outside {region:?}", tiles[i].rect);
        let ratio = area(tiles[i].rect) / tiles[i].bytes as f64;
        assert!((ratio / per_byte - 1.0).abs() < 1e-3, "not proportional: {ratio} vs {per_byte}");
        for &j in &group[n + 1..] {
            assert!(overlap(tiles[i].rect, tiles[j].rect) < 1e-2, "overlap {:?} {:?}", tiles[i].rect, tiles[j].rect);
        }
    }
}

#[test]
fn layout_invariants_hold_at_every_level() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    let top: Vec<usize> = (0..tiles.len()).filter(|&i| tiles[i].depth == 0).collect();
    check_group(&tiles, &top, R);
    let mut frames = 0;
    for i in 0..tiles.len() {
        if tiles[i].kind == Kind::Frame {
            frames += 1;
            let r = tiles[i].rect;
            check_group(&tiles, &kids(&tiles, &tree, i), Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 });
        }
    }
    assert_eq!(frames, 2, "a and nest");
    assert_eq!(tiles.iter().filter(|t| t.kind == Kind::File).count(), 8, "the zero-size file has no tile");
    assert!(tiles.iter().all(|t| t.bytes > 0));
}

#[test]
fn tiles_come_parent_before_child_largest_first_with_classes() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    for (i, t) in tiles.iter().enumerate() {
        if t.depth > 0 {
            let p = tiles[..i].iter().rposition(|p| p.depth + 1 == t.depth).expect("a parent before it");
            assert!(tiles[p].kind == Kind::Frame && inside(t.rect, tiles[p].rect));
        }
    }
    assert_eq!((tiles[0].kind, tree.name_string(tiles[0].node)), (Kind::Frame, "a".to_string()), "a (1100) outweighs c.mp4 (900)");
    let mp4 = tiles.iter().find(|t| tree.name_string(t.node) == "c.mp4").unwrap();
    assert_eq!((mp4.kind, mp4.class, mp4.bytes, mp4.count), (Kind::File, Class::Video, 900, 1));
    let a = tiles[0];
    assert_eq!((a.bytes, a.count, a.class), (1100, 5, Class::Other));
}

#[test]
fn layout_is_deterministic_and_breaks_ties_by_node_id() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let ids: Vec<NodeId> = (0..6).map(|i| file(&mut b, root, &format!("f{i}"), 100)).collect();
    let tree = b.finish().unwrap();
    let (x, y) = (layout(&tree, root, R, 1.0), layout(&tree, root, R, 1.0));
    assert_eq!(x, y);
    assert_eq!(x.iter().map(|t| t.node).collect::<Vec<_>>(), ids);
}

#[test]
fn a_huge_folder_of_tiny_files_becomes_one_rest_tile() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    for _ in 0..200_000 {
        file(&mut b, root, "f", 1);
    }
    let tree = b.finish().unwrap();
    let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    let tiles = layout(&tree, root, r, 1.0);
    assert_eq!(tiles.len(), 1);
    assert_eq!((tiles[0].kind, tiles[0].node, tiles[0].count, tiles[0].bytes, tiles[0].rect), (Kind::Rest, root, 200_000, 200_000, r));
}

#[test]
fn small_siblings_merge_into_a_rest_tile_that_still_tiles_the_folder() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let big = file(&mut b, root, "big.bin", 1_000_000_000);
    for i in 0..3 {
        file(&mut b, root, &format!("s{i}"), 1);
    }
    let tree = b.finish().unwrap();
    let r = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
    let tiles = layout(&tree, root, r, 1.0);
    assert_eq!(tiles.len(), 2);
    assert_eq!((tiles[0].node, tiles[0].kind), (big, Kind::File));
    assert_eq!((tiles[1].kind, tiles[1].count, tiles[1].bytes), (Kind::Rest, 3, 3));
    check_group(&tiles, &[0, 1], r);
}

#[test]
fn a_folder_too_small_to_enter_is_solid() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    file(&mut b, root, "big.bin", 9_950);
    let d = dir(&mut b, root, "small");
    file(&mut b, d, "x", 50);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, 1.0);
    let t = tiles.iter().find(|t| t.node == d).unwrap();
    assert_eq!((t.kind, t.bytes), (Kind::Solid, 50));
    assert_eq!(tiles.len(), 2, "nothing inside it");
}

#[test]
fn nesting_stops_at_the_depth_cap() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    let mut cur = root;
    for i in 0..40 {
        cur = dir(&mut b, cur, &format!("d{i}"));
    }
    file(&mut b, cur, "x.bin", 1_000_000);
    let tree = b.finish().unwrap();
    let tiles = layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 2000.0, h: 2000.0 }, 1.0);
    assert_eq!(tiles.iter().map(|t| t.depth).max(), Some(MAX_DEPTH - 1));
    assert_eq!(tiles.last().map(|t| t.kind), Some(Kind::Solid));
}

#[test]
fn nothing_to_draw_gives_no_tiles_and_a_file_root_gives_one() {
    let mut b = TreeBuilder::new();
    let root = dir(&mut b, NodeId::NONE, "root");
    file(&mut b, root, "z", 0);
    let f = file(&mut b, NodeId::NONE, "alone.mp4", 5);
    let tree = b.finish().unwrap();
    assert!(layout(&tree, root, R, 1.0).is_empty());
    assert!(layout(&tree, NodeId::NONE, R, 1.0).is_empty());
    assert!(layout(&tree, NodeId(99), R, 1.0).is_empty());
    assert!(layout(&tree, root, Rect { x: 0.0, y: 0.0, w: 0.0, h: 10.0 }, 1.0).is_empty());
    let t = layout(&tree, f, R, 1.0);
    assert_eq!((t.len(), t[0].kind, t[0].class, t[0].rect), (1, Kind::File, Class::Video, R));
}

#[test]
fn hit_finds_the_deepest_tile_and_none_outside() {
    let (tree, root) = sample();
    let tiles = layout(&tree, root, R, 1.0);
    let c = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
    for (i, t) in tiles.iter().enumerate().filter(|(_, t)| t.kind == Kind::File) {
        let (x, y) = c(t.rect);
        assert_eq!(hit(&tiles, x, y), Some(i));
    }
    let a = tiles[0].rect;
    assert_eq!(hit(&tiles, a.x + 0.5, a.y + 0.5), Some(0), "the frame's own border belongs to the frame");
    assert_eq!(hit(&tiles, 5.0, 5.0), None);
    assert_eq!(hit(&[], 1.0, 1.0), None);
}

#[test]
fn the_map_root_is_the_selected_folder_else_its_parent_else_the_first_root() {
    let (tree, root) = sample();
    let find = |n: &str| (0..tree.len() as u32).map(NodeId).find(|&i| tree.name_string(i) == n).unwrap();
    assert_eq!(root_for(&tree, None), Some(root));
    assert_eq!(root_for(&tree, Some(find("a"))), Some(find("a")));
    assert_eq!(root_for(&tree, Some(find("a1.rs"))), Some(find("a")));
    assert_eq!(root_for(&tree, Some(find("e.txt"))), Some(root));
    assert_eq!(root_for(&tree, Some(NodeId(999))), Some(root), "a stale selection means the root");
}
