//! A snapshot of the running processes: `NtQuerySystemInformation(SystemProcessInformation)`, parsed by x64 offset.
use crate::reason::Reason;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
};

const SYSTEM_PROCESS_INFORMATION: u32 = 5;
/// Where a process's threads start in its entry, and the size of each: `SYSTEM_PROCESS_INFORMATION` and
/// `SYSTEM_THREAD_INFORMATION` on x64.
const THREADS_AT: usize = 256;
const THREAD_LEN: usize = 80;
/// `ThreadState` Waiting and `WaitReason` Suspended: how a suspended thread reads (the Task Manager's test).
const WAITING: u32 = 5;
const SUSPENDED: u32 = 5;

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
    /// Every thread is suspended (and there is at least one).
    pub suspended: bool,
    /// Runs with an elevated (administrator) token; false too when the token cannot be read.
    pub elevated: bool,
    /// `None` when the process cannot be opened (another user's, protected).
    pub path: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProcError(pub i32);

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

pub(crate) fn image_path(pid: u32) -> Option<String> {
    identity(pid).0
}

/// What does not change while a process runs: its executable's path (`None` when it cannot be opened) and whether its
/// token is elevated.
fn identity(pid: u32) -> (Option<String>, bool) {
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { return (None, false) };
    let mut buf = vec![0u16; 32768]; // the longest path Windows allows
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut n) }.is_ok();
    let elevated = crate::volume::process_elevated(h);
    let _ = unsafe { CloseHandle(h) };
    (ok.then(|| String::from_utf16_lossy(&buf[..n as usize])), elevated)
}

fn why(e: windows::core::Error) -> Reason {
    if e.code().0 == 0x8007_0005_u32 as i32 { Reason::AccessDenied } else { Reason::other(e.message()) }
}

/// Runs `act` on the process `pid` opened with `access`, if it was created at `created` (the FILETIME from its entry). A
/// pid is reused, so a process created at another time is left alone. The error is the reason, in words.
fn with_process(pid: u32, created: u64, access: PROCESS_ACCESS_RIGHTS, act: impl FnOnce(HANDLE) -> Result<(), Reason>) -> Result<(), Reason> {
    let h = unsafe { OpenProcess(access | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(|e| if e.code().0 == 0x8007_0057_u32 as i32 { Reason::other("it has already ended") } else { why(e) })?;
    let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let started = unsafe { GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u) }.map(|_| (c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64);
    let out = match started {
        Ok(t) if t == created => act(h),
        Ok(_) => Err(Reason::other("the pid now belongs to a different process")),
        Err(e) => Err(why(e)),
    };
    let _ = unsafe { CloseHandle(h) };
    out
}

/// Ends the process `pid` created at `created`, with exit code 1.
pub fn terminate(pid: u32, created: u64) -> Result<(), Reason> {
    with_process(pid, created, PROCESS_TERMINATE, |h| unsafe { TerminateProcess(h, 1) }.map_err(why))
}

/// Suspends the process `pid` created at `created` (or, with `resume`, resumes it). The system counts suspensions, so a
/// process suspended twice needs two resumes.
pub fn pause(pid: u32, created: u64, resume: bool) -> Result<(), Reason> {
    with_process(pid, created, PROCESS_SUSPEND_RESUME, |h| {
        crate::nt::pause_process(h, resume).map_err(|s| if s == 0xC000_0022_u32 as i32 { Reason::AccessDenied } else { Reason::other(format!("the system refused (status {s:#010x})")) })
    })
}

/// Takes snapshots, remembering each process's path (and that it could not be had) and elevation so the lookup, which
/// opens the process, is made once per process and not once per snapshot.
#[derive(Default)]
pub struct Snapshotter {
    known: std::collections::HashMap<(u32, u64), (Option<String>, bool)>,
}

impl Snapshotter {
    pub fn take(&mut self) -> Result<Vec<ProcEntry>, ProcError> {
        let mut list = read()?;
        let mut known = std::collections::HashMap::with_capacity(list.len());
        for e in &mut list {
            let key = (e.pid, e.created);
            let id = self.known.remove(&key).unwrap_or_else(|| if e.pid == 0 { (None, false) } else { identity(e.pid) });
            (e.path, e.elevated) = id.clone();
            known.insert(key, id);
        }
        self.known = known; // what has exited is forgotten
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
        let threads = u32::from_le_bytes(e[4..8].try_into().unwrap()) as usize;
        let thread_u32 = |i: usize, off: usize| u32::from_le_bytes(e[THREADS_AT + i * THREAD_LEN + off..][..4].try_into().unwrap());
        let suspended = threads > 0 && (0..threads).all(|i| thread_u32(i, 68) == WAITING && thread_u32(i, 72) == SUSPENDED);
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
            suspended,
            elevated: false,
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
    fn terminate_ends_the_process_it_was_given_and_refuses_a_pid_created_at_another_time() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let created = snapshot().unwrap().iter().find(|p| p.pid == child.id()).expect("child listed").created;
        let err = terminate(child.id(), created + 1).unwrap_err();
        assert!(err.to_string().contains("different process"), "{err}");
        assert!(child.try_wait().unwrap().is_none(), "a mismatch kills nothing");
        terminate(child.id(), created).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(1));
        assert!(terminate(child.id(), created).is_err(), "an ended process cannot be ended again");
    }

    #[test]
    fn a_suspended_process_reads_as_suspended_until_it_is_resumed_and_a_stale_pid_is_refused() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let state = || snapshot().unwrap().into_iter().find(|p| p.pid == child.id()).expect("child listed");
        let created = state().created;
        assert!(!state().suspended, "a running process is not suspended");
        assert_eq!(state().elevated, crate::volume::is_elevated(), "a child runs with its parent's token");
        assert!(pause(child.id(), created + 1, false).unwrap_err().to_string().contains("different process"));
        pause(child.id(), created, false).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state().suspended && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(state().suspended, "every thread waits as suspended");
        pause(child.id(), created, true).unwrap();
        assert!(!state().suspended, "resumed");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_handle_count_follows_the_handles_a_process_opens() {
        let me = std::process::id();
        let count = || snapshot().unwrap().iter().find(|p| p.pid == me).expect("own pid listed").handles;
        let before = count();
        assert!(before > 0, "a process always holds handles");
        let dir = std::env::temp_dir().join(format!("plumb-handles-{me}"));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::fs::File> = (0..200).map(|i| std::fs::File::create(dir.join(format!("f{i}"))).unwrap()).collect();
        let during = count();
        drop(files);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(during >= before + 100, "{during} while 200 more files were open, {before} before");
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
