//! The Processes view's runtime: a poller thread that takes a snapshot every second, and the reconciler state that
//! folds each snapshot into the app's tree.
use crate::app::{process_summary, App, ProcInfo};
use plumb_core::procs::{Metric, Procs, Sample};
use plumb_scan::procs::{physical_memory, ProcEntry, Snapshotter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::Thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// How often the poller looks.
const POLL: Duration = Duration::from_secs(1);
/// Dead nodes the tree may collect before it is built afresh (selection and expansion reset once).
const MAX_DEAD: usize = 20_000;

/// One snapshot, posted to the window as the `lparam` of `WM_PROCS` (a leaked `Box` if the window is gone).
pub struct Snapshot {
    pub drive: usize,
    pub entries: Vec<ProcEntry>,
    /// 100 ns since the previous snapshot of this poller; 0 for its first.
    pub elapsed: u64,
    /// (total, available) physical memory.
    pub ram: (u64, u64),
}

/// The thread that takes the snapshots. It runs until stopped; stopping does not wait for it.
struct Poller {
    stop: Arc<AtomicBool>,
    thread: Thread,
}

impl Poller {
    fn start(hwnd: HWND, drive: usize, msg: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let hwnd = hwnd.0 as isize;
        let handle = std::thread::spawn(move || {
            let mut last: Option<Instant> = None;
            let mut shots = Snapshotter::default();
            while !flag.load(Ordering::Acquire) {
                if let Ok(entries) = shots.take() {
                    let now = Instant::now();
                    let elapsed = last.map_or(0, |t| (now - t).as_nanos() as u64 / 100);
                    last = Some(now);
                    let boxed = Box::into_raw(Box::new(Snapshot { drive, entries, elapsed, ram: physical_memory() }));
                    if unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(boxed as isize)) }.is_err() {
                        drop(unsafe { Box::from_raw(boxed) }); // the window is gone
                        return;
                    }
                }
                std::thread::park_timeout(POLL);
            }
        });
        Self { stop, thread: handle.thread().clone() }
    }

    fn kick(&self) {
        self.thread.unpark();
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

/// What a Processes view keeps besides the app's tree: the reconciler, and the poller while it is on screen.
#[derive(Default)]
pub struct ProcState {
    sync: Option<Procs>,
    poller: Option<Poller>,
    /// Snapshots folded in so far.
    updates: u64,
}

impl ProcState {
    /// Starts polling (if not already), posting `msg` to `hwnd`. The first snapshot after a start has no rates.
    pub fn start(&mut self, hwnd: HWND, drive: usize, msg: u32) {
        if self.poller.is_none() {
            self.poller = Some(Poller::start(hwnd, drive, msg));
        }
    }

    pub fn stop(&mut self) {
        if let Some(p) = self.poller.take() {
            p.stop();
        }
    }

    pub fn polling(&self) -> bool {
        self.poller.is_some()
    }

    /// Weighs the processes by `m` from now on (the view must be the app's active one).
    pub fn set_metric(&mut self, app: &mut App, m: Metric) {
        app.proc_metric = m;
        if let (Some(sync), Some(tree)) = (self.sync.as_mut(), app.tree.as_mut()) {
            sync.set_metric(tree, m);
            app.refresh_rows();
        }
    }

    pub fn updates(&self) -> u64 {
        self.updates
    }

    /// Asks for a snapshot now.
    pub fn kick(&self) {
        if let Some(p) = &self.poller {
            p.kick();
        }
    }

    /// Folds a snapshot into the app's tree (this view must be the app's active one). A snapshot that arrives after
    /// polling stopped is dropped. Returns whether the chip's label changed width-relevantly (free memory text).
    pub fn update(&mut self, app: &mut App, snap: Snapshot) -> bool {
        if self.poller.is_none() {
            return false;
        }
        self.updates += 1;
        let Snapshot { drive, entries, elapsed, ram } = snap;
        let samples: Vec<Sample> = entries
            .iter()
            .map(|e| Sample { pid: e.pid, parent: e.parent, created: e.created, name: e.name.clone(), memory: e.working_set, cpu_100ns: e.cpu_100ns, io_bytes: e.io_bytes })
            .collect();
        let used = entries.iter().filter(|e| e.pid != 0).map(|e| e.working_set).sum();
        let summary = process_summary(entries.len(), used, ram.0);
        let info = entries.into_iter().map(|e| (e.pid, ProcInfo { path: e.path, created: e.created })).collect();
        let label = |app: &App| app.drives.get(drive).map(|d| crate::fmt::size(d.free));
        let before = label(app);
        app.set_procs(drive, info, summary.clone(), ram);
        let rebuild = self.sync.is_none() || app.tree.as_ref().is_none_or(|t| t.dead() > MAX_DEAD);
        match (rebuild, self.sync.as_mut(), app.tree.as_mut()) {
            (false, Some(sync), Some(tree)) => {
                sync.apply(tree, &samples, elapsed);
                app.refresh_rows();
            }
            _ => {
                let (mut sync, mut tree) = Procs::new(app.proc_metric);
                sync.apply(&mut tree, &samples, elapsed);
                app.finish_scan(tree, summary);
                self.sync = Some(sync);
            }
        }
        label(app) != before
    }
}
