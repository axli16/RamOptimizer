#![windows_subsystem = "windows"]

mod actions;
mod config;
mod log;
mod policy;
mod procs;
mod stats;
mod toast;
mod tray;

use config::{Config, Decision};
use policy::{Action, Facts, Planner, exe_in};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SW_SHOWNORMAL, SetTimer, TranslateMessage, WM_DESTROY, WM_LBUTTONUP,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

const MB: u64 = 1024 * 1024;

struct App {
    hwnd: HWND,
    dir: PathBuf,
    cfg: Config,
    cfg_mtime: Option<SystemTime>,
    planner: Planner,
    popup_open: bool,
    paused_until: Option<Instant>,
    pending_close: Option<(usize, Instant)>,
    taskbar_msg: u32,
    stats: stats::Stats,
    stats_dirty: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Short borrows only: popups run a nested message loop that re-enters the window procedure.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|a| f(a.borrow_mut().as_mut().expect("app initialised")))
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl App {
    fn cfg_path(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    fn log(&self, msg: &str) {
        log::log_line(&self.dir, msg);
    }

    /// Picks up hand edits; a broken file keeps the last good config.
    fn reload_if_changed(&mut self) {
        let m = mtime(&self.cfg_path());
        if m == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = m;
        let (cfg, err) = config::load_or_create(&self.cfg_path());
        match err {
            Some(e) => self.log(&format!("config error: {e}; keeping previous config")),
            None => {
                if cfg.poll_seconds != self.cfg.poll_seconds {
                    unsafe { SetTimer(self.hwnd, 1, cfg.poll_seconds.max(1) * 1000, None) };
                }
                self.cfg = cfg;
            }
        }
    }

    fn record_close(&mut self, name: &str, mb: u64) {
        if mb > 0 {
            self.stats.record_close(name, mb);
            self.stats_dirty = true;
        }
    }

    fn save_stats(&mut self) {
        if let Err(e) = stats::save(&self.dir.join("stats.toml"), &self.stats) {
            self.log(&format!("stats save failed: {e}"));
        }
        self.stats_dirty = false;
    }

    fn save_decision(&mut self, idx: usize, d: Decision) {
        self.cfg.apps[idx].decision = d;
        if let Err(e) = config::save(&self.cfg_path(), &self.cfg) {
            self.log(&format!("config save failed: {e}"));
        }
        self.cfg_mtime = mtime(&self.cfg_path());
    }
}

fn app_name(cfg: &Config, idx: usize) -> String {
    cfg.apps[idx].display_name()
}

/// Kills every live process of the given exe list (fresh snapshot, name re-checked per PID). Returns MB freed.
fn kill_exes(exes: &[String]) -> u64 {
    let mut freed = 0;
    for p in procs::processes().iter().filter(|p| exe_in(exes, &p.exe)) {
        if actions::terminate_checked(p.pid, &p.exe) {
            freed += p.ws_bytes;
        }
    }
    freed / MB
}

fn alive(exes: &[String]) -> HashSet<u32> {
    procs::processes().into_iter().filter(|p| exe_in(exes, &p.exe)).map(|p| p.pid).collect()
}

fn popup<R>(f: impl FnOnce(HWND) -> R) -> R {
    let hwnd = with(|a| {
        a.popup_open = true;
        a.hwnd
    });
    let r = f(hwnd);
    with(|a| a.popup_open = false);
    r
}

fn tick() {
    let now = Instant::now();
    let (actions, facts, cfg) = with(|a| {
        a.reload_if_changed();
        let facts: Facts = procs::gather(now);
        let paused = a.paused_until.is_some_and(|t| now < t);
        let actions = a.planner.plan(&a.cfg, &facts, a.popup_open, paused);
        (actions, facts, a.cfg.clone())
    });

    for act in actions {
        match act {
            Action::Trim(pid) => {
                if actions::trim(pid) {
                    let p = facts.procs.iter().find(|p| p.pid == pid);
                    let (exe, mb) = p.map_or(("?", 0), |p| (p.exe.as_str(), p.ws_bytes / MB));
                    with(|a| {
                        a.planner.note_trimmed(pid, now);
                        a.stats.record_trim(mb);
                        a.stats_dirty = true;
                        a.log(&format!("trim {exe} {pid} {mb}MB"));
                    });
                }
            }
            Action::KillApp { app_idx } => {
                let mb = kill_exes(&cfg.apps[app_idx].exe);
                let name = app_name(&cfg, app_idx);
                with(|a| {
                    a.log(&format!("kill {name} {mb}MB (always)"));
                    a.record_close(&name, mb);
                });
            }
            Action::PromptApp { app_idx, mb } => {
                let name = app_name(&cfg, app_idx);
                let choice = popup(|h| actions::ask_app(h, &name, mb));
                let kill = |label: &str| {
                    let mb = kill_exes(&cfg.apps[app_idx].exe);
                    with(|a| {
                        a.log(&format!("kill {name} {mb}MB{label}"));
                        a.record_close(&name, mb);
                    });
                };
                match choice {
                    actions::AppChoice::Kill => kill(""),
                    actions::AppChoice::Always => {
                        with(|a| a.save_decision(app_idx, Decision::Always));
                        kill(" (always)");
                    }
                    actions::AppChoice::Never => with(|a| a.save_decision(app_idx, Decision::Never)),
                    actions::AppChoice::NotNow => with(|a| a.planner.not_now_app(app_idx, Instant::now())),
                }
            }
            Action::PromptIdle { group_idx, mb } => {
                let g = &cfg.idle_groups[group_idx];
                let choice = popup(|h| actions::ask_idle(h, &g.name, g.idle_minutes, mb));
                with(|a| match choice {
                    actions::IdleChoice::Close => {
                        actions::close_windows(&alive(&g.exe));
                        a.pending_close = Some((group_idx, Instant::now() + Duration::from_secs(60)));
                        // It may sit on its own "save changes?" dialog: don't re-ask while it closes.
                        a.planner.not_now_group(group_idx, Instant::now());
                        a.log(&format!("close {} {mb}MB", g.name));
                        a.record_close(&g.name, mb); // counted once here, not again on force kill
                    }
                    actions::IdleChoice::NotNow => a.planner.not_now_group(group_idx, Instant::now()),
                    actions::IdleChoice::Snooze => a.planner.snooze_group(group_idx, Instant::now()),
                });
            }
        }
    }

    // Antigravity was asked to close 60 s ago: offer a force kill if it is still running.
    let due = with(|a| match a.pending_close {
        Some((i, t)) if !a.popup_open && Instant::now() >= t => {
            a.pending_close = None;
            Some(i)
        }
        _ => None,
    });
    if let Some(i) = due.filter(|&i| i < cfg.idle_groups.len()) {
        let g = &cfg.idle_groups[i];
        if !alive(&g.exe).is_empty() && popup(|h| actions::ask_force(h, &g.name)) {
            let mb = kill_exes(&g.exe); // group exes only: companions (vmmem) are never killed
            with(|a| a.log(&format!("force kill {} {mb}MB", g.name)));
        }
    }
    weekly_summary();
    actions::trim(std::process::id()); // keeps our own footprint near the 5 MB target
}

fn epoch_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Shows the weekly summary on the first tick after Sunday 18:00 local, then resets the totals.
fn weekly_summary() {
    let now = epoch_now();
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    let anchor = stats::last_anchor(now, t.wDayOfWeek as u64, t.wHour as u64, t.wMinute as u64, t.wSecond as u64);
    let text = with(|a| {
        let first_run = a.stats.last_summary == 0;
        let due = !a.popup_open && a.stats.check_due(now, anchor);
        if first_run {
            a.stats_dirty = true;
        }
        due.then(|| a.stats.summary_text())
    });
    if let Some((head, body)) = text {
        // Reset before showing: a tick inside the dialog's message loop must not show it twice.
        with(|a| {
            a.log(&format!("weekly summary: {head}"));
            a.stats.reset(now);
            a.save_stats();
        });
        popup(|h| actions::show_summary(h, &head, &body));
    }
    with(|a| {
        if a.stats_dirty {
            a.save_stats();
        }
    });
}

fn open_config() {
    let path = with(|a| a.cfg_path());
    let (verb, file, args) = (actions::wide("open"), actions::wide(&path.to_string_lossy()), std::ptr::null());
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), args, std::ptr::null(), SW_SHOWNORMAL) };
    if (r as isize) <= 32 {
        // No app is associated with .toml: fall back to Notepad.
        let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => tick(),
        tray::WM_TRAY if matches!(lp as u32, WM_RBUTTONUP | WM_LBUTTONUP) => match tray::show_menu(hwnd) {
            Some(tray::MenuCmd::Pause) => with(|a| {
                a.paused_until = Some(Instant::now() + Duration::from_secs(3600));
                a.log("paused 1 hr");
            }),
            Some(tray::MenuCmd::OpenConfig) => open_config(),
            Some(tray::MenuCmd::Quit) => {
                tray::remove(hwnd);
                unsafe { DestroyWindow(hwnd) };
            }
            None => {}
        },
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        // Explorer restarted: the tray icon must be re-added.
        m if m != 0 && APP.with(|a| a.borrow().as_ref().is_some_and(|a| a.taskbar_msg == m)) => tray::add(hwnd),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    0
}

fn main() {
    let name = actions::wide("Local\\RamOptimizer");
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    let dir = config::config_dir();
    let (cfg, err) = config::load_or_create(&dir.join("config.toml"));
    if let Some(e) = err {
        log::log_line(&dir, &format!("config error: {e}; using defaults"));
    }

    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = actions::wide("RamOptimizerWnd");
    let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinst;
    wc.lpszClassName = class.as_ptr();
    unsafe { RegisterClassW(&wc) };
    // Hidden top-level window (never shown): tray menus need a window that can take the foreground.
    let hwnd = unsafe {
        CreateWindowExW(0, class.as_ptr(), class.as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0, std::ptr::null_mut(),
            std::ptr::null_mut(), hinst, std::ptr::null())
    };
    if hwnd.is_null() {
        log::log_line(&dir, "failed to create window");
        return;
    }

    let taskbar_msg = unsafe { RegisterWindowMessageW(actions::wide("TaskbarCreated").as_ptr()) };
    let poll = cfg.poll_seconds.max(1);
    let cfg_mtime = mtime(&dir.join("config.toml"));
    let planner = Planner::new(&cfg);
    let stats_path = dir.join("stats.toml");
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            dir,
            cfg,
            cfg_mtime,
            planner,
            popup_open: false,
            paused_until: None,
            pending_close: None,
            taskbar_msg,
            stats: stats::load(&stats_path),
            stats_dirty: false,
        })
    });

    tray::add(hwnd);
    tray::ensure_autostart();
    unsafe { SetTimer(hwnd, 1, poll * 1000, None) };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
