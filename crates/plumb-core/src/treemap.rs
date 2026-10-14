//! Squarified treemap of a folder's subtree: pure layout, colour classes and hit testing.
//! No UI types, so it tests anywhere; `plumb` paints the tiles.

use crate::node::{NodeFlags, NodeId};
use crate::rect::Rect;
use crate::tree::Tree;

/// What kind of file a tile stands for, by extension. Colours are chosen by the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Video,
    Audio,
    Image,
    Archive,
    Document,
    Code,
    Executable,
    DiskImage,
    /// Databases and logs.
    Database,
    Other,
}

impl Class {
    pub const ALL: [Class; 10] = [
        Class::Video, Class::Audio, Class::Image, Class::Archive, Class::Document,
        Class::Code, Class::Executable, Class::DiskImage, Class::Database, Class::Other,
    ];

    /// Position in `ALL` (and in a theme's colour table).
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        ["Video", "Audio", "Images", "Archives", "Documents", "Code", "Programs", "Disk images", "Databases and logs", "Other"][self.index()]
    }
}

/// The class of a file named `name` (UTF-16, as the tree stores it): by the text after the last dot,
/// ASCII case-insensitively. No extension, an empty one or a non-ASCII one is `Other`.
pub fn classify(name: &[u16]) -> Class {
    let Some(dot) = name.iter().rposition(|&u| u == u16::from(b'.')) else { return Class::Other };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 8 {
        return Class::Other;
    }
    let mut buf = [0u8; 8];
    for (b, &u) in buf.iter_mut().zip(ext) {
        if u >= 0x80 {
            return Class::Other;
        }
        *b = (u as u8).to_ascii_lowercase();
    }
    match std::str::from_utf8(&buf[..ext.len()]).unwrap_or("") {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "m2ts" => Class::Video,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "wma" | "opus" => Class::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "avif" | "raw" | "cr2" | "nef" | "psd" | "svg" => Class::Image,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" => Class::Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "csv" | "epub" => Class::Document,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "js" | "ts" | "jsx" | "tsx" | "py" | "java" | "go" | "rb" | "php" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "xml" | "sh" | "ps1" | "bat" => Class::Code,
        "exe" | "dll" | "sys" | "msi" => Class::Executable,
        "vhd" | "vhdx" | "iso" | "vmdk" | "wim" => Class::DiskImage,
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "mdf" | "ldf" | "log" | "etl" => Class::Database,
        _ => Class::Other,
    }
}

/// Deepest nesting of folder frames; the tree can be deeper (a stack guard).
pub const MAX_DEPTH: u8 = 24;
/// A child with less area than this many physical pixels is merged into its folder's `Rest` tile.
const MIN_PX2: f64 = 9.0;
/// A folder whose inside (after its frame) has less area than this is drawn `Solid`, not entered.
const SOLID_PX2: f64 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    /// A folder with its children laid out inside.
    Frame,
    /// A folder too small to enter.
    Solid,
    /// The merged small items of one folder.
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// For `Rest`, the folder it belongs to.
    pub node: NodeId,
    pub rect: Rect,
    /// Nesting level: the root's children are 0.
    pub depth: u8,
    pub class: Class,
    pub kind: Kind,
    pub bytes: u64,
    /// Files in the subtree for a folder, 1 for a file, merged items for `Rest`.
    pub count: u32,
}

/// Edges in f64, so tiles that share an edge share the very same number.
#[derive(Clone, Copy)]
struct Box64 {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Box64 {
    fn of(r: Rect) -> Self {
        Self { x0: r.x as f64, y0: r.y as f64, x1: (r.x + r.w) as f64, y1: (r.y + r.h) as f64 }
    }

    fn w(&self) -> f64 {
        self.x1 - self.x0
    }

    fn h(&self) -> f64 {
        self.y1 - self.y0
    }

    fn area(&self) -> f64 {
        self.w() * self.h()
    }

    fn rect(&self) -> Rect {
        Rect { x: self.x0 as f32, y: self.y0 as f32, w: (self.x1 - self.x0) as f32, h: (self.y1 - self.y0) as f32 }
    }
}

/// The tiles of `root`'s descendants filling `rect`, parent before child; `px` is the size of one
/// physical pixel in the caller's units (96 / dpi for DIPs). See the spec for the rules.
pub fn layout(tree: &Tree, root: NodeId, rect: Rect, px: f32) -> Vec<Tile> {
    let mut out = Vec::new();
    if root.is_none() || root.idx() >= tree.len() || tree.flags[root].contains(NodeFlags::DEAD) {
        return out;
    }
    let area = Box64::of(rect);
    if !(area.w() > 0.0 && area.h() > 0.0) {
        return out;
    }
    let ctx = Ctx { tree, px: px as f64 };
    if tree.flags[root].contains(NodeFlags::DIR) {
        ctx.fill(root, area, 0, &mut out);
    } else if tree.size[root] > 0 {
        ctx.emit(root, area, 0, &mut out);
    }
    out
}

struct Ctx<'a> {
    tree: &'a Tree,
    px: f64,
}

impl Ctx<'_> {
    /// Lays out the children of `dir` in `area`.
    fn fill(&self, dir: NodeId, area: Box64, depth: u8, out: &mut Vec<Tile>) {
        let t = self.tree;
        let total = t.subtree_size[dir].saturating_sub(t.size[dir]);
        let a = area.area();
        if total == 0 || a.is_nan() || a <= 0.0 {
            return;
        }
        // One O(k) pass: the children big enough to get MIN_PX2 are kept, the rest are summed.
        let min_area = MIN_PX2 * self.px * self.px;
        let mut kept: Vec<(NodeId, u64)> = Vec::new();
        let (mut rest_bytes, mut rest_n) = (0u64, 0u32);
        for c in t.children(dir) {
            let w = t.subtree_size[c];
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
                out.push(Tile { node: dir, rect: b.rect(), depth, class: Class::Other, kind: Kind::Rest, bytes: w, count: rest_n });
            } else {
                self.emit(c, b, depth, out);
            }
        }
    }

    /// The tile for `c` in `b`, and for a folder big enough, everything inside it.
    fn emit(&self, c: NodeId, b: Box64, depth: u8, out: &mut Vec<Tile>) {
        let t = self.tree;
        let tile = |kind, class, count| Tile { node: c, rect: b.rect(), depth, class, kind, bytes: t.subtree_size[c], count };
        if !t.flags[c].contains(NodeFlags::DIR) {
            out.push(tile(Kind::File, classify(t.name(c)), 1));
            return;
        }
        let inset = self.px;
        let inner = Box64 { x0: b.x0 + inset, y0: b.y0 + inset, x1: b.x1 - inset, y1: b.y1 - inset };
        let enter = depth + 1 < MAX_DEPTH && inner.w() > 0.0 && inner.h() > 0.0 && inner.area() >= SOLID_PX2 * self.px * self.px;
        if enter {
            out.push(tile(Kind::Frame, Class::Other, t.subtree_files[c]));
            self.fill(c, inner, depth + 1, out);
        } else {
            out.push(tile(Kind::Solid, Class::Other, t.subtree_files[c]));
        }
    }
}

/// Worst aspect ratio of a row of total area `s` (largest item `hi`, smallest `lo`) laid along `side`.
fn worst(s: f64, lo: f64, hi: f64, side: f64) -> f64 {
    ((side * side * hi) / (s * s)).max((s * s) / (side * side * lo))
}

/// Boxes tiling `area`, one per weight in order, with areas proportional to the weights (Bruls,
/// Huizing, van Wijk). The weights are positive and `area` has positive width and height.
fn squarify(weights: &[f64], area: Box64) -> Vec<Box64> {
    let n = weights.len();
    let scale = area.area() / weights.iter().sum::<f64>();
    let mut out = Vec::with_capacity(n);
    let mut rest = area;
    let mut i = 0;
    while i < n {
        // A wide remainder gets a column on its left edge, a tall one a strip along its top edge.
        let wide = rest.w() >= rest.h();
        let side = rest.w().min(rest.h());
        let first = weights[i] * scale;
        let (mut sum, mut lo, mut hi) = (first, first, first);
        let mut cur = worst(sum, lo, hi, side);
        let mut j = i + 1;
        while j < n {
            let a = weights[j] * scale;
            let (s2, lo2, hi2) = (sum + a, lo.min(a), hi.max(a));
            let w2 = worst(s2, lo2, hi2, side);
            if w2 > cur {
                break;
            }
            (sum, lo, hi, cur) = (s2, lo2, hi2, w2);
            j += 1;
        }
        let (near, far) = if wide { (rest.x0, rest.x1) } else { (rest.y0, rest.y1) };
        let edge = if j == n { far } else { (near + sum / side).min(far) };
        let (o0, o1) = if wide { (rest.y0, rest.y1) } else { (rest.x0, rest.x1) };
        let (mut prev, mut acc) = (o0, 0.0);
        for (k, w) in weights.iter().enumerate().take(j).skip(i) {
            acc += w * scale;
            let next = if k + 1 == j { o1 } else { o0 + (o1 - o0) * (acc / sum) };
            out.push(if wide {
                Box64 { x0: near, x1: edge, y0: prev, y1: next }
            } else {
                Box64 { x0: prev, x1: next, y0: near, y1: edge }
            });
            prev = next;
        }
        if wide {
            rest.x0 = edge;
        } else {
            rest.y0 = edge;
        }
        i = j;
    }
    out
}

/// The deepest tile containing (x, y): tiles are stored parent before child, so the last match is it.
pub fn hit(tiles: &[Tile], x: f32, y: f32) -> Option<usize> {
    tiles.iter().rposition(|t| t.rect.contains(x, y))
}

/// The folder the map shows for a selection: the selected folder, else a file's parent, else the
/// tree's first root (no selection, or one that no longer exists).
pub fn root_for(tree: &Tree, selected: Option<NodeId>) -> Option<NodeId> {
    let alive = |n: NodeId| !n.is_none() && n.idx() < tree.len() && !tree.flags[n].contains(NodeFlags::DEAD);
    match selected.filter(|&s| alive(s)) {
        Some(s) if tree.flags[s].contains(NodeFlags::DIR) => Some(s),
        Some(s) => Some(if tree.parent[s].is_none() { s } else { tree.parent[s] }),
        None => tree.roots.first().copied(),
    }
}

#[cfg(test)]
mod tests;
