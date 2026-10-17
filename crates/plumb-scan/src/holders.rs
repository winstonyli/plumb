//! Who is using a file: the Restart Manager's answer for one path.
//!
//! `RmGetList` can take a while, so call this from a worker thread. Any failure reads as "nobody found".
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::RestartManager::{RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO};
use windows::core::{PCWSTR, PWSTR};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// The executable's file name, or the Restart Manager's name for the application when the path is unknown.
    pub name: String,
}

/// Ends the Restart Manager session when dropped.
struct Session(u32);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = unsafe { RmEndSession(self.0) };
    }
}

fn utf16(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
}

/// The programs that have `path` open (empty on any error, or when nothing does).
pub fn holders(path: &str) -> Vec<Holder> {
    let mut handle = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    if unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let _session = Session(handle);
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    if unsafe { RmRegisterResources(handle, Some(&[PCWSTR(wide.as_ptr())]), None, None) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut room = 8u32;
    // The list can grow between the call that sizes it and the one that fills it: ask again, a few times.
    for _ in 0..4 {
        let mut info = vec![RM_PROCESS_INFO::default(); room as usize];
        let (mut needed, mut got, mut reasons) = (0u32, room, 0u32);
        let r = unsafe { RmGetList(handle, &mut needed, &mut got, Some(info.as_mut_ptr()), &mut reasons) };
        if r == ERROR_SUCCESS {
            return info[..got as usize]
                .iter()
                .map(|p| {
                    let pid = p.Process.dwProcessId;
                    let name = crate::procs::image_path(pid)
                        .and_then(|f| f.rsplit('\\').next().map(str::to_string))
                        .unwrap_or_else(|| utf16(&p.strAppName));
                    Holder { pid, name }
                })
                .collect();
        }
        if r != ERROR_MORE_DATA {
            break;
        }
        room = needed.max(room + 1);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("plumb-holders-{tag}-{}.tmp", std::process::id()));
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn a_file_this_process_holds_names_this_process() {
        let p = scratch("held");
        let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p).unwrap();
        let found = holders(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        assert!(found.iter().any(|h| h.pid == std::process::id()), "{found:?}");
        assert!(found.iter().all(|h| !h.name.is_empty()), "{found:?}");
    }

    #[test]
    fn a_file_nobody_holds_has_no_holders() {
        let p = scratch("free");
        let found = holders(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_missing_path_has_no_holders() {
        assert!(holders(r"C:\no\such\plumb-file.tmp").is_empty());
    }
}
