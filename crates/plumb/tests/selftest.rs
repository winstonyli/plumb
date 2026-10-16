#![cfg(windows)]
//! Runs the real binary's `--selftest`: hidden window, real swapchain on the monitor's adapter,
//! a scan of a temp directory through the message loop. Uses the physical GPU, so it is opt-in:
//! `cargo test -p plumb -- --ignored`.
use std::process::Command;

#[test]
#[ignore = "uses the physical GPU and creates a hidden window"]
fn hidden_window_scans_and_renders() {
    let out = Command::new(env!("CARGO_BIN_EXE_plumb")).arg("--selftest").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("selftest ok") && stdout.contains("1 files") && stdout.contains("102400"), "{stdout}");
}

/// Runs `--selftest-delete`: marks files in a scratch directory and answers the real commit dialogs.
/// The dialogs flash on screen for a moment, and one file passes through the Recycle Bin.
#[test]
#[ignore = "shows the commit dialog briefly and uses the Recycle Bin"]
fn commit_dialogs_delete_only_the_marked_items() {
    let out = Command::new(env!("CARGO_BIN_EXE_plumb")).arg("--selftest-delete").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("selftest-delete ok"), "{stdout}");
}
