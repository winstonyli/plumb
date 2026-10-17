//! The commit dialog: what is about to be deleted, and how.
use crate::fmt;
use plumb_scan::holders::Holder;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::{IDNO, IDYES};
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Recycle,
    Permanent,
    /// Unmark everything.
    Clear,
    Cancel,
}

pub(crate) const RECYCLE: i32 = 1001;
pub(crate) const PERMANENT: i32 = 1002;
const CLEAR: i32 = 1003;
pub(crate) const RETRY: i32 = 1004;
/// The paths shown before "and N more".
const SHOWN: usize = 20;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn plural(n: usize) -> String {
    format!("{n} item{}", if n == 1 { "" } else { "s" })
}

/// Asks what to do with the marked `paths` (`bytes` is what they hold, an upper bound).
pub fn choose(owner: HWND, paths: &[String], bytes: u64) -> Choice {
    let title = wide("plumb");
    let head = wide(&format!("Delete {}? Up to {}.", plural(paths.len()), fmt::size(bytes)));
    let body = wide("Recycle moves them to the Recycle Bin (Explorer can undo it). Delete permanently cannot be undone.");
    let list = wide(&fmt::path_list(paths, SHOWN));
    let toggle = wide("Show the paths");
    let (recycle, permanent, clear) = (wide("Recycle"), wide("Delete permanently"), wide("Unmark all"));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: RECYCLE, pszButtonText: PCWSTR(recycle.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: PERMANENT, pszButtonText: PCWSTR(permanent.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: CLEAR, pszButtonText: PCWSTR(clear.as_ptr()) },
    ];
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_EXPAND_FOOTER_AREA,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        pszExpandedInformation: PCWSTR(list.as_ptr()),
        pszExpandedControlText: PCWSTR(toggle.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: RECYCLE,
        ..Default::default()
    };
    let mut id = 0;
    match unsafe { TaskDialogIndirect(&cfg, Some(&mut id), None, None) } {
        Ok(()) => match id {
            RECYCLE => Choice::Recycle,
            PERMANENT => Choice::Permanent,
            CLEAR => Choice::Clear,
            _ => Choice::Cancel,
        },
        Err(_) => Choice::Cancel,
    }
}

/// The failure dialog's words: the headline, and the details one line per item (the first `SHOWN`, then a count).
fn failure_text(failed: &[(String, String)], total: usize) -> (String, String) {
    let head = format!("{} of {} not deleted", failed.len(), total);
    let lines: Vec<String> = failed.iter().map(|(p, why)| format!("{p}: {why}")).collect();
    (head, fmt::path_list(&lines, SHOWN))
}

/// Reports what a delete could not remove, with the reason for each; `true` to retry just those.
pub fn failed(owner: HWND, failed: &[(String, String)], total: usize) -> bool {
    let (head, list) = failure_text(failed, total);
    let title = wide("plumb");
    let head = wide(&head);
    let body = wide(if failed.len() == 1 { "It stays marked. Retry tries it again." } else { "They stay marked. Retry tries them again." });
    let list = wide(&list);
    let toggle = wide("Show the reasons");
    let retry = wide("Retry");
    let buttons = [TASKDIALOG_BUTTON { nButtonID: RETRY, pszButtonText: PCWSTR(retry.as_ptr()) }];
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_EXPAND_FOOTER_AREA | TDF_EXPANDED_BY_DEFAULT,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        pszExpandedInformation: PCWSTR(list.as_ptr()),
        pszExpandedControlText: PCWSTR(toggle.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: RETRY,
        ..Default::default()
    };
    let mut id = 0;
    unsafe { TaskDialogIndirect(&cfg, Some(&mut id), None, None) }.is_ok() && id == RETRY
}

/// The words of the "who is using this" answer: the headline, and one `PID  name` line each.
fn holders_text(path: &str, held_by: &[Holder]) -> (String, String) {
    let list = held_by.iter().map(|h| format!("{}  {}", h.pid, h.name)).collect::<Vec<_>>().join("\n");
    let head = match held_by.len() {
        0 => format!("Nothing is using {path}."),
        1 => format!("1 program is using {path}."),
        n => format!("{n} programs are using {path}."),
    };
    (head, list)
}

/// Says which programs hold `path` open (or that none do).
pub fn holders(owner: HWND, path: &str, held_by: &[Holder]) {
    let (head, list) = holders_text(path, held_by);
    let (title, head, list) = (wide("plumb"), wide(&head), wide(&list));
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: if held_by.is_empty() { PCWSTR::null() } else { PCWSTR(list.as_ptr()) },
        ..Default::default()
    };
    let _ = unsafe { TaskDialogIndirect(&cfg, None, None, None) };
}

/// The second question before a permanent delete.
pub fn confirm_permanent(owner: HWND, count: usize, bytes: u64) -> bool {
    let title = wide("plumb");
    let head = wide(&format!("Permanently delete {}?", plural(count)));
    let body = wide(&format!("Up to {} will be removed for good. This cannot be undone.", fmt::size(bytes)));
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(head.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        nDefaultButton: IDNO.0,
        ..Default::default()
    };
    let mut id = 0;
    unsafe { TaskDialogIndirect(&cfg, Some(&mut id), None, None) }.is_ok() && id == IDYES.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_holders_text_says_nothing_one_or_many() {
        let h = |name: &str, pid| Holder { pid, name: name.into() };
        assert_eq!(holders_text("f.txt", &[]), ("Nothing is using f.txt.".into(), String::new()));
        assert_eq!(holders_text("f.txt", &[h("x.exe", 7)]), ("1 program is using f.txt.".into(), "7  x.exe".into()));
        assert_eq!(holders_text("f.txt", &[h("x.exe", 7), h("y.exe", 8)]), ("2 programs are using f.txt.".into(), "7  x.exe\n8  y.exe".into()));
    }

    #[test]
    fn the_failure_text_counts_and_lists_each_reason_and_caps_a_long_list() {
        let f: Vec<(String, String)> = (0..22).map(|i| (format!("C:\\f{i}"), "in use by another program".into())).collect();
        let (head, list) = failure_text(&f, 30);
        assert_eq!(head, "22 of 30 not deleted");
        assert!(list.starts_with("C:\\f0: in use by another program\nC:\\f1: "), "{list}");
        assert!(list.ends_with("\nand 2 more"), "{list}");
        assert_eq!(failure_text(&f[..1], 1).0, "1 of 1 not deleted");
    }
}
