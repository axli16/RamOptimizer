//! Gathers facts from Win32: processes, visible windows, foreground app, memory load.

use crate::policy::{Facts, Proc};
use std::collections::HashSet;
use std::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsWindowVisible,
};
use windows_sys::core::BOOL;

pub const DWM_CLOAKED_APP: u32 = 0x1;
pub const DWM_CLOAKED_SHELL: u32 = 0x2;
pub const DWM_CLOAKED_INHERITED: u32 = 0x4;

/// Shell-cloaked alone means "on another virtual desktop", which still counts as open.
pub fn cloak_hides(cloaked: u32) -> bool {
    cloaked & (DWM_CLOAKED_APP | DWM_CLOAKED_INHERITED) != 0
}

pub fn mem_load() -> u32 {
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return 0;
    }
    m.dwMemoryLoad
}

fn session_of(pid: u32) -> Option<u32> {
    let mut s = 0u32;
    (unsafe { ProcessIdToSessionId(pid, &mut s) } != 0).then_some(s)
}

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

/// (working set bytes, kernel+user CPU in 100 ns); zeros when access is denied.
fn usage(pid: u32) -> (u64, u64) {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return (0, 0);
    }
    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ws = if unsafe { K32GetProcessMemoryInfo(h, &mut pmc, pmc.cb) } != 0 { pmc.WorkingSetSize as u64 } else { 0 };
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    let cpu = if unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0 { ft(k) + ft(u) } else { 0 };
    unsafe { CloseHandle(h) };
    (ws, cpu)
}

pub fn processes() -> Vec<Proc> {
    let own = session_of(std::process::id());
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut ok = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while ok {
        let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
        let pid = e.th32ProcessID;
        if pid != 0 {
            let (ws_bytes, cpu_100ns) = usage(pid);
            out.push(Proc {
                pid,
                exe: String::from_utf16_lossy(&e.szExeFile[..n]),
                ws_bytes,
                cpu_100ns,
                same_session: own.is_some() && session_of(pid) == own,
            });
        }
        ok = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };
    out
}

pub fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    pid
}

/// Visible, unowned, not app-cloaked top-level window.
pub fn is_user_visible(h: HWND) -> bool {
    if unsafe { IsWindowVisible(h) } == 0 || !unsafe { GetWindow(h, GW_OWNER) }.is_null() {
        return false;
    }
    let mut cloaked = 0u32;
    let hr = unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), size_of::<u32>() as u32)
    };
    hr != 0 || !cloak_hides(cloaked)
}

unsafe extern "system" fn collect_visible(h: HWND, l: LPARAM) -> BOOL {
    let set = unsafe { &mut *(l as *mut HashSet<u32>) };
    if is_user_visible(h) {
        set.insert(window_pid(h));
    }
    1
}

pub fn visible_pids() -> HashSet<u32> {
    let mut set = HashSet::new();
    unsafe { EnumWindows(Some(collect_visible), &mut set as *mut _ as LPARAM) };
    set
}

pub fn gather(now: Instant) -> Facts {
    let fg = unsafe { GetForegroundWindow() };
    Facts {
        now,
        mem_load: mem_load(),
        procs: processes(),
        visible_pids: visible_pids(),
        foreground_pid: if fg.is_null() { 0 } else { window_pid(fg) },
        self_pid: std::process::id(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn gather_sees_self() {
        let f = gather(Instant::now());
        let me = f.procs.iter().find(|p| p.pid == std::process::id()).unwrap();
        assert!(me.ws_bytes > 0 && me.same_session);
        assert!(f.mem_load > 0 && f.mem_load <= 100);
        assert_eq!(f.self_pid, std::process::id());
    }

    #[test]
    fn cloak_shell_counts_visible() {
        assert!(!cloak_hides(DWM_CLOAKED_SHELL));
        assert!(cloak_hides(DWM_CLOAKED_APP));
        assert!(cloak_hides(DWM_CLOAKED_INHERITED));
        assert!(!cloak_hides(0));
    }
}
