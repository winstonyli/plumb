//! The in-app modal: one panel over the dimmed window that asks, or tells, and is answered with a button.
//! It is state the app owns, not a nested message loop, so the answer is an `Effect` like any other input.
use crate::app::CommitRequest;
use crate::fmt;
use crate::layout::{Rect, FILTER_BOX_H, ROW_H};
use plumb_scan::holders::Holder;

/// A button's meaning; the answer a modal gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Recycle,
    Permanent,
    /// Unmark everything.
    Unmark,
    Retry,
    Yes,
    No,
    Close,
    Cancel,
}

/// What a modal is about, and so what each answer does (`App::answer`).
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// How to delete the marked items.
    Delete(CommitRequest),
    /// The second question before a permanent delete.
    Permanent(CommitRequest),
    /// These paths could not be deleted; Retry tries them again.
    Failed { view: usize, permanent: bool, paths: Vec<String> },
    /// Which programs hold a file open.
    Holders,
    /// A delete is still running and the window is being closed.
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub pick: Pick,
    pub label: &'static str,
    /// The answer the modal leans toward: drawn lit.
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Modal {
    pub ask: Ask,
    pub heading: String,
    /// Short lines, shown as written (the panel does not wrap).
    pub body: Vec<String>,
    /// One line each, fitted to the panel: paths, reasons, programs.
    pub detail: Vec<String>,
    pub buttons: Vec<Button>,
    /// The button Enter presses.
    pub focus: usize,
    /// What Esc answers.
    pub cancel: Pick,
}

/// The detail lines shown before "and N more".
const SHOWN: usize = 8;
const PAD: f32 = 20.0;
const GAP: f32 = 8.0;
const HEADING_H: f32 = 28.0;
const MIN_W: f32 = 460.0;

fn items(n: usize) -> String {
    fmt::noun(n as u64, "item", "items")
}

fn lines(items: &[String]) -> Vec<String> {
    fmt::path_list(items, SHOWN).lines().map(str::to_string).collect()
}

fn button(pick: Pick, label: &'static str, primary: bool) -> Button {
    Button { pick, label, primary }
}

impl Modal {
    fn new(ask: Ask, heading: String, body: &[&str], detail: Vec<String>, buttons: Vec<Button>, focus: usize, cancel: Pick) -> Self {
        Self { ask, heading, body: body.iter().map(|s| s.to_string()).collect(), detail, buttons, focus, cancel }
    }

    /// What to do with the marked `req.paths`.
    pub fn delete(req: CommitRequest) -> Self {
        let heading = format!("Delete {}? Up to {}.", items(req.paths.len()), fmt::size(req.bytes));
        let detail = lines(&req.paths);
        let body = ["Recycle moves them to the Recycle Bin (Explorer can undo it).", "Delete permanently cannot be undone."];
        let buttons = vec![button(Pick::Recycle, "Recycle", true), button(Pick::Permanent, "Delete permanently", false), button(Pick::Unmark, "Unmark all", false), button(Pick::Cancel, "Cancel", false)];
        Self::new(Ask::Delete(req), heading, &body, detail, buttons, 0, Pick::Cancel)
    }

    /// The second question before a permanent delete; Cancel is the default.
    pub fn permanent(req: CommitRequest) -> Self {
        let heading = format!("Permanently delete {}?", items(req.paths.len()));
        let size = format!("Up to {} will be removed for good.", fmt::size(req.bytes));
        let buttons = vec![button(Pick::Yes, "Delete", false), button(Pick::No, "Cancel", true)];
        Self::new(Ask::Permanent(req), heading, &[&size, "This cannot be undone."], Vec::new(), buttons, 1, Pick::No)
    }

    /// What a delete could not remove (`failed` is (path, reason)) out of `total`; Retry tries just those again.
    pub fn failed(view: usize, permanent: bool, failed: &[(String, String)], total: usize) -> Self {
        let heading = format!("{} of {} not deleted", failed.len(), total);
        let body = [if failed.len() == 1 { "It stays marked. Retry tries it again." } else { "They stay marked. Retry tries them again." }];
        let reasons: Vec<String> = failed.iter().map(|(p, why)| format!("{p}: {why}")).collect();
        let paths = failed.iter().map(|f| f.0.clone()).collect();
        Self::new(Ask::Failed { view, permanent, paths }, heading, &body, lines(&reasons), vec![button(Pick::Retry, "Retry", true), button(Pick::Close, "Close", false)], 0, Pick::Close)
    }

    /// Which programs hold `path` open (or that none do).
    pub fn holders(path: &str, held_by: &[Holder]) -> Self {
        let heading = match held_by.len() {
            0 => format!("Nothing is using {path}."),
            1 => format!("1 program is using {path}."),
            n => format!("{n} programs are using {path}."),
        };
        let list: Vec<String> = held_by.iter().map(|h| format!("{}  {}", h.pid, h.name)).collect();
        Self::new(Ask::Holders, heading, &[], lines(&list), vec![button(Pick::Close, "Close", true)], 0, Pick::Close)
    }

    /// The window is closing while a delete runs.
    pub fn closing() -> Self {
        Self::new(Ask::Close, "A delete is still running.".into(), &["Closing now stops it partway."], Vec::new(), vec![button(Pick::Yes, "Close anyway", false), button(Pick::No, "Keep open", true)], 1, Pick::No)
    }

    /// Moves the focus to the next (or previous) button, wrapping.
    pub fn step(&mut self, forward: bool) {
        let n = self.buttons.len();
        self.focus = if forward { (self.focus + 1) % n } else { (self.focus + n - 1) % n };
    }

    /// The button with this meaning, if the modal has one.
    pub fn index_of(&self, pick: Pick) -> Option<usize> {
        self.buttons.iter().position(|b| b.pick == pick)
    }

    /// Where everything goes in a window of `w` by `h` DIPs.
    pub fn geometry(&self, w: f32, h: f32) -> Geometry {
        // The buttons are sized by their labels (an estimate: the layout has no text measure).
        let widths: Vec<f32> = self.buttons.iter().map(|b| (b.label.chars().count() as f32 * 8.0 + 32.0).max(88.0)).collect();
        let buttons_w = widths.iter().sum::<f32>() + GAP * (widths.len() as f32 - 1.0);
        let pw = (buttons_w + 2.0 * PAD).max(MIN_W).min((w - 32.0).max(0.0));
        let body_n = self.body.len();
        let mut ph = PAD + HEADING_H + 8.0 + body_n as f32 * ROW_H;
        if !self.detail.is_empty() {
            ph += 8.0 + self.detail.len() as f32 * ROW_H;
        }
        ph += 16.0 + FILTER_BOX_H + PAD;
        let panel = Rect { x: ((w - pw) / 2.0).max(0.0), y: ((h - ph) / 2.0).max(16.0), w: pw, h: ph };
        let inner = Rect { x: panel.x + PAD, w: pw - 2.0 * PAD, ..panel };
        let heading = Rect { y: panel.y + PAD, h: HEADING_H, ..inner };
        let body = Rect { y: heading.y + HEADING_H + 8.0, h: body_n as f32 * ROW_H, ..inner };
        let detail = Rect { y: body.y + body.h + if body_n > 0 || !self.detail.is_empty() { 8.0 } else { 0.0 }, h: self.detail.len() as f32 * ROW_H, ..inner };
        let by = panel.y + panel.h - PAD - FILTER_BOX_H;
        let mut x = panel.x + panel.w - PAD - buttons_w;
        let buttons = widths
            .iter()
            .map(|&bw| {
                let r = Rect { x, y: by, w: bw, h: FILTER_BOX_H };
                x += bw + GAP;
                r
            })
            .collect();
        Geometry { panel, heading, body, detail, buttons }
    }
}

pub struct Geometry {
    pub panel: Rect,
    pub heading: Rect,
    /// All the body lines, `ROW_H` each.
    pub body: Rect,
    /// All the detail lines, `ROW_H` each.
    pub detail: Rect,
    /// One per button, in order.
    pub buttons: Vec<Rect>,
}

impl Geometry {
    /// The button under (x, y).
    pub fn button_at(&self, x: f32, y: f32) -> Option<usize> {
        self.buttons.iter().position(|b| b.contains(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(n: usize) -> CommitRequest {
        CommitRequest { view: 0, paths: (0..n).map(|i| format!("C:\\f{i}")).collect(), bytes: 2048 }
    }

    #[test]
    fn the_delete_modal_counts_items_and_caps_a_long_list() {
        let m = Modal::delete(req(12));
        assert_eq!(m.heading, "Delete 12 items? Up to 2.0 KB.");
        assert_eq!(m.detail.len(), SHOWN + 1);
        assert_eq!(m.detail.last().unwrap(), "and 4 more");
        assert_eq!(Modal::delete(req(1)).heading, "Delete 1 item? Up to 2.0 KB.");
    }

    #[test]
    fn the_failure_modal_names_each_reason_and_the_holders_modal_says_none_one_or_many() {
        let f: Vec<(String, String)> = (0..2).map(|i| (format!("C:\\f{i}"), "in use by another program".into())).collect();
        let m = Modal::failed(3, false, &f, 5);
        assert_eq!(m.heading, "2 of 5 not deleted");
        assert_eq!(m.detail[0], "C:\\f0: in use by another program");
        assert_eq!(m.ask, Ask::Failed { view: 3, permanent: false, paths: vec!["C:\\f0".into(), "C:\\f1".into()] });
        let h = |name: &str, pid| Holder { pid, name: name.into() };
        assert_eq!(Modal::holders("f.txt", &[]).heading, "Nothing is using f.txt.");
        assert_eq!(Modal::holders("f.txt", &[h("x.exe", 7)]).detail, ["7  x.exe"]);
        assert_eq!(Modal::holders("f.txt", &[h("x.exe", 7), h("y.exe", 8)]).heading, "2 programs are using f.txt.");
    }

    #[test]
    fn the_panel_is_centred_holds_its_buttons_and_stays_inside_a_small_window() {
        let m = Modal::delete(req(3));
        let g = m.geometry(1000.0, 700.0);
        assert!((g.panel.x * 2.0 + g.panel.w - 1000.0).abs() < 1.0 && g.panel.y > 0.0);
        assert_eq!(g.buttons.len(), 4);
        for b in &g.buttons {
            assert!(b.x >= g.panel.x && b.x + b.w <= g.panel.x + g.panel.w && b.y + b.h <= g.panel.y + g.panel.h);
        }
        assert!(g.buttons.windows(2).all(|p| p[0].x + p[0].w <= p[1].x), "buttons do not overlap");
        assert_eq!(g.button_at(g.buttons[1].x + 2.0, g.buttons[1].y + 2.0), Some(1));
        let small = m.geometry(300.0, 200.0);
        assert!(small.panel.x >= 0.0 && small.panel.w <= 300.0);
    }

    #[test]
    fn focus_steps_around_the_buttons_and_the_permanent_question_defaults_to_cancel() {
        let mut m = Modal::delete(req(1));
        m.step(false);
        assert_eq!(m.buttons[m.focus].pick, Pick::Cancel);
        m.step(true);
        assert_eq!(m.buttons[m.focus].pick, Pick::Recycle);
        let p = Modal::permanent(req(1));
        assert_eq!(p.buttons[p.focus].pick, Pick::No);
        assert_eq!(p.cancel, Pick::No);
        assert_eq!(p.body[0], "Up to 2.0 KB will be removed for good.");
        let c = Modal::closing();
        assert_eq!(c.buttons[c.focus].pick, Pick::No);
    }
}
