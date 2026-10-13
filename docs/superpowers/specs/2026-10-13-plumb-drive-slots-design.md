# Per-drive slots (design)

Status: implemented 2026-10-13.

Switching drives used to cancel the scan in flight and throw away the finished tree. Now each drive
keeps its own state, and a scan keeps running while another drive is on screen.

## Mechanism

State for the drive on screen lives where it always did (`App` fields, `Win` fields), so the draw
code and the app tests did not change. The other drives wait in `Win::parked`, one `Drive` each
(`Drive.app` is `app::Parked`: tree, rows, scroll, scan state, summary, live status, per-tree filter
state; the rest is the scan channel and progress, the live watch, the held directory watch).

- `swap_drive` exchanges all of it at once (`App::switch_slot` + `swap_window_fields`).
- `Win::open(drive, root)` parks the drive on screen, then unparks the target or starts a scan.
  Choosing the drive already on screen scans it afresh, as before.
- `Win::in_slot(drive, f)` runs `f` with a parked drive swapped in. `WM_SCAN_DONE` carries the drive
  index in `wparam`; `WM_JOURNAL` visits every drive with a watch. So scan results and live changes
  reach a drive that is off screen through the same code that serves the one on screen. While swapped
  in, `borrowed` keeps timers and the filter alone.
- `sync_timers` derives the progress, preview and live timers from the state on screen; nothing else
  starts or stops them.

## Decisions

- At most `KEPT_DRIVES` (3) trees are held, the one on screen included. The least recently used drive
  without a scan running is dropped (its watch stops). Coming back to it rescans.
- Live watches stay on for parked drives. Changes are applied to the parked tree; its rows refresh when
  it comes back. A watch that ends while parked is marked stale (no auto-rescan was needed there: a
  background rescan does start if the policy allows).
- Only the drive on screen has a partial-tree preview. Parking a first scan drops its partial tree and
  preview; returning shows the progress counts until the scan finishes.
- Filter: the query text is shared; the installed result belongs to the drive. Parking takes the filter
  off the rows; unparking filters the returning tree afresh. Generations come from one counter so a
  result can only match the run that asked for it.
- Cancel and Esc cancel the scan of the drive on screen only.

## Known limits

- A parked drive under heavy churn grows `Tree::touched` until it is shown again (8 bytes a change).
- Chips do not show that a background scan is running or done.
- `--selftest-watch` covers: a parked drive's watch applying changes, instant return to a kept tree,
  a scan surviving a switch away and back. It uses a made-up second drive entry with a temp-dir root.
