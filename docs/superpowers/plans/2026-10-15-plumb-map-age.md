# Map age shading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Treemap file tiles keep their type hue and fade toward the map background with the file's last-write age (continuous, log scale), fed by a per-node modified day stored in the tree.

**Architecture:** `Tree.mtime: Vec<Option<NonZeroU32>>` (days since 1601) is filled by the walker, the MFT parse (`$STANDARD_INFORMATION`), the preview and the live paths through a `set_mtime` setter. A pure `plumb_core::age` module owns the day conversion and the shade function. `Tile` carries the file's day; `Pen::tile` mixes the class colour toward `Theme.bg` using `now` captured with the tile generation.

**Tech Stack:** Rust 2024 workspace (`plumb-core`, `plumb-scan`, `plumb`), Direct2D, WARP render tests.

**Spec:** `docs/superpowers/specs/2026-10-15-plumb-map-age-design.md`

## Global Constraints

- `Option<NonZeroU32>` days since 1601-01-01 for every stored time; `None` = unknown = plain class colour. A FILETIME under one day maps to `None`.
- `push`, `push_str`, `add_child` keep their signatures; times arrive through `set_mtime`.
- No timer redraws: "now" is captured when tiles are rebuilt.
- Constants: `AGE_FULL_DAYS = 3650`, `FADE_MAX = 0.5` (in `plumb_core::age`).
- Build with `-j 8`; run `cargo clippy --workspace --all-targets` clean before each commit; commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- Python must never read stdin (write a file first, run `python -P f.py < /dev/null`), or avoid python.

---

### Task 1: Core: `age` module, `Tree.mtime`, `Tile.mtime`

**Files:**
- Create: `crates/plumb-core/src/age.rs`
- Modify: `crates/plumb-core/src/lib.rs` (add `pub mod age;`), `crates/plumb-core/src/tree.rs`, `crates/plumb-core/src/treemap.rs`
- Test: `crates/plumb-core/src/age.rs` (inline `#[cfg(test)]`), `crates/plumb-core/src/tree/mutation_tests.rs`, `crates/plumb-core/src/treemap/tests.rs`

**Interfaces:**
- Produces (used by every later task):
  - `plumb_core::age::{DAY_TICKS: u64, AGE_FULL_DAYS: f32, FADE_MAX: f32}`
  - `age::day_of(filetime: u64) -> Option<NonZeroU32>`
  - `age::day_from_unix(secs: u64) -> u32`
  - `age::shade(now: u32, mtime: Option<NonZeroU32>) -> f32` (0.0 newest or unknown, 1.0 oldest)
  - `Tree.mtime: Vec<Option<NonZeroU32>>` (pub), `Tree::set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>)`, `TreeBuilder::set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>)`
  - `Tile.mtime: Option<NonZeroU32>` (the file's day for `Kind::File`, else `None`)

- [ ] **Step 1: Write the failing tests** in the new `age.rs`:

```rust
//! Last-write time as a whole day, and how faded a tile of that age is drawn.

use std::num::NonZeroU32;

/// 100 ns FILETIME ticks in a day.
pub const DAY_TICKS: u64 = 864_000_000_000;
/// Days from 1601-01-01 to 1970-01-01.
const UNIX_EPOCH_DAYS: u32 = 134_774;
/// Age (days) at which the fade is complete.
pub const AGE_FULL_DAYS: f32 = 3650.0;
/// How far toward the background the oldest tile is mixed (0 = not at all, 1 = vanishes).
pub const FADE_MAX: f32 = 0.5;

/// The day a FILETIME falls on, or `None` if it is under one day (0 means "unset").
pub fn day_of(filetime: u64) -> Option<NonZeroU32> {
    NonZeroU32::new(u32::try_from(filetime / DAY_TICKS).ok()?)
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
        assert_eq!(day_of(u64::MAX), None); // past what a u32 of days holds
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
        let (week, year, decade, ancient) = (shade(now, d(now - 7)), shade(now, d(now - 365)), shade(now, d(now - 3650)), shade(now, d(100)));
        assert!(0.0 < week && week < year && year < decade);
        assert!((decade - 1.0).abs() < 1e-6);
        assert_eq!(ancient, 1.0);
    }
}
```

Add `pub mod age;` to `lib.rs` next to the other `pub mod` lines.

Add to `tree/mutation_tests.rs` (adapt the helper names there; build a 2-node tree with the file's `TreeBuilder`):

```rust
#[test]
fn mtime_starts_unknown_is_set_and_survives_add_child() {
    let mut b = TreeBuilder::new();
    let root = b.push_str(NodeId::NONE, "r", 0, NodeFlags::DIR);
    let f = b.push_str(root, "f", 5, NodeFlags::empty());
    b.set_mtime(f, NonZeroU32::new(9));
    let mut t = b.finish().unwrap();
    assert_eq!((t.mtime[root], t.mtime[f]), (None, NonZeroU32::new(9)));
    let g = t.add_child(root, &[b'g' as u16], 1, NodeFlags::empty());
    assert_eq!(t.mtime[g], None);
    t.set_mtime(g, NonZeroU32::new(3));
    assert_eq!(t.mtime[g], NonZeroU32::new(3));
}
```

In `treemap/tests.rs`, extend an existing file-tile test (or add) so that a tree whose file has `set_mtime(f, Some(7))` yields a `Kind::File` tile with `mtime == NonZeroU32::new(7)` and every non-File tile has `mtime == None`.

- [ ] **Step 2: Run to verify failure.** `cargo test -p plumb-core -j 8` → compile errors (`age`, `set_mtime`, `Tile.mtime` missing).

- [ ] **Step 3: Implement.**
  - `tree.rs`: add `use std::num::NonZeroU32;`; add `pub mtime: Vec<Option<NonZeroU32>>,` to `Tree` (after `flags`) and to `TreeBuilder` (private); `TreeBuilder::with_capacity` reserves it; `TreeBuilder::push` pushes `None`; `finish` moves it into `Tree { mtime: self.mtime, .. }`; `Tree::add_child` pushes `None`; `Tree::reserve` reserves it. Add:

```rust
    pub fn set_mtime(&mut self, id: NodeId, m: Option<NonZeroU32>) {
        self.mtime[id] = m;
    }
```
  to both `Tree` and `TreeBuilder` (`self.mtime[id.idx()]` if `Vec` indexing by `NodeId` is not implemented for `Option<NonZeroU32>`; `NodeId` already indexes `Vec<T>` in this file, so `self.mtime[id]` works). Fix any other `Tree { .. }` literal the compiler reports by adding `mtime: vec![None; n]` (or `Vec::new()` for an empty tree).
  - `treemap.rs`: add `pub mtime: Option<NonZeroU32>,` to `Tile` with doc "the file's last-write day (`Kind::File` only)". In `fill` (the `Kind::Rest` push) set `mtime: None`. In `emit`, the `tile` closure gets `mtime: None`, and the file branch becomes `out.push(Tile { mtime: t.mtime[c], ..tile(Kind::File, classify(t.name(c)), 1) });`. Fix the Tile literals in `treemap/tests.rs` and `crates/plumb/src/app.rs:1392` (add `mtime: None`).

- [ ] **Step 4: Run.** `cargo test --workspace -j 8` → PASS; `cargo clippy --workspace --all-targets -j 8` clean.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Core: Tree.mtime, Tile.mtime and the age module"` (with the trailer).

---

### Task 2: MFT parse reads `$STANDARD_INFORMATION`

**Files:**
- Modify: `crates/plumb-core/src/mft.rs`, `crates/plumb-core/src/synth.rs`
- Test: `crates/plumb-core/src/mft.rs` (existing `#[cfg(test)]` module)

**Interfaces:**
- Consumes: `age::day_of`.
- Produces: `ParsedRecord.mtime: Option<NonZeroU32>` (base record only; `merge_extension` leaves the base's), `RecordBuilder::standard_info(self, modified_filetime: u64) -> Self`.

- [ ] **Step 1: Failing test** in `mft.rs` tests:

```rust
    #[test]
    fn standard_information_gives_the_last_write_day() {
        let ft = 5 * crate::age::DAY_TICKS + 123;
        let mut buf = RecordBuilder::file().standard_info(ft).file_name(5, "a.txt", 1).data_resident(10).build();
        let mut out = ParsedRecord::default();
        parse_record(&mut buf, &mut out).unwrap();
        assert_eq!(out.mtime, NonZeroU32::new(5));
        let mut buf = RecordBuilder::file().file_name(5, "b", 1).build();
        parse_record(&mut buf, &mut out).unwrap();
        assert_eq!(out.mtime, None); // `clear` resets it and a record without the attribute stays unknown
    }
```

- [ ] **Step 2: Run** `cargo test -p plumb-core mft -j 8` → FAIL (no `standard_info`, no `mtime`).

- [ ] **Step 3: Implement.** `synth.rs`, in `impl RecordBuilder`:

```rust
    /// A `$STANDARD_INFORMATION` attribute whose last-write time (value offset 0x08) is `modified`.
    pub fn standard_info(mut self, modified: u64) -> Self {
        let mut v = vec![0u8; 0x30];
        put64(&mut v, 0x08, modified);
        self.attrs.push(resident_attr(0x10, "", &v));
        self
    }
```
`mft.rs`: `use std::num::NonZeroU32;`, `const ATTR_STANDARD_INFORMATION: u32 = 0x10;`, field `pub mtime: Option<NonZeroU32>,` on `ParsedRecord` (doc: "Last-write day from `$STANDARD_INFORMATION` (base records only)"), `self.mtime = None;` in `clear`, dispatch `ATTR_STANDARD_INFORMATION => parse_standard_information(a, out)?,` and:

```rust
fn parse_standard_information(a: &[u8], out: &mut ParsedRecord) -> Result<(), ParseError> {
    if a[8] != 0 {
        return Ok(()); // always resident; ignore anything else
    }
    let v = resident_value(a)?;
    out.mtime = crate::age::day_of(u64_at(v, 0x08)?);
    Ok(())
}
```

- [ ] **Step 4: Run** `cargo test --workspace -j 8` → PASS (existing record tests unaffected: they have no attribute 0x10).

- [ ] **Step 5: Commit** "Core: MFT parse reads the last-write day".

---

### Task 3: Scanners carry the day (walker, merger, FileState, preview message)

**Files:**
- Modify: `crates/plumb-scan/src/listing.rs`, `crates/plumb-scan/src/walk.rs`, `crates/plumb-scan/src/preview.rs`, `crates/plumb-scan/src/merge.rs`, `crates/plumb-scan/src/journal/state.rs`; fix literals in `crates/plumb-scan/src/journal/tests.rs`, `journal/usn.rs`, and `mft_scan.rs` as the compiler reports.
- Test: `crates/plumb-scan/src/merge.rs` tests, `crates/plumb-scan/src/walk/tests.rs`, `crates/plumb-scan/src/journal/state.rs` tests.

**Interfaces:**
- Consumes: `age::day_of`, `ParsedRecord.mtime`, `TreeBuilder::set_mtime`.
- Produces: `Entry.mtime: Option<NonZeroU32>`, `FileState.mtime: Option<NonZeroU32>`, `PreviewMsg::Add { parent, name, size, flags, mtime }`.

- [ ] **Step 1: Failing tests.**
  - `merge.rs` tests: using `RecordBuilder` with `.standard_info(9 * DAY_TICKS)` on a file with two hard links (two `.file_name` calls under different parents), merge, and assert both nodes have `tree.mtime == NonZeroU32::new(9)` and the root/dirs without the attribute have `None`. Mirror an existing merge test for the setup (root record 5 plus a dir record).
  - `journal/state.rs` tests: `file_state(7, &rec)` with `rec.mtime = NonZeroU32::new(4)` returns `FileState { mtime: NonZeroU32::new(4), .. }`.
  - `walk/tests.rs`: after a scan of a temp dir containing one file just written (look at an existing test for the pattern), `tree.mtime[file]` is `Some(d)` with `d` within one day of `age::day_from_unix(now)`.

- [ ] **Step 2: Run** `cargo test -p plumb-scan -j 8` → FAIL.

- [ ] **Step 3: Implement.**
  - `listing.rs`: add `pub mtime: Option<NonZeroU32>,` to `Entry` (doc: "Last-write day; `None` if unknown") and `use std::num::NonZeroU32;`.
  - `walk.rs` `list_dir`: after `let id = ...;` add `let mtime = plumb_core::age::day_of(e.LastWriteTime as u64);` (`LastWriteTime` is an `i64`; negative → huge → `None` via `u32::try_from`) and pass `mtime` in the `Entry { .. }` literal. In the owning loop: `let id = out.push(done.id, &e.name, size, flags); out.set_mtime(id, e.mtime);` and add `mtime: e.mtime` to the `PreviewMsg::Add` literal (two places: the root `Add` gets `mtime: None`).
  - `preview.rs`: `Add { parent: NodeId, name: Vec<u16>, size: u64, flags: NodeFlags, mtime: Option<NonZeroU32> }`.
  - `journal/state.rs`: `FileState` gains `pub mtime: Option<NonZeroU32>,` (doc: "Last-write day"); `file_state` sets `mtime: rec.mtime`.
  - `merge.rs`: `MftMerger` gains `mtime: Vec<Option<NonZeroU32>>` initialised `vec![None; record_count]`; in `add`, inside the `if rec.base.is_none() { ... }` block that sets `F_IN_USE`, add `self.mtime[o] = rec.mtime;`; in the push loop, replace `out.push(parent, self.name(..), size, flags);` with `let id = out.push(parent, self.name(&self.names[ni as usize]), size, flags); out.set_mtime(id, self.mtime[o]);`.
  - Keep the workspace compiling: fix every `Entry { .. }`, `FileState { .. }` and `PreviewMsg::Add { .. }` literal the compiler reports with `mtime: None` (about 13 sites: `journal/tests.rs`, `journal/usn.rs`, `plumb/src/dirsync/tests.rs`, `plumb/src/live/tests.rs`, `plumb/src/preview/tests.rs`), and in `plumb/src/preview.rs` make the `Add` arm `PreviewMsg::Add { parent, name, size, flags, mtime: _ }` (Task 4 uses it). The new behaviour in the `plumb` crate is Task 4.

- [ ] **Step 4: Run** `cargo test --workspace -j 8` → PASS (the `plumb` crate compiles and its tests pass with `mtime: None` literals).

- [ ] **Step 5: Parity check** (the spec's scan-compare extension). In `crates/plumb-scan/examples/scan-compare.rs`, after both scans, compare `walk_tree.mtime` and `mft_tree.mtime` per matched path and print the number that differ by more than 1 day (an MFT time can lag a directory-listing time slightly for files being written). Run it elevated on a quiet directory and record the result in the commit message.

- [ ] **Step 6: Commit** "Scan: walker, merger and FileState carry the last-write day".

---

### Task 4: `plumb`: preview, live and dirsync keep the day current

**Files:**
- Modify: `crates/plumb/src/preview.rs`, `crates/plumb/src/live.rs`, `crates/plumb/src/dirsync.rs`.
- Test: the three `tests.rs` files.

**Interfaces:** Consumes `Tree::set_mtime`, `Entry.mtime`, `FileState.mtime`, `PreviewMsg::Add.mtime`.

- [ ] **Step 1: Failing tests.**
  - `preview/tests.rs`: an `Add` with `mtime: NonZeroU32::new(8)` gives `tree.mtime[child] == NonZeroU32::new(8)`; an `Upsert` of a `FileState` with `mtime` does the same, including a child that was parked and placed later.
  - `live/tests.rs`: a `FileState` whose size is unchanged but `mtime` differs updates `tree.mtime[id]` and returns a nonzero change count; a new record gets its `mtime`.
  - `dirsync/tests.rs`: same for an `Entry` with unchanged size but a new `mtime`; a new entry gets its `mtime`.

- [ ] **Step 2: Run** `cargo test -p plumb -j 8` → FAIL.

- [ ] **Step 3: Implement.**
  - `preview.rs` `Add` arm: destructure `mtime`, `let id = t.add_child(parent, &name, size, flags); t.set_mtime(id, mtime);`. In the placement (`let id = t.add_child(parent, &s.name, size, flags);`, ~line 197) add `t.set_mtime(id, s.mtime);`. The root-creating branch needs no change.
  - `live.rs`: in the existing-node branch, after the size check add

```rust
                if tree.mtime[id] != s.mtime {
                    tree.set_mtime(id, s.mtime);
                    self.changes += 1;
                }
```
  and after `self.records[record] = tree.add_child(...)` use `let id = tree.add_child(parent, &s.name, size, flags); tree.set_mtime(id, s.mtime); self.records[record] = id;`. The synthetic `<orphans>` add at line ~168 stays `None`.
  - `dirsync.rs`: in the `Some(id) if tree.flags[id] & mask == want` arm add the same compare with `e.mtime` (`changes += 1`); in both `add_child` calls (`Some(id)` kind-changed arm and `None` arm) set `e.mtime` on the new id.

- [ ] **Step 4: Run** `cargo test --workspace -j 8` and `cargo run -p plumb -- --selftest` → PASS; clippy clean.

- [ ] **Step 5: Commit** "Preview, live and directory sync keep the last-write day".

---

### Task 5: Rendering: shade, "now", tooltip date, age strip

**Files:**
- Modify: `crates/plumb/src/draw.rs`, `crates/plumb/src/app.rs`, `crates/plumb/src/fmt.rs`, `crates/plumb/src/window.rs` (where `MapView` is built, if not in `app.rs`)
- Test: `crates/plumb/src/fmt.rs` (inline), `crates/plumb/tests/render.rs`, `crates/plumb/src/app.rs` tests

**Interfaces:**
- Consumes: `age::{shade, day_from_unix, FADE_MAX}`, `Tile.mtime`, `Tree.mtime`.
- Produces: `fmt::date(day: u32) -> String` (`2024-03-09`), `fmt::age(now: u32, day: u32) -> String` (`2.1 y`, `3 mo`, `5 d`, `today`), `MapView.now: u32`.

- [ ] **Step 1: Failing tests.**
  - `fmt.rs`:

```rust
    #[test]
    fn dates_and_ages() {
        assert_eq!(date(134_774), "1970-01-01");
        assert_eq!(date(134_774 + 19_791), "2024-03-09");
        assert_eq!(age(100, 100), "today");
        assert_eq!(age(105, 100), "5 d");
        assert_eq!(age(100 + 365 * 2 + 36, 100), "2.1 y"); // 766 days
        assert_eq!(age(100 + 90, 100), "3 mo");
    }
```
  (Check `19_791`: 2024-03-09 is 19_791 days after 1970-01-01. Verify with `date` in the red run and fix the constant if the arithmetic is off; the test must pin the algorithm, not the typo.)
  - `app.rs` tests: `tile_tip` of a File tile whose node has `mtime` ends with `, modified 2024-03-09 (2.1 y ago)` computed from a fixed `now`; with `None` it has no `modified` text. (`tile_tip` gains a `now: u32` parameter.)
  - `tests/render.rs`: (a) render a map of two files of one class, one `mtime = Some(now)` and one `Some(now - 3650)`; the two tiles' interior pixels differ and the older is nearer to `Theme.bg`; (b) with `mtime = None` the pixel equals the class colour exactly; (c) for both palettes, the 11 class colours mixed by `FADE_MAX` toward `bg` are pairwise at least 24 apart in max-channel distance (compute from `Painter`'s theme; adjust `FADE_MAX`'s doc/value if two classes collide and record the finding); (d) the legend draws an age strip when wide and omits it when narrow, in the manner of the existing legend width tests.

- [ ] **Step 2: Run** `cargo test -p plumb -j 8` → FAIL.

- [ ] **Step 3: Implement.**
  - `fmt.rs` (civil-from-days, Howard Hinnant):

```rust
/// `YYYY-MM-DD` for a day count since 1601-01-01.
pub fn date(day: u32) -> String {
    let z = i64::from(day) - 134_774 + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How long before `now` a day was: "today", "5 d", "3 mo", "2.1 y".
pub fn age(now: u32, day: u32) -> String {
    let d = now.saturating_sub(day);
    match d {
        0 => "today".into(),
        1..=59 => format!("{d} d"),
        60..=729 => format!("{} mo", (d + 15) / 30),
        _ => format!("{:.1} y", d as f32 / 365.0),
    }
}
```
  (With these rules 766 days gives "2.1 y" and 90 days gives "3 mo"; keep the test and rules consistent.)
  - `draw.rs`: add `pub now: u32` to `MapView` (doc: "Today as a day count; captured with the tiles"). Change `Pen::tile(&self, tile, t, now)`; in the `Kind::File` arm:

```rust
                let c = t.class[tile.class.index()];
                let k = plumb_core::age::shade(now, tile.mtime) * plumb_core::age::FADE_MAX;
                let c = D2D1_COLOR_F { r: c.r + (t.bg.r - c.r) * k, g: c.g + (t.bg.g - c.g) * k, b: c.b + (t.bg.b - c.b) * k, a: c.a };
```
  The rest of the arm (edge fill at 0.6, interior fill) uses this faded `c`. `refresh_map_cache` passes `m.now` and adds nothing to the key (the generation covers it, see below).
  - Legend strip: in `Painter::legend`, after computing `n` and `used` (before drawing), define `const STRIP: f32 = 48.0; const STRIP_GAP: f32 = 12.0;` and:

```rust
        let strip = used + STRIP_GAP + STRIP <= max;
        let right = r.x + r.w;
        if strip {
            let x0 = right - STRIP;
            for i in 0..8 {
                let k = i as f32 / 7.0 * plumb_core::age::FADE_MAX;
                let c = s.theme.fg;
                let b = s.theme.bar; // the bar's own background, which the strip sits on
                self.fill(
                    Rect { x: x0 + i as f32 * 6.0, y: r.y + (r.h - 10.0) / 2.0, w: 6.0, h: 10.0 },
                    D2D1_COLOR_F { r: c.r + (b.r - c.r) * k, g: c.g + (b.g - c.g) * k, b: c.b + (b.b - c.b) * k, a: c.a },
                );
            }
        }
        let shift = if strip { STRIP + STRIP_GAP } else { 0.0 };
```
  then draw the type entries starting at `x = right - shift - used`, and return `used + shift`. (Labels "new"/"old" are omitted from the strip itself; the tooltip over the status bar gains the line `Tiles fade with age: bright = recently modified.` — add it to `App::tip_at`'s status-bar text.) Confirm `Theme.bar` is the status bar's background by reading its use at `draw.rs:477`; if it is not, use `s.theme.panel`.
  - `app.rs`: add `map_now: u32` to `App`; set it where `map_tiles` is rebuilt (the same place `map_classes` is set, ~line 628) from `age::day_from_unix(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))`, and bump `map_gen` as it already does. `MapView { now: self.map_now, .. }` in `with_scene`. `tile_tip(tree, t, now)`: for non-`Rest` tiles append `, modified {date} ({age} ago)` when `tree.mtime[t.node]` is `Some`; folders show their own time too.

- [ ] **Step 4: Run** `cargo test --workspace -j 8`, `cargo run -p plumb -- --selftest`, clippy → all green. Render outputs: set `PLUMB_SHOT_DIR` to a scratch dir and look at one shot of a map with mixed ages (read the PNG) to confirm the fade is visible and the hues are still distinct; keep the PNG out of the repo.

- [ ] **Step 5: Commit** "Map: tiles fade with the file's age; tooltip shows the date; legend gets an age strip".

---

### Task 6: Measure, tune on a real drive, document

**Files:**
- Modify: `CHANGELOG.md`, `docs/superpowers/specs/2026-10-15-plumb-map-age-design.md`, `docs/superpowers/specs/2026-10-14-plumb-treemap-design.md`, `docs/benchmarks.md`

- [ ] **Step 1: Memory and time.** Elevated, on `C:` with the release build in a separate target dir (no other cargo running; check `Get-Process` for orphans): run `crates/plumb-scan/examples/walk.rs` and the MFT scan example before and after (`git stash` is not needed: build the parent commit of Task 1 in a throwaway worktree) and record peak working set and scan time for both. Expect tree +4 bytes a node and a merger peak of +4 bytes a record; a scan-time change over 5% is investigated before going on. Record numbers in `docs/benchmarks.md` with machine load and Defender state.
- [ ] **Step 2: Real-drive look (needs the user).** Launch the release build on a real drive and ask the user to judge `AGE_FULL_DAYS`, `FADE_MAX` and the strip at 100% and 200% DPI. Adjust the two constants if asked; the render test in Task 5 step 1(c) must still pass.
- [ ] **Step 3: Docs.** CHANGELOG `[Unreleased]` → Added: "`plumb`: map tiles fade with the file's last-write age (log scale, ten years to the full fade) while keeping their type hue; the tooltip shows the modified date; the legend gets an age strip. `plumb-core`: `Tree.mtime`, `age`; `plumb-scan`: the walker and MFT scanner record last-write times (`Entry.mtime`, `FileState.mtime`)." Set the age spec's status to "implemented" with the measured numbers under Risks; in the treemap spec change "(not yet built)" to "(built)".
- [ ] **Step 4: Verify and commit.** `cargo test --workspace -j 8`, `cargo run -p plumb -- --selftest`, `--selftest-watch`, clippy. Commit "Docs: map age shading measured and documented".
