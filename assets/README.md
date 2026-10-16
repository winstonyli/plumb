# App icon

`plumb.svg` is the source (256 x 256, hand-built; nothing generates it). `plumb.ico` holds PNG entries at 16, 24, 32, 48 and 256 px, each rendered from the SVG at its own size (headless Chrome, transparent background), not resampled from the 256. `crates/plumb/icon.rc` embeds it as resource id 1 and the window class loads it (`window.rs`, `create_window`).

To change it: edit the SVG, re-render the five sizes, repack the `.ico`, rebuild.

## Design

- Unit grid u = 8: tile gap = visible outline = 10, margin 2u, string width u. The outer corner radius (40) is concentric with the icon border (56); interior tile corners are square.
- A treemap of four tiles (red 17u wide on the left; blue, green, purple stacked on the right, blue 12u tall) in plumb's own map class colours (`crates/plumb/src/draw.rs`: `#d0605e`, `#5a8fd6`, `#4fae72`, `#8f6fd0`). Border and gap colour `#11151d`; a faint white-to-black overlay (10%) over the tiles.
- A turned-brass ("acorn") plumb bob on a cream string (`#f7e6b4`), body and chamfered cap in one shape with one shared outline. Shading is a warm horizontal gradient with tip darkening, a left crescent highlight, a right rim light, a glint and a dark inner edge.
- Reference: real bobs are turned brass with round shoulders and a long taper (Wikimedia Commons).

## Small sizes

A hand-simplified 16 px cut (flat two-tone bob, 1 px outline) was mocked on 2026-10-16 and rejected: its pointed triangle reads as a pen nib. The full art, rendered at 16 px, keeps the acorn silhouette and is used at every size.
