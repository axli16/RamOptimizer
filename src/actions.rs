//! Win32 actions: trim, checked terminate, polite close, popups.

use crate::procs::{is_user_visible, window_pid};
use std::collections::HashSet;
use crate::toast;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, PostMessageW, WM_CLOSE};
use windows_sys::core::BOOL;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn trim(pid: u32) -> bool {
    let h = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let ok = unsafe { K32EmptyWorkingSet(h) } != 0;
    unsafe { CloseHandle(h) };
    ok
}

/// Re-checks the exe name on the open handle so a reused PID is never killed.
pub fn terminate_checked(pid: u32, expected_exe: &str) -> bool {
    let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let named = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit('\\').next().unwrap_or("");
    let ok = named && file.eq_ignore_ascii_case(expected_exe) && unsafe { TerminateProcess(h, 1) } != 0;
    unsafe { CloseHandle(h) };
    ok
}

unsafe extern "system" fn close_cb(h: HWND, l: LPARAM) -> BOOL {
    let pids = unsafe { &*(l as *const HashSet<u32>) };
    if is_user_visible(h) && pids.contains(&window_pid(h)) {
        unsafe { PostMessageW(h, WM_CLOSE, 0, 0) };
    }
    1
}

pub fn close_windows(pids: &HashSet<u32>) {
    unsafe { EnumWindows(Some(close_cb), pids as *const _ as LPARAM) };
}

/// Shows the notification-style card (see toast.rs). Returns the clicked index; None on ✕/Esc.
fn dialog(_owner: HWND, text: &str, content: &str, buttons: &[&str]) -> Option<usize> {
    toast::show(text, content, buttons, None)
}

#[derive(Debug, PartialEq)]
pub enum AppChoice {
    Kill,
    NotNow,
    Always,
    Never,
}

#[derive(Debug, PartialEq)]
pub enum IdleChoice {
    Close,
    NotNow,
    Snooze,
}

pub fn ask_app(owner: HWND, name: &str, mb: u64) -> AppChoice {
    match dialog(owner, &format!("{name} is closed but using {mb} MB."), "Free it now?", &["Kill", "Not now", "Always kill", "Never ask"]) {
        Some(0) => AppChoice::Kill,
        Some(2) => AppChoice::Always,
        Some(3) => AppChoice::Never,
        _ => AppChoice::NotNow,
    }
}

pub fn ask_idle(owner: HWND, name: &str, minutes: u64, mb: u64) -> IdleChoice {
    let gb = mb as f64 / 1024.0;
    let text = format!("{name} idle for {minutes} min");
    let detail = format!("IDE + VM using {gb:.1} GB. Close it to free the memory?");
    match dialog(owner, &text, &detail, &["Close it", "Not now", "Snooze 1 hr"]) {
        Some(0) => IdleChoice::Close,
        Some(2) => IdleChoice::Snooze,
        _ => IdleChoice::NotNow,
    }
}

pub fn ask_force(owner: HWND, name: &str) -> bool {
    dialog(owner, &format!("{name} did not close. Force kill it?"), "", &["Force kill", "Leave it"]) == Some(0)
}

pub fn show_summary(owner: HWND, head: &str, body: &str) {
    let _ = owner;
    toast::show(head, body, &["OK"], Some(30_000)); // summary auto-hides after 30 s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_self_ok() {
        assert!(trim(std::process::id()));
    }

    #[test]
    fn terminate_skips_name_mismatch() {
        let mut c = std::process::Command::new("cmd").args(["/c", "ping", "-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        assert!(!terminate_checked(c.id(), "notepad.exe"));
        assert!(c.try_wait().unwrap().is_none()); // still alive
        assert!(terminate_checked(c.id(), "CMD.EXE"));
        assert!(c.wait().is_ok());
    }

    #[test]
    fn terminate_dead_pid_false() {
        assert!(!terminate_checked(0xFFFF_FFF0, "x.exe"));
    }
}
