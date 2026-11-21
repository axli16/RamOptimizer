//! Tray icon, its menu, and the HKCU Run-key autostart.

use crate::actions::wide;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, MF_STRING, SetForegroundWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
};

pub const WM_TRAY: u32 = WM_APP + 1;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE: &str = "RamOptimizer";

#[derive(Debug, PartialEq)]
pub enum MenuCmd {
    Pause,
    OpenConfig,
    Quit,
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

pub fn add(hwnd: HWND) {
    let mut d = icon_data(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    // Resource id 1 = res/app.ico (MAKEINTRESOURCE(1)).
    d.hIcon = unsafe { LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _) };
    let tip = wide("RamOptimizer");
    d.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &d) };
}

pub fn remove(hwnd: HWND) {
    let d = icon_data(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
}

pub fn show_menu(hwnd: HWND) -> Option<MenuCmd> {
    unsafe {
        let m = CreatePopupMenu();
        for (id, label) in [(1usize, "Pause 1 hr"), (2, "Open config"), (3, "Quit")] {
            AppendMenuW(m, MF_STRING, id, wide(label).as_ptr());
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd); // required, or the menu never closes when clicking away
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        DestroyMenu(m);
        match cmd {
            1 => Some(MenuCmd::Pause),
            2 => Some(MenuCmd::OpenConfig),
            3 => Some(MenuCmd::Quit),
            _ => None,
        }
    }
}

/// Points the Run value at this exe; writes only when missing or different.
pub fn ensure_autostart() {
    let Ok(exe) = std::env::current_exe() else { return };
    let want = format!("\"{}\"", exe.display());
    let (key, val) = (wide(RUN_KEY), wide(VALUE));
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(),
            buf.as_mut_ptr().cast(), &mut size)
    };
    let have = (rc == 0).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]));
    if have.as_deref() != Some(want.as_str()) {
        let data = wide(&want);
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), REG_SZ, data.as_ptr().cast(),
                (data.len() * 2) as u32)
        };
    }
}
