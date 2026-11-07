//! The in-app modal: one panel over the dimmed window that asks, or tells, and is answered with a button.
//! It is state the app owns, not a nested message loop, so the answer is an `Effect` like any other input.
use crate::app::CommitRequest;
use crate::fmt;
use crate::layout::{Rect, FILTER_BOX_H, ROW_H};
use plumb_scan::holders::Holder;
use plumb_scan::reason::Reason;

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
    /// Put the holders on the clipboard; the modal stays.
    Copy,
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
    /// Only to be read: which programs hold a file open, why a process could not be ended.
    Info,
    /// End these processes, in this order.
    End(Vec<Victim>),
    /// A delete is still running and the window is being closed.
    Close,
}

/// A process to end: the pid and creation time (FILETIME) together identify it, since pids are reused.
#[derive(Clone, Debug, PartialEq)]
pub struct Victim {
    pub pid: u32,
    pub created: u64,
    pub name: String,
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
    /// What Copy puts on the clipboard, in full (the lines shown may be cut to fit); `None` for a modal with no Copy button.
    pub copy: Option<String>,
}

/// The detail lines shown before "and N more".
const SHOWN: usize = 8;
const PAD: f32 = 20.0;
const GAP: f32 = 8.0;
const HEADING_H: f32 = 28.0;
const MIN_W: f32 = 460.0;

fn items(n: usize) -> fmt::Show<'static> {
    fmt::noun(n as u64, "item", "items")
}

fn sentence(reason: &str, hint: &str) -> String {
    let mut c = reason.chars();
    let first: String = c.next().map(|f| f.to_uppercase().collect()).unwrap_or_default();
    format!("{first}{}.{hint}", c.as_str())
}

/// The parts every "could not be done" dialog shares, so each reads the same: one item gets "Could not <verb> <it>" and
/// its reason as a sentence; several get "N of M not <past>" and a line per item. `hint` follows the reason(s).
struct Failure {
    heading: String,
    body: Vec<String>,
    detail: Vec<String>,
    /// "<subject>: <reason>" per item, for Copy.
    copy: String,
}

fn failure(verb: &str, past: &str, failed: &[(String, String)], total: usize, hint: &str) -> Failure {
    let each: Vec<String> = failed.iter().map(|(subject, why)| format!("{subject}: {why}")).collect();
    let copy = each.join("\n");
    match failed {
        [(subject, why)] if total == 1 => Failure { heading: format!("Could not {verb} {subject}"), body: vec![sentence(why, hint)], detail: Vec::new(), copy },
        _ => Failure { heading: format!("{} of {total} not {past}", failed.len()), body: vec![hint.trim().to_string()], detail: lines(&each), copy },
    }
}

/// What to do about a refusal, for the process dialogs: only "access denied" has a remedy.
fn admin_hint<'a>(reasons: impl IntoIterator<Item = &'a Reason>) -> &'static str {
    if reasons.into_iter().any(Reason::is_access_denied) { " It may be protected or elevated; run plumb as administrator to reach more processes." } else { "" }
}

fn lines(items: &[String]) -> Vec<String> {
    fmt::path_list(items, SHOWN).lines().map(str::to_string).collect()
}

fn button(pick: Pick, label: &'static str, primary: bool) -> Button {
    Button { pick, label, primary }
}

impl Modal {
    fn new(ask: Ask, heading: String, body: &[&str], detail: Vec<String>, buttons: Vec<Button>, focus: usize, cancel: Pick) -> Self {
        Self { ask, heading, body: body.iter().map(|s| s.to_string()).collect(), detail, buttons, focus, cancel, copy: None }
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
        let f = failure("delete", "deleted", failed, total, "");
        let again = if failed.len() == 1 { "It stays marked. Retry tries it again." } else { "They stay marked. Retry tries them again." };
        let paths = failed.iter().map(|f| f.0.clone()).collect();
        let mut body: Vec<&str> = f.body.iter().map(String::as_str).filter(|l| !l.is_empty()).collect();
        body.push(again);
        let buttons = vec![button(Pick::Retry, "Retry", true), button(Pick::Copy, "Copy", false), button(Pick::Close, "Close", false)];
        let mut m = Self::new(Ask::Failed { view, permanent, paths }, f.heading, &body, f.detail, buttons, 0, Pick::Close);
        m.copy = Some(f.copy);
        m
    }

    /// Which programs hold `path` open (or that none do).
    pub fn holders(path: &str, held_by: &[Holder]) -> Self {
        let heading = match held_by.len() {
            0 => format!("Nothing is using {path}."),
            1 => format!("1 program is using {path}."),
            n => format!("{n} programs are using {path}."),
        };
        let list: Vec<String> = held_by.iter().map(|h| format!("{}  {}", h.pid, h.name)).collect();
        if list.is_empty() {
            return Self::new(Ask::Info, heading, &[], Vec::new(), vec![button(Pick::Close, "Close", true)], 0, Pick::Close);
        }
        let text = format!("{path}\n{}", list.join("\n"));
        let mut m = Self::new(Ask::Info, heading, &[], lines(&list), vec![button(Pick::Copy, "Copy", false), button(Pick::Close, "Close", true)], 1, Pick::Close);
        m.copy = Some(text);
        m
    }

    /// Ending `victims` (one process, or a process and its descendants); Cancel is the default.
    pub fn end(victims: Vec<Victim>) -> Self {
        let n = victims.len();
        let (heading, body, label) = match victims.first() {
            Some(v) if n == 1 => (format!("End {} (PID {})?", v.name, v.pid), "It stops at once, and anything unsaved in it is lost.", "End process"),
            _ => (format!("End {n} processes?"), "They stop at once, and anything unsaved in them is lost.", "End all"),
        };
        let detail = if n > 1 { lines(&victims.iter().map(|v| format!("{}  (PID {})", v.name, v.pid)).collect::<Vec<_>>()) } else { Vec::new() };
        let buttons = vec![button(Pick::Yes, label, false), button(Pick::No, "Cancel", true)];
        Self::new(Ask::End(victims), heading, &[body], detail, buttons, 1, Pick::No)
    }

    /// Why these processes could not be ended: each with its reason.
    pub fn end_failed(failed: &[(Victim, Reason)], total: usize) -> Self {
        Self::process_failed("end", "ended", failed, total)
    }

    /// Why `victim` could not be suspended (or, with `resume`, resumed).
    pub fn pause_failed(victim: &Victim, why: &Reason, resume: bool) -> Self {
        let (verb, past) = if resume { ("resume", "resumed") } else { ("suspend", "suspended") };
        Self::process_failed(verb, past, &[(victim.clone(), why.clone())], 1)
    }

    fn process_failed(verb: &str, past: &str, failed: &[(Victim, Reason)], total: usize) -> Self {
        let named: Vec<(String, String)> = failed.iter().map(|(v, why)| (format!("{} (PID {})", v.name, v.pid), why.to_string())).collect();
        let f = failure(verb, past, &named, total, admin_hint(failed.iter().map(|(_, why)| why)));
        let buttons = vec![button(Pick::Copy, "Copy", false), button(Pick::Close, "Close", true)];
        let body: Vec<&str> = f.body.iter().map(String::as_str).filter(|l| !l.is_empty()).collect();
        let mut m = Self::new(Ask::Info, f.heading, &body, f.detail, buttons, 1, Pick::Close);
        m.copy = Some(f.copy);
        m
    }

    /// Wraps each body line to the panel, `measure` giving a text's width in DIPs. Done once, when the modal is put up, so
    /// that `geometry` stays a pure function of the lines.
    pub fn fit(&mut self, w: f32, h: f32, measure: &dyn Fn(&str) -> f32) {
        let width = self.geometry(w, h).body.w;
        self.body = self.body.iter().flat_map(|l| fmt::wrap(l, width, measure)).collect();
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
        let one = Modal::failed(3, false, &f[..1], 1);
        assert_eq!((one.heading.as_str(), one.body[0].as_str()), ("Could not delete C:\\f0", "In use by another program."));
        assert_eq!(m.ask, Ask::Failed { view: 3, permanent: false, paths: vec!["C:\\f0".into(), "C:\\f1".into()] });
        let h = |name: &str, pid| Holder { pid, name: name.into() };
        assert_eq!(Modal::holders("f.txt", &[]).heading, "Nothing is using f.txt.");
        assert_eq!(Modal::holders("f.txt", &[h("x.exe", 7)]).detail, ["7  x.exe"]);
        assert_eq!(Modal::holders("f.txt", &[h("x.exe", 7), h("y.exe", 8)]).heading, "2 programs are using f.txt.");
    }

    #[test]
    fn the_holders_modal_offers_copy_of_the_whole_list_beside_close_and_none_when_nothing_holds_the_file() {
        let h = |name: &str, pid| Holder { pid, name: name.into() };
        let m = Modal::holders("f.txt", &[h("x.exe", 7), h("y.exe", 8)]);
        assert_eq!(m.ask, Ask::Info);
        assert_eq!(m.copy.as_deref(), Some("f.txt\n7  x.exe\n8  y.exe"));
        assert_eq!(m.buttons.iter().map(|b| b.pick).collect::<Vec<_>>(), [Pick::Copy, Pick::Close]);
        assert_eq!((m.buttons[m.focus].pick, m.cancel), (Pick::Close, Pick::Close), "Enter and Esc still close");
        let none = Modal::holders("f.txt", &[]);
        assert_eq!(none.buttons.iter().map(|b| b.pick).collect::<Vec<_>>(), [Pick::Close], "nothing to copy");
        let v = |pid, name: &str| Victim { pid, created: 5, name: name.into() };
        let f = Modal::end_failed(&[(v(7, "x.exe"), Reason::AccessDenied)], 1);
        assert_eq!(f.heading, "Could not end x.exe (PID 7)");
        assert_eq!(f.body, ["Access denied. It may be protected or elevated; run plumb as administrator to reach more processes."]);
        assert_eq!(f.copy.as_deref(), Some("x.exe (PID 7): access denied"), "the copy is the plain reason");
        let two = Modal::end_failed(&[(v(7, "x.exe"), Reason::AccessDenied), (v(8, "y.exe"), Reason::other("it has already ended"))], 3);
        assert_eq!((two.heading.as_str(), two.detail.len()), ("2 of 3 not ended", 2));
        let paused = Modal::pause_failed(&v(9, "z.exe"), &Reason::other("the system refused (status 0x1)"), true);
        assert_eq!((paused.heading.as_str(), paused.body[0].as_str()), ("Could not resume z.exe (PID 9)", "The system refused (status 0x1)."), "no hint without access denied");
        assert_eq!(two.detail[1], "y.exe (PID 8): it has already ended");
        let (one, many) = (Modal::end(vec![v(7, "x.exe")]), Modal::end(vec![v(7, "x.exe"), v(8, "y.exe")]));
        assert_eq!((one.buttons[0].label, one.detail.len()), ("End process", 0));
        assert_eq!((many.heading.as_str(), many.buttons[0].label, many.detail.len()), ("End 2 processes?", "End all", 2));
        let narrow = |s: &str| s.chars().count() as f32 * 7.0;
        let before = f.geometry(1000.0, 700.0).panel.h;
        let mut wrapped = f.clone();
        wrapped.fit(1000.0, 700.0, &narrow);
        assert!(wrapped.body.len() > 1 && wrapped.body.iter().all(|l| narrow(l) <= 420.0), "{:?}", wrapped.body);
        assert!(wrapped.geometry(1000.0, 700.0).panel.h > before, "the panel grows with the wrapped lines");
        let many: Vec<Holder> = (0..30).map(|i| h("p.exe", i)).collect();
        let text = Modal::holders("f.txt", &many).copy.unwrap();
        assert_eq!(text.lines().count(), 31, "the copy has every holder, not the shown ones");
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
