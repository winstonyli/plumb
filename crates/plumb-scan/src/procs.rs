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
    /// Open handles of all kinds (`HandleCount`, byte offset 96); a cheap sign that the files held may have changed.
    pub handles: u32,
    /// `None` when the process cannot be opened (another user's, protected).
    pub path: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProcError(pub i32);

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

pub(crate) fn image_path(pid: u32) -> Option<String> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 32768]; // the longest path Windows allows
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut n) }.is_ok();
    let _ = unsafe { CloseHandle(h) };
    ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// Takes snapshots, remembering each process's path (and that it could not be had) so the lookup, which opens
/// the process, is made once per process and not once per snapshot.
#[derive(Default)]
pub struct Snapshotter {
    paths: std::collections::HashMap<(u32, u64), Option<String>>,
}

impl Snapshotter {
    pub fn take(&mut self) -> Result<Vec<ProcEntry>, ProcError> {
        let mut list = read()?;
        let mut paths = std::collections::HashMap::with_capacity(list.len());
        for e in &mut list {
            let key = (e.pid, e.created);
            let path = self.paths.remove(&key).unwrap_or_else(|| if e.pid == 0 { None } else { image_path(e.pid) });
            e.path = path.clone();
            paths.insert(key, path);
        }
        self.paths = paths; // what has exited is forgotten
        Ok(list)
    }
}

/// One snapshot, with every path looked up.
pub fn snapshot() -> Result<Vec<ProcEntry>, ProcError> {
    Snapshotter::default().take()
}

/// The processes without their paths.
fn read() -> Result<Vec<ProcEntry>, ProcError> {
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
            handles: u32::from_le_bytes(e[96..100].try_into().unwrap()),
            path: None,
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
    fn the_handle_count_follows_the_handles_a_process_opens() {
        let me = std::process::id();
        let count = || snapshot().unwrap().iter().find(|p| p.pid == me).expect("own pid listed").handles;
        let before = count();
        assert!(before > 0, "a process always holds handles");
        let dir = std::env::temp_dir().join(format!("plumb-handles-{me}"));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::fs::File> = (0..20).map(|i| std::fs::File::create(dir.join(format!("f{i}"))).unwrap()).collect();
        let during = count();
        drop(files);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(during >= before + 20, "{during} while 20 more files were open, {before} before");
    }

    #[test]
    fn our_own_process_is_listed_with_a_plausible_working_set() {
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 3 127.0.0.1 >nul"]).spawn().unwrap();
        // Plausible, not exact: the snapshot's own buffer (several MB, freed before it returns) sits in the working set for
        // part of the call, so the listed value can lie either side of a reading taken around it.
        let mut pmc = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb).unwrap() };
        let before = pmc.WorkingSetSize as f64;
        let list = snapshot().unwrap();
        let me = std::process::id();
        let e = list.iter().find(|p| p.pid == me).expect("own pid listed");
        let got = e.working_set as f64;
        assert!(got >= before * 0.5 && got <= before + (32 << 20) as f64, "{got} vs {before} before the call");
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

#[cfg(test)]
mod cost {
    use super::*;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn cpu() -> std::time::Duration {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32 | u64::from(f.dwLowDateTime)) * 100;
        std::time::Duration::from_nanos(t(k) + t(u))
    }

    /// Prints what a snapshot costs in CPU: `cargo test -p plumb-scan procs::cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn what_a_snapshot_costs() {
        let mut shots = Snapshotter::default();
        let first = shots.take().unwrap();
        let n = 100;
        let c0 = cpu();
        for _ in 0..n {
            shots.take().unwrap();
        }
        let take = (cpu() - c0) / n;
        let c0 = cpu();
        for _ in 0..n {
            crate::nt::query(SYSTEM_PROCESS_INFORMATION, 1 << 20).unwrap();
        }
        let raw = (cpu() - c0) / n;
        let c0 = cpu();
        for _ in 0..n {
            read().unwrap();
        }
        let parse = (cpu() - c0) / n;
        eprintln!("PROCS {} processes; CPU per snapshot: take {take:?}, read {parse:?}, raw query {raw:?}", first.len());
    }
}
