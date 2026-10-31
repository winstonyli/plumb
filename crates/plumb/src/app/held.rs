//! The "Files held" pane of the Processes view: which process it describes, when its list is asked for again, and the
//! rows shown.
use super::*;

/// Where the "Files held" list for the selected process stands.
#[derive(Default)]
pub(super) enum HeldState {
    #[default]
    Idle,
    /// Selected; waiting for the selection to settle before asking.
    Waiting,
    Loading,
    Ready(Result<HandleList, HandleError>),
}

/// Snapshots (one a second while the view is on screen) between two refreshes of the Files held list.
pub(super) const HELD_REFRESH_SNAPSHOTS: u32 = 10;

#[derive(Default)]
pub(super) struct Held {
    pub(super) pid: Option<u32>,
    /// Bumped whenever the process of interest changes, so a late answer for another one is recognised.
    pub(super) epoch: u64,
    pub(super) state: HeldState,
    /// The first file line shown.
    pub(super) scroll: usize,
    /// The handle count when the list was last asked for.
    pub(super) handles: u32,
    /// Snapshots since the list was last asked for.
    pub(super) age: u32,
    /// A fresh look is wanted and not yet started.
    pub(super) stale: bool,
    /// A fresh look is in flight (the old list stays on screen meanwhile).
    pub(super) refreshing: bool,
}

impl App {
    /// Notes which process is selected, for the "Files held" pane. True when it changed to one that should be
    /// asked about once the selection settles (the window starts its timer).
    pub fn held_sync(&mut self) -> bool {
        let want = (self.kind() == ViewKind::Processes)
            .then(|| {
                let (tree, id) = (self.tree.as_ref()?, self.model.as_ref()?.selected()?);
                tree.aux.get(id.idx()).map(|a| a[2] as u32).filter(|&p| p != 0)
            })
            .flatten();
        if want == self.held.pid {
            return false;
        }
        self.held = Held { pid: want, epoch: self.held.epoch + 1, state: if want.is_some() { HeldState::Waiting } else { HeldState::Idle }, ..Held::default() };
        want.is_some()
    }

    /// The settle timer fired: the (generation, pid) to look up, once, if the selection is still waiting.
    pub fn held_request(&mut self) -> Option<(u64, u32)> {
        let pid = self.held.pid.filter(|_| matches!(self.held.state, HeldState::Waiting))?;
        self.held.state = HeldState::Loading;
        self.held.handles = self.held_handles().unwrap_or(0);
        self.held.age = 0;
        Some((self.held.epoch, pid))
    }

    /// The handle count of the process the pane is about, from the latest snapshot.
    pub(super) fn held_handles(&self) -> Option<u32> {
        self.proc_info.get(&self.held.pid?).map(|i| i.handles)
    }

    /// Whether the pane shows a list that was read (a refresh has something to replace).
    pub(super) fn held_readable(&self) -> bool {
        matches!(self.held.state, HeldState::Ready(Ok(_)))
    }

    /// Wants a fresh look at the files held (the selected row was clicked again). Does nothing when there is no
    /// readable list to replace.
    pub fn held_mark_stale(&mut self) {
        if self.held_readable() && !self.held.refreshing {
            self.held.stale = true;
        }
    }

    /// The (epoch, pid) to look up again, once, if a refresh is due; the old list stays until `held_done`.
    pub fn held_take_refresh(&mut self) -> Option<(u64, u32)> {
        let pid = self.held.pid.filter(|_| self.held.stale && !self.held.refreshing && self.held_readable())?;
        self.held.stale = false;
        self.held.refreshing = true;
        self.held.handles = self.held_handles().unwrap_or(self.held.handles);
        self.held.age = 0;
        Some((self.held.epoch, pid))
    }

    /// The lookup finished; an answer for a process no longer selected is dropped.
    pub fn held_done(&mut self, epoch: u64, result: Result<HandleList, HandleError>) {
        if epoch == self.held.epoch && (matches!(self.held.state, HeldState::Loading) || self.held.refreshing) {
            self.held.refreshing = false;
            self.held.state = HeldState::Ready(result);
            self.held_scroll(0); // keeps the offset, clamped to the new list
        }
    }

    /// Scrolls the file lines by `rows`, within the list.
    pub(super) fn held_scroll(&mut self, rows: i32) {
        let n = match &self.held.state {
            HeldState::Ready(Ok(list)) => list.files.len(),
            _ => 0,
        };
        self.held.scroll = (self.held.scroll as i64 + rows as i64).clamp(0, n.saturating_sub(crate::layout::HELD_ROWS) as i64) as usize;
    }

    /// The pane's title line and the file lines in view (`HELD_ROWS` from the scroll offset; the title gives the range).
    pub(super) fn held_text(&self) -> Option<(String, &[String])> {
        let pid = self.held.pid?;
        let name = self.tree.as_ref().zip(self.model.as_ref().and_then(|m| m.selected())).map_or_else(String::new, |(t, id)| t.name_string(id).to_string());
        let head = |what: String| format!("Files held by {name} (PID {pid}): {what}");
        Some(match &self.held.state {
            HeldState::Idle | HeldState::Waiting | HeldState::Loading => (head("reading\u{2026}".into()), &[]),
            HeldState::Ready(Err(HandleError::Denied)) => (head("run as administrator to see the files of this process".into()), &[]),
            HeldState::Ready(Err(HandleError::Query(_))) => (head("the system handle list could not be read".into()), &[]),
            HeldState::Ready(Ok(list)) => {
                let n = list.files.len();
                let skipped = if list.skipped > 0 { format!(", {} not readable", list.skipped) } else { String::new() };
                let first = self.held.scroll.min(n.saturating_sub(crate::layout::HELD_ROWS));
                let lines = &list.files[first..n.min(first + crate::layout::HELD_ROWS)];
                let range = if n > crate::layout::HELD_ROWS { format!(" ({}\u{2013}{})", first + 1, first + lines.len()) } else { String::new() };
                (head(format!("{}{range}{skipped}", fmt::noun(n as u64, "file", "files"))), lines)
            }
        })
    }
}
