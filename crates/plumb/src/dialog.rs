//! The commit dialog: what is about to be deleted, and how.
use crate::fmt;
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
