//! A snapshot of the running processes: `NtQuerySystemInformation(SystemProcessInformation)`, parsed by x64 offset.
use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};

const SYSTEM_PROCESS_INFORMATION: u32 = 5;

#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    pub parent: u32,
    /// FILETIME (100 ns since 1601); with the pid it identifies a process, since pids are reused.
    pub created: u64,
    pub name: String,
    pub working_set: u64,
    pub private_bytes: u64,
    /// User plus kernel time, 100 ns units.
    pub cpu_100ns: u64,
    /// Bytes read, written and transferred by other operations.
    pub io_bytes: u64,
    /// `None` when the process cannot be opened (another user's, protected).
    pub path: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProcError(pub i32);

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn image_path(pid: u32) -> Option<String> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut n) }.is_ok();
    let _ = unsafe { CloseHandle(h) };
    ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

pub fn snapshot() -> Result<Vec<ProcEntry>, ProcError> {
    let words = crate::nt::query(SYSTEM_PROCESS_INFORMATION, 1 << 20).map_err(ProcError)?;
    let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 8) };
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let e = &bytes[at..];
        let pid = u64_at(e, 80) as u32;
        let len = u16::from_le_bytes([e[56], e[57]]) as usize;
        let name_ptr = u64_at(e, 64) as *const u16;
        let name = if pid == 0 || len == 0 || name_ptr.is_null() {
            if pid == 0 { "System Idle Process".to_string() } else { String::new() }
        } else {
            // The name lives in the same buffer, after the entries.
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(name_ptr, len / 2) })
        };
        out.push(ProcEntry {
            pid,
            parent: u64_at(e, 88) as u32,
            created: u64_at(e, 32),
            name,
            working_set: u64_at(e, 144),
            private_bytes: u64_at(e, 200),
            cpu_100ns: u64_at(e, 40) + u64_at(e, 48),
            io_bytes: u64_at(e, 232) + u64_at(e, 240) + u64_at(e, 248),
            path: if pid == 0 { None } else { image_path(pid) },
        });
        let next = u32::from_le_bytes(e[0..4].try_into().unwrap()) as usize;
        if next == 0 {
            break;
        }
        at += next;
    }
    Ok(out)
}

/// (total, available) physical memory in bytes.
pub fn physical_memory() -> (u64, u64) {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() { (m.ullTotalPhys, m.ullAvailPhys) } else { (0, 0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    #[test]
    fn our_own_process_is_listed_with_a_plausible_working_set() {
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 3 127.0.0.1 >nul"]).spawn().unwrap();
        let list = snapshot().unwrap();
        let me = std::process::id();
        let e = list.iter().find(|p| p.pid == me).expect("own pid listed");
        let mut pmc = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb).unwrap() };
        let ws = pmc.WorkingSetSize as f64;
        assert!((e.working_set as f64 - ws).abs() <= ws * 0.2, "{} vs {}", e.working_set, ws);
        let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut x, &mut k, &mut u).unwrap() };
        assert_eq!(e.created, (c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64);
        let exe = std::env::current_exe().unwrap();
        assert!(e.name.eq_ignore_ascii_case(exe.file_name().unwrap().to_str().unwrap()), "{}", e.name);
        assert!(e.path.as_deref().is_some_and(|p| p.to_lowercase().ends_with(&exe.file_name().unwrap().to_string_lossy().to_lowercase())), "{:?}", e.path);
        assert!(list.iter().any(|p| p.pid == child.id() && p.parent == me), "child listed under us");
        assert!(list.iter().any(|p| p.pid == 0 && p.name == "System Idle Process"));
        assert!(physical_memory().0 > 1 << 30);
        let _ = child.kill();
        let _ = child.wait();
    }
}
