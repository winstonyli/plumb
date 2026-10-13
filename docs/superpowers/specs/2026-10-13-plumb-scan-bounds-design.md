# Soft upper bound on the % of parent bar during a scan

While a scan runs, a row's bar is its current share of its parent's current size. The bar gets a faded extension to the most that share could still grow.

- **Estimate.** `U = total - free` from `GetDiskFreeSpaceExW` (already in `DriveInfo`), `S` the scanner's running byte count (`Scan::Running.bytes`). Unscanned `R = U - S` (`App::unscanned`).
- **High share.** With `s` the row's size and `p` its parent's: `(s + R) / (p + R)`, i.e. all unscanned bytes land in this row. It is always at least the current `s / p`. The bar is solid to `s / p` (the number shown), then fades to the high share (`Pen::fade`, a linear gradient from `Theme.bound`, an amber distinct from the blue bar, at 90% opacity to transparent). The low share `s / (p + R)` is not drawn: the solid bar already sits between low and high.
- **Gone when it means nothing.** `R` is `None` once `S > U`, and the fade is hidden. Spike (2026-10-13, unelevated walker): D: counted 141.8 GiB against 98.7 GiB used (+44%), C: 813.7 against 787.7 (+3%). Hard links, cloud placeholders and compressed or sparse files make the scan count more than the volume reports used, so `U` is not a hard bound. On a drive like D: the fade vanishes early, while the scan is still running. That is the failure mode of the estimate, not a bug.
- **Not measured:** the MFT scanner's total against `U` (needs an elevated run); whether the walker's overcount is hard links or something else.
- **Not done:** per-row numeric "≤" bounds (they would need a real bound); the root's share of `U`.
