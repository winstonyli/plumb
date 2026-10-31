//! Following a scanned tree as it changes: the change journal or the directory watcher, and applying what they report.
use super::*;

/// What a live watch follows.
pub(super) enum Source {
    Journal { reader: JournalReader, live: Live, queue: VecDeque<Delta> },
    Dir { watcher: Watcher, queue: VecDeque<Listing> },
}

pub(super) struct Scanned {
    pub(super) tree: Tree,
    pub(super) report: ScanReport,
    pub(super) elapsed: Duration,
    /// The volume and journal position to follow, read before the scan began so changes made
    /// during it are replayed. Present only for elevated NTFS drives.
    pub(super) journal: Option<(Volume, JournalInfo)>,
    pub(super) root: String,
}

pub(super) type ScanResult = std::result::Result<Scanned, String>;

/// Everything live updates need for the view on screen.
pub(super) struct Watch {
    pub(super) src: Source,
    pub(super) pacer: Pacer,
    /// The tree changed since the rows were last refreshed.
    pub(super) dirty: bool,
    /// `TIMER_LIVE` is running.
    pub(super) armed: bool,
}

/// Why a watch ended; `why` is `None` when it was asked to stop.
pub(super) struct Ended {
    pub(super) rescan: bool,
    pub(super) why: Option<String>,
}

impl Watch {
    /// Moves what the source has queued into its queue; `Some` if the source has ended.
    pub(super) fn drain(&mut self) -> Option<Ended> {
        let ended = |rescan: bool, why: &str| Some(Ended { rescan, why: Some(why.to_string()) });
        match &mut self.src {
            Source::Journal { reader, queue, .. } => {
                while let Some(ev) = reader.try_recv() {
                    match ev {
                        Event::Deltas(d) => queue.extend(d),
                        Event::Stopped(Stop::Requested) => return Some(Ended { rescan: false, why: None }),
                        Event::Stopped(Stop::Wrapped) => {
                            return ended(true, "the change journal wrapped, so changes were missed");
                        }
                        Event::Stopped(Stop::Disabled) => return ended(false, "the change journal is not active"),
                        Event::Stopped(Stop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
            Source::Dir { watcher, queue } => {
                while let Some(ev) = watcher.try_recv() {
                    match ev {
                        WatchEvent::Listings(l) => queue.extend(l),
                        WatchEvent::Stopped(WatchStop::Overflow) => {
                            return ended(true, "the directory watch overflowed, so changes were missed");
                        }
                        WatchEvent::Stopped(WatchStop::TooMany) => {
                            return ended(true, "too many folders changed at once");
                        }
                        WatchEvent::Stopped(WatchStop::Failed(m)) => return ended(false, &m),
                    }
                }
            }
        }
        None
    }

    /// Applies queued changes for the budget; returns how many tree changes were made.
    pub(super) fn apply(&mut self, tree: &mut Tree, deadline: Instant) -> usize {
        match &mut self.src {
            Source::Journal { live, queue, .. } => live.apply(tree, queue, deadline),
            Source::Dir { queue, .. } => crate::dirsync::apply(tree, queue, deadline),
        }
    }

    pub(super) fn queued(&self) -> bool {
        match &self.src {
            Source::Journal { queue, .. } => !queue.is_empty(),
            Source::Dir { queue, .. } => !queue.is_empty(),
        }
    }

    /// Work left that a timer tick should finish.
    pub(super) fn backlog(&self) -> bool {
        self.queued() || matches!(&self.src, Source::Journal { live, .. } if live.pending())
    }

    /// Stops the source (blocks briefly: call off the UI thread).
    pub(super) fn stop(self) {
        match self.src {
            Source::Journal { reader, .. } => reader.stop(),
            Source::Dir { watcher, .. } => watcher.stop(),
        }
    }
}

pub(super) fn open_journal(root: &str) -> Option<(Volume, JournalInfo)> {
    let root = Path::new(root);
    if !is_elevated() || !is_ntfs(root) {
        return None;
    }
    let vol = Volume::open(root).ok()?; // fails for anything but a whole volume
    let info = vol.query().ok()?;
    Some((vol, info))
}

pub(super) fn local_time() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

impl Win {
    pub(super) fn start_watch(&mut self, vol: Volume, info: JournalInfo, records: Vec<NodeId>) {
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        let reader = JournalReader::start(vol, info, JournalConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        });
        self.watch = Some(Watch {
            src: Source::Journal { reader, live: Live::new(records), queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
    }

    /// Makes the directory watch started with the scan the live watch: releases what it collected
    /// while the scan ran and wakes the window to apply it.
    pub(super) fn install_dir_watch(&mut self, watcher: Watcher) {
        watcher.release();
        self.watch = Some(Watch {
            src: Source::Dir { watcher, queue: VecDeque::new() },
            pacer: Pacer::new(Instant::now()),
            dirty: false,
            armed: false,
        });
        self.app.set_live(LiveStatus::On { last: None });
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
    }

    /// Starts a held directory watch on `root` unless the change journal will follow it
    /// (`journal`). A watch that cannot start leaves the tree static.
    pub(super) fn start_dir_pending(&mut self, root: &str, journal: bool) {
        self.drop_pending();
        self.dir_failed = None;
        if journal {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        let posted = self.posted.clone();
        match Watcher::start(Path::new(root), WatchConfig::default(), move || {
            if !posted.swap(true, Ordering::AcqRel) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
            }
        }) {
            Ok(w) => self.dir_pending = Some(w),
            Err(m) => self.dir_failed = Some(m),
        }
    }

    pub(super) fn drop_pending(&mut self) {
        if let Some(w) = self.dir_pending.take() {
            std::thread::spawn(move || w.stop());
        }
    }

    /// Stops following changes. The source may be mid-wait, so it is joined off the UI thread.
    pub(super) fn drop_watch(&mut self) {
        if let Some(w) = self.watch.take() {
            std::thread::spawn(move || w.stop());
            self.sync_timers();
        }
    }

    /// `WM_JOURNAL`: moves what the source has queued into its queue and applies some of it.
    pub(super) fn journal_event(&mut self) {
        self.posted.store(false, Ordering::Release);
        for d in 0..self.parked.len().max(self.app.active.map_or(0, |a| a + 1)) {
            // The watches of views off screen keep their trees current too.
            self.in_slot(d, |w| {
                let ended = w.watch.as_mut().and_then(Watch::drain);
                w.apply_live();
                if let Some(e) = ended {
                    w.watch_ended(e);
                }
            });
        }
    }

    /// Applies queued changes for at most `APPLY_BUDGET`; leftovers continue on the next message.
    pub(super) fn apply_queued(&mut self) {
        let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.slot.tree) else { return };
        let n = w.apply(tree, Instant::now() + APPLY_BUDGET);
        w.dirty |= n > 0;
        if w.queued() {
            let _ = unsafe { PostMessageW(Some(self.hwnd), WM_JOURNAL, WPARAM(0), LPARAM(0)) };
        }
    }

    /// `apply_queued`, and starts the live timer if there is work to refresh. A view off screen
    /// keeps its changes marked until it is shown again.
    pub(super) fn apply_live(&mut self) {
        self.apply_queued();
        if let Some(w) = &mut self.watch
            && !w.armed
            && (w.dirty || w.backlog())
            && !self.borrowed
        {
            w.armed = true;
            self.sync_timers();
        }
    }

    /// `TIMER_LIVE`: at most every `REPAINT`, refreshes the rows once if the tree changed.
    pub(super) fn live_tick(&mut self) {
        self.apply_live();
        let due = self.watch.as_ref().is_some_and(|w| w.dirty && w.pacer.due(Instant::now()));
        if due {
            let t0 = Instant::now();
            self.app.refresh_rows();
            self.app.live_changed(local_time());
            self.invalidate();
            if let Some(w) = &mut self.watch {
                w.pacer.done(Instant::now(), t0.elapsed());
                w.dirty = false;
            }
            if self.app.slot.tree.as_ref().is_some_and(needs_compaction) {
                self.auto_rescan();
            }
        }
        if let Some(w) = &mut self.watch
            && w.armed
            && !w.dirty
            && !w.backlog()
        {
            w.armed = false;
            self.sync_timers();
        }
    }

    /// The source ended: bring the tree up to date, then rescan or say why updates stopped.
    pub(super) fn watch_ended(&mut self, e: Ended) {
        if let (Some(w), Some(tree)) = (&mut self.watch, &mut self.app.slot.tree) {
            w.apply(tree, Instant::now() + Duration::from_millis(50));
        }
        self.app.refresh_rows();
        self.drop_watch();
        self.app.set_live(LiveStatus::Off);
        let Some(why) = e.why else { return };
        if !(e.rescan && self.auto_rescan()) {
            self.app.set_live(LiveStatus::Stale(why));
        }
        self.invalidate();
    }

    /// Starts a background rescan if the policy allows (or one is already running).
    pub(super) fn auto_rescan(&mut self) -> bool {
        if self.rx.is_some() {
            return true;
        }
        let Some(i) = self.app.active else { return false };
        if !self.policy.allow(&self.app.views[i].root, Instant::now(), self.last_input) {
            return false;
        }
        self.start_rescan(i);
        true
    }
}
