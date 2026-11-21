//! Custom dark popup in the notification corner (bottom-right of the work area).
//! Windows 11 notification look, GDI only. `show` runs a small message loop until answered, like a
//! dialog, so callers keep a simple return value while the app's timer keeps ticking.

use crate::actions::wide;
use std::cell::RefCell;
use std::sync::Once;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_RETURN};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Close,
    Button(usize),
}

/// Window in screen coordinates; everything else in client coordinates.
#[derive(Clone, Debug)]
pub struct Layout {
    pub win: Rect,
    pub close: Rect,
    pub header: Rect,
    pub title: Rect,
    pub body: Vec<Rect>,
    pub buttons: Vec<Rect>,
    pub icon: i32,
}

// Sizes at 96 DPI, matching the Windows 11 notification card.
const W: f32 = 364.0;
const PAD: f32 = 16.0;
const HEADER: f32 = 20.0;
const GAP: f32 = 10.0;
const TITLE: f32 = 20.0;
const LINE: f32 = 18.0;
const BTN_GAP_TOP: f32 = 16.0;
const BTN: f32 = 32.0;
const BTN_GAP: f32 = 8.0;
const MARGIN: f32 = 12.0;
const CLOSE: f32 = 28.0;

pub fn layout(work: Rect, scale: f32, n_buttons: usize, body_lines: usize) -> Layout {
    let px = |v: f32| (v * scale).round() as i32;
    let base_h = PAD + HEADER + GAP + TITLE + body_lines as f32 * LINE + BTN_GAP_TOP + BTN + PAD;
    let (w, h) = (px(W), px(base_h));
    let (pad, margin) = (px(PAD), px(MARGIN));
    let win = Rect { l: work.r - margin - w, t: work.b - margin - h, r: work.r - margin, b: work.b - margin };
    let header = Rect { l: pad, t: pad, r: w - pad - px(CLOSE), b: pad + px(HEADER) };
    let close = Rect { l: w - px(8.0) - px(CLOSE), t: px(6.0), r: w - px(8.0), b: px(6.0) + px(CLOSE) };
    let title_t = header.b + px(GAP);
    let title = Rect { l: pad, t: title_t, r: w - pad, b: title_t + px(TITLE) };
    let body = (0..body_lines)
        .map(|i| {
            let t = title.b + px(i as f32 * LINE);
            Rect { l: pad, t, r: w - pad, b: t + px(LINE) }
        })
        .collect();
    let n = n_buttons.max(1) as i32;
    let gap = px(BTN_GAP);
    let bw = (w - 2 * pad - gap * (n - 1)) / n;
    let bt = h - pad - px(BTN);
    let buttons = (0..n)
        .map(|i| {
            let l = pad + i * (bw + gap);
            let r = if i == n - 1 { w - pad } else { l + bw };
            Rect { l, t: bt, r, b: bt + px(BTN) }
        })
        .take(n_buttons)
        .collect();
    Layout { win, close, header, title, body, buttons, icon: px(16.0) }
}

pub fn hit(l: &Layout, x: i32, y: i32) -> Option<Hit> {
    if l.close.contains(x, y) {
        return Some(Hit::Close);
    }
    l.buttons.iter().position(|b| b.contains(x, y)).map(Hit::Button)
}

// Windows 11 dark palette (COLORREF = 0x00BBGGRR).
const BG: u32 = 0x2B2B2B;
const BORDER: u32 = 0x3D3D3D;
const TEXT: u32 = 0xFFFFFF;
const TEXT_2: u32 = 0xC5C5C5;
const BTN_BG: u32 = 0x3A3A3A;
const BTN_HOVER: u32 = 0x454545;
const ACCENT: u32 = 0xFFC24C; // #4CC2FF
const ACCENT_HOVER: u32 = 0xFFC85A;
const ACCENT_TEXT: u32 = 0x000000;
const AUTO_HIDE_TIMER: usize = 7;
const WM_MOUSELEAVE: u32 = 0x02A3; // not exported by the windows-sys modules imported here

struct State {
    layout: Layout,
    scale: f32,
    title: String,
    body: Vec<String>,
    buttons: Vec<String>,
    hover: Option<Hit>,
    result: Option<Option<usize>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn finish(choice: Option<usize>) {
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.result.get_or_insert(choice);
        }
    });
}

unsafe fn font(px: i32, weight: i32, face: &str) -> HFONT {
    let f = wide(face);
    unsafe {
        CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32, DEFAULT_PITCH as u32, f.as_ptr())
    }
}

unsafe fn text(hdc: HDC, s: &str, r: Rect, color: u32, flags: u32) {
    if s.is_empty() {
        return; // an empty Vec has a dangling pointer, which DrawTextW may still read
    }
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut rc = RECT { left: r.l, top: r.t, right: r.r, bottom: r.b };
    unsafe {
        SetTextColor(hdc, color);
        DrawTextW(hdc, w.as_ptr(), w.len() as i32, &mut rc, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    }
}

unsafe fn fill_round(hdc: HDC, r: Rect, color: u32, radius: i32) {
    unsafe {
        let b = CreateSolidBrush(color);
        let old_b = SelectObject(hdc, b);
        let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
        RoundRect(hdc, r.l, r.t, r.r + 1, r.b + 1, radius, radius);
        SelectObject(hdc, old_p);
        SelectObject(hdc, old_b);
        DeleteObject(b);
    }
}

/// Paints the whole card into `target` (double-buffered to avoid flicker).
unsafe fn paint(target: HDC) {
    STATE.with(|st| {
        let st = st.borrow();
        let Some(s) = st.as_ref() else { return };
        let l = &s.layout;
        let (w, h) = (l.win.r - l.win.l, l.win.b - l.win.t);
        let px = |v: f32| (v * s.scale).round() as i32;
        unsafe {
            let hdc = CreateCompatibleDC(target);
            let bmp = CreateCompatibleBitmap(target, w, h);
            let old_bmp = SelectObject(hdc, bmp);
            let bg = CreateSolidBrush(BG);
            FillRect(hdc, &RECT { left: 0, top: 0, right: w, bottom: h }, bg);
            DeleteObject(bg);
            SetBkMode(hdc, TRANSPARENT as i32);

            let small = font(px(12.0), 400, "Segoe UI");
            let bold = font(px(14.0), 600, "Segoe UI");
            let glyph = font(px(10.0), 400, "Segoe MDL2 Assets");

            // Header: app icon + name.
            let icon = LoadImageW(GetModuleHandleW(std::ptr::null()), 1 as _, IMAGE_ICON, l.icon, l.icon, 0);
            if !icon.is_null() {
                DrawIconEx(hdc, l.header.l, l.header.t + (l.header.b - l.header.t - l.icon) / 2, icon as HICON,
                    l.icon, l.icon, 0, std::ptr::null_mut(), DI_NORMAL);
                DestroyIcon(icon as HICON);
            }
            SelectObject(hdc, small);
            let name = Rect { l: l.header.l + l.icon + px(8.0), ..l.header };
            text(hdc, "RamOptimizer", name, TEXT_2, DT_LEFT);

            // Close (✕): Segoe MDL2 "ChromeClose" glyph.
            if s.hover == Some(Hit::Close) {
                fill_round(hdc, l.close, BTN_BG, px(8.0));
            }
            SelectObject(hdc, glyph);
            text(hdc, "\u{E8BB}", l.close, TEXT_2, DT_CENTER);

            SelectObject(hdc, bold);
            text(hdc, &s.title, l.title, TEXT, DT_LEFT | DT_END_ELLIPSIS);

            // Body; a tab splits a line into a left column and a right-aligned column.
            SelectObject(hdc, small);
            for (line, r) in s.body.iter().zip(&l.body) {
                let (left, right) = line.split_once('\t').unwrap_or((line, ""));
                text(hdc, left, *r, TEXT_2, DT_LEFT | DT_END_ELLIPSIS);
                text(hdc, right, *r, TEXT_2, DT_RIGHT);
            }

            for (i, (label, r)) in s.buttons.iter().zip(&l.buttons).enumerate() {
                let hover = s.hover == Some(Hit::Button(i));
                let (bg, fg) = match (i, hover) {
                    (0, false) => (ACCENT, ACCENT_TEXT),
                    (0, true) => (ACCENT_HOVER, ACCENT_TEXT),
                    (_, false) => (BTN_BG, TEXT),
                    (_, true) => (BTN_HOVER, TEXT),
                };
                fill_round(hdc, *r, bg, px(8.0));
                text(hdc, label, *r, fg, DT_CENTER | DT_END_ELLIPSIS);
            }

            BitBlt(target, 0, 0, w, h, hdc, 0, 0, SRCCOPY);
            SelectObject(hdc, old_bmp);
            for o in [bmp as HGDIOBJ, small as HGDIOBJ, bold as HGDIOBJ, glyph as HGDIOBJ] {
                DeleteObject(o);
            }
            DeleteDC(hdc);
        }
    });
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn toast_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(h, &mut ps);
            paint(hdc);
            EndPaint(h, &ps);
        },
        WM_PRINTCLIENT => unsafe { paint(wp as HDC) }, // used by AnimateWindow's slide-in
        WM_ERASEBKGND => return 1,
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            let changed = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let Some(s) = s.as_mut() else { return false };
                let new = hit(&s.layout, x, y);
                std::mem::replace(&mut s.hover, new) != new
            });
            unsafe {
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
                TrackMouseEvent(&mut t);
                if changed {
                    InvalidateRect(h, std::ptr::null(), 0);
                }
            }
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| s.borrow_mut().as_mut().map(|s| s.hover = None));
            unsafe { InvalidateRect(h, std::ptr::null(), 0) };
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            match STATE.with(|s| s.borrow().as_ref().and_then(|s| hit(&s.layout, x, y))) {
                Some(Hit::Close) => finish(None),
                Some(Hit::Button(i)) => finish(Some(i)),
                None => {}
            }
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => finish(None),
        WM_KEYDOWN if wp == VK_RETURN as usize => finish(Some(0)),
        WM_TIMER if wp == AUTO_HIDE_TIMER => finish(None),
        _ => return unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
    0
}

const CLASS: &str = "RamOptimizerToast";

fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let class = wide(CLASS);
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(toast_proc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = class.as_ptr();
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        RegisterClassW(&wc);
    });
}

unsafe fn dwm_u32(h: HWND, attr: i32, v: u32) {
    unsafe { DwmSetWindowAttribute(h, attr as u32, (&v as *const u32).cast(), 4) };
}

/// Shows the card and waits for a click. Returns the button index; None for ✕, Esc or auto-hide.
pub fn show(title: &str, body: &str, buttons: &[&str], auto_hide_ms: Option<u32>) -> Option<usize> {
    register();
    let mut wa = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut wa as *mut RECT).cast(), 0) };
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let body: Vec<String> = body.lines().filter(|l| !l.is_empty()).map(String::from).collect();
    let l = layout(Rect { l: wa.left, t: wa.top, r: wa.right, b: wa.bottom }, scale, buttons.len(), body.len());
    let win = l.win;
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            layout: l,
            scale,
            title: title.into(),
            body,
            buttons: buttons.iter().map(|b| b.to_string()).collect(),
            hover: None,
            result: None,
        })
    });

    let class = wide(CLASS);
    let h = unsafe {
        CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, class.as_ptr(), class.as_ptr(), WS_POPUP,
            win.l, win.t, win.r - win.l, win.b - win.t, std::ptr::null_mut(), std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()), std::ptr::null())
    };
    if h.is_null() {
        STATE.with(|s| *s.borrow_mut() = None);
        return None;
    }
    unsafe {
        dwm_u32(h, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
        dwm_u32(h, DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
        dwm_u32(h, DWMWA_BORDER_COLOR, BORDER);
        if let Some(ms) = auto_hide_ms {
            SetTimer(h, AUTO_HIDE_TIMER, ms, None);
        }
        if AnimateWindow(h, 180, AW_SLIDE | AW_VER_NEGATIVE) == 0 {
            ShowWindow(h, SW_SHOWNOACTIVATE);
        }
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    let result = loop {
        if let Some(r) = STATE.with(|s| s.borrow().as_ref().and_then(|s| s.result)) {
            break r;
        }
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            // App is quitting (tray Quit while the card is up): pass WM_QUIT on to the main loop.
            unsafe { PostQuitMessage(msg.wParam as i32) };
            break None;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    };
    unsafe { DestroyWindow(h) };
    STATE.with(|s| *s.borrow_mut() = None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect { l: 0, t: 0, r: 1920, b: 1032 }; // 1080p minus a 48 px taskbar

    #[test]
    fn sits_bottom_right_with_margin() {
        let l = layout(WORK, 1.0, 4, 1);
        assert_eq!(l.win.r, 1920 - 12);
        assert_eq!(l.win.b, 1032 - 12);
        assert_eq!(l.win.r - l.win.l, 364);
    }

    #[test]
    fn scales_with_dpi() {
        let a = layout(WORK, 1.0, 2, 0);
        let b = layout(WORK, 1.5, 2, 0);
        assert_eq!(b.win.r - b.win.l, 546);
        assert_eq!(b.win.b - b.win.t, ((a.win.b - a.win.t) as f32 * 1.5).round() as i32);
    }

    #[test]
    fn grows_with_body_lines() {
        let a = layout(WORK, 1.0, 1, 0);
        let b = layout(WORK, 1.0, 1, 5);
        assert_eq!((b.win.b - b.win.t) - (a.win.b - a.win.t), 5 * 18);
    }

    #[test]
    fn buttons_fill_row_without_overlap() {
        let l = layout(WORK, 1.0, 4, 1);
        assert_eq!(l.buttons.len(), 4);
        assert_eq!(l.buttons[0].l, 16);
        assert_eq!(l.buttons[3].r, 364 - 16);
        for w in l.buttons.windows(2) {
            assert!(w[0].r < w[1].l);
            assert_eq!(w[0].t, w[1].t);
        }
    }

    #[test]
    fn hit_testing() {
        let l = layout(WORK, 1.0, 2, 0);
        let b = l.buttons[1];
        assert_eq!(hit(&l, (b.l + b.r) / 2, (b.t + b.b) / 2), Some(Hit::Button(1)));
        let c = l.close;
        assert_eq!(hit(&l, c.l + 2, c.t + 2), Some(Hit::Close));
        assert_eq!(hit(&l, 2, 2), None);
    }

    /// Shows a real card for 1.5 s. Run: cargo test --release -- --ignored toast_smoke
    #[test]
    #[ignore]
    fn toast_smoke() {
        assert_eq!(show("Smoke test", "line one\nleft\tright", &["OK", "Other"], Some(1500)), None);
    }
}
