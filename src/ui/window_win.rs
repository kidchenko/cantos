//! The native Windows settings window, in plain Win32 and GDI.
//!
//! Run as its own process (`cantos --settings-window`, see [`super::child`]):
//! it reads the tray's calls from stdin and writes its messages to stdout,
//! in the protocol of [`super::window`].
//!
//! One top-level window draws everything itself. Every control (switch,
//! chip, select, slider, segmented buttons, links) is a rectangle in a
//! [`Layout`] that is hit-tested on mouse input and painted into an
//! off-screen bitmap, so the web page's tokens and shapes carry over exactly and
//! nothing flickers. The only real child windows are the custom command
//! fields, which are `EDIT` controls sitting inside drawn boxes: text editing
//! (caret, selection, IME, clipboard) is not worth rewriting.
//!
//! Layout is worked out in the web page's CSS pixels and multiplied by the
//! window's DPI scale at paint time; the binary is PerMonitorV2, so the scale
//! is per window and changes with `WM_DPICHANGED`.
//!
//! Re-entrancy: Win32 calls such as `SetFocus` and `TrackPopupMenuEx` send
//! messages straight back into the window procedure. The window's state lives
//! in a `RefCell` that is never held across such calls: handlers return
//! [`Fx`] effects that run after the borrow ends, and a message that does
//! arrive mid-borrow goes to `DefWindowProcW`.

use super::child;
use super::d2d_win::D2d;
use crate::actions::CATALOG;
use crate::config::{Action, Config, Modifier, MonitorMode};
use traykit::util::{pcwstr, wide};
use traykit::{lerror, linfo};

use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io::BufRead;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse,
    EndPaint, ExtCreatePen, FillRect, GetDC, GetMonitorInfoW, GetStockObject,
    GetTextExtentPoint32W, GetTextFaceW, GetTextMetricsW, InvalidateRect, MonitorFromWindow,
    Polyline, ReleaseDC, RoundRect, ScreenToClient, SelectClipRgn, SelectObject, SetBkColor,
    SetBkMode, SetTextCharacterExtra, SetTextColor, TextOutW, BS_SOLID, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT, HGDIOBJ, LOGBRUSH,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    PEN_STYLE, PS_ENDCAP_ROUND, PS_ENDCAP_SQUARE, PS_GEOMETRIC, PS_INSIDEFRAME, PS_JOIN_MITER,
    PS_JOIN_ROUND, PS_SOLID, SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_END, VK_F4, VK_HOME, VK_LEFT, VK_NEXT,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, GetWindowTextLengthW,
    GetWindowTextW, IsIconic, KillTimer, LoadCursorW, LoadImageW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenuEx, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, ES_AUTOHSCROLL, GWLP_WNDPROC, HICON, HMENU, HTCLIENT, ICON_BIG, ICON_SMALL,
    IDC_ARROW, IDC_HAND, IDC_IBEAM, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, MINMAXINFO, MSG, SM_CXICON, SM_CXSMICON, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_RESTORE, SW_SHOW, SW_SHOWNA, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_TOPALIGN,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS,
    WM_SETFONT, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
    WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};

const CORNER_NAMES: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
const CORNER_KEYS: [&str; 4] = ["topLeft", "topRight", "bottomLeft", "bottomRight"];
/// The web page's `CORNER_GLYPH`: an L bracket per corner, in a 12-unit box.
const CORNER_GLYPHS: [&str; 4] = ["M1 5V1h4", "M7 1h4v4", "M1 7v4h4", "M11 7v4H7"];

const MODIFIERS: [(&str, &str); 5] = [
    ("none", "Not required"),
    ("ctrl", "Ctrl"),
    ("alt", "Alt"),
    ("shift", "Shift"),
    ("win", "Windows"),
];

const PRESETS: [(&str, [&str; 4]); 3] = [
    ("Recommended", ["taskView", "showDesktop", "none", "none"]),
    ("Minimal", ["taskView", "none", "none", "none"]),
    ("Clear all", ["none", "none", "none", "none"]),
];

const MODES: [(MonitorMode, &str); 3] = [
    (MonitorMode::OuterCorners, "Outer"),
    (MonitorMode::EveryMonitor, "Every screen"),
    (MonitorMode::PrimaryOnly, "Primary"),
];

/// `(min, max, step)` of the Dwell, Corner size and Cooldown ranges.
const SLIDERS: [(u32, u32, u32); 3] = [(0, 1200, 25), (1, 40, 1), (200, 3000, 100)];

const LINKS: [(&str, &str); 3] = [
    ("Log", "openLog"),
    ("Config folder", "openConfigDir"),
    ("Reset", "reset"),
];

const PLACEHOLDERS: [&str; 2] = ["Program, script, document, or URL", "Arguments"];

/// The web page's 16x16 stroke glyph per action, `GLYPH` in the original web page.
fn glyph_for(kind: &str) -> &'static str {
    match kind {
        "taskView" => "M2 3h5v4H2zM9 3h5v4H9zM2 9h5v4H2zM9 9h5v4H9z",
        "showDesktop" => "M2 3h12v7H2zM6 13h4M8 10v3",
        "minimizeAll" => "M3 13h10M8 2.5v6M5.6 6.4L8 8.8l2.4-2.4",
        "desktopLeft" => "M9.6 3.4L5.2 8l4.4 4.6M13 3.4v9.2",
        "desktopRight" => "M6.4 3.4L10.8 8l-4.4 4.6M3 3.4v9.2",
        "startMenu" => "M2.6 4.3l4.4-.8v4.1H2.6zM8.4 3.2l5-.9v5.3h-5zM2.6 8.6H7v4.1l-4.4-.8zM8.4 8.6h5v5.3l-5-.9z",
        "search" => "M7.2 3a4.2 4.2 0 1 0 0 8.4 4.2 4.2 0 0 0 0-8.4M10.3 10.3L13.4 13.4",
        "settings" => "M8 5.9a2.1 2.1 0 1 0 0 4.2 2.1 2.1 0 0 0 0-4.2M8 2.2v1.6M8 12.2v1.6M13.8 8h-1.6M3.8 8H2.2M12.1 3.9l-1.2 1.2M5.1 10.9l-1.2 1.2M12.1 12.1l-1.2-1.2M5.1 5.1L3.9 3.9",
        "quickSettings" => "M3 5.2h10M3 10.8h10M6.2 3.2v4M10 8.8v4",
        "notifications" => "M5 11V7.2a3 3 0 0 1 6 0V11M3.4 11h9.2M6.8 13.2h2.4",
        "quickLink" => "M3.4 4.2h9.2M3.4 8h9.2M3.4 11.8h5.4",
        "runDialog" => "M2.5 3.5h11v9h-11zM4.8 6.6L7 8.4l-2.2 1.8M8.6 10.2h3",
        "fileExplorer" => "M2 4.4h4.2l1.3 2.1H14v7.1H2z",
        "screenshot" => "M3 6.2V3h3.2M13 6.2V3H9.8M3 9.8V13h3.2M13 9.8V13H9.8",
        "clipboard" => "M6 2.8h4v2H6zM4.2 4.2h7.6v9.4H4.2z",
        "emoji" => "M8 2.2a5.8 5.8 0 1 0 0 11.6 5.8 5.8 0 0 0 0-11.6M5.9 6.6h.02M10.1 6.6h.02M5.5 9.5a3.2 3.2 0 0 0 5 0",
        "projectDisplay" => "M2 3.4h7.6v5.2H2zM6.4 7.4H14v5.2H6.4",
        "lockWorkstation" => "M4 7.5h8v5.5H4zM6 7.5V5.5a2 2 0 0 1 4 0v2",
        "screensaver" => "M13 9.5A5.5 5.5 0 0 1 6.5 3 5.5 5.5 0 1 0 13 9.5z",
        "displayOff" => "M8 2v6M4.5 4.5a5 5 0 1 0 7 0",
        "custom" => "M3 4l3 4-3 4M8.5 12H13",
        _ => "M3 8h10",
    }
}

fn kind_of(action: &Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_else(|| "none".into())
}

fn modifier_id(m: Modifier) -> String {
    serde_json::to_value(m)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "none".into())
}

/// The action for a picked kind, keeping a custom command's text if the
/// corner already had one.
fn action_from(kind: &str, current: &Action) -> Action {
    if kind == "custom" {
        return match current {
            Action::Custom { .. } => current.clone(),
            _ => Action::Custom {
                command: String::new(),
                args: String::new(),
            },
        };
    }
    serde_json::from_value(serde_json::json!({ "kind": kind })).unwrap_or(Action::None)
}

// ---------------------------------------------------------------------------
// Tokens: the original web page's `:root`, dark and light
// ---------------------------------------------------------------------------

struct Tokens {
    bg: u32,
    raised: u32,
    sunken: u32,
    line: u32,
    line_soft: u32,
    text: u32,
    dim: u32,
    faint: u32,
    accent: u32,
    accent_dim: u32,
    danger: u32,
}

const DARK: Tokens = Tokens {
    bg: 0x0e1016,
    raised: 0x161a24,
    sunken: 0x0a0c11,
    line: 0x242938,
    line_soft: 0x1b1f2b,
    text: 0xe9ecf4,
    dim: 0x97a0b5,
    faint: 0x5d657a,
    accent: 0x6ea8fe,
    accent_dim: 0x2f4a78,
    danger: 0xef7676,
};

const LIGHT: Tokens = Tokens {
    bg: 0xf2f4f9,
    raised: 0xffffff,
    sunken: 0xeaeef6,
    line: 0xd3dae8,
    line_soft: 0xe5eaf2,
    text: 0x151925,
    dim: 0x59627a,
    faint: 0x939cb1,
    accent: 0x2563c9,
    accent_dim: 0xa8c1ea,
    danger: 0xc2402f,
};

fn tokens(dark: bool) -> &'static Tokens {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// `color-mix(in srgb, a t, b)`: `t` of `a`, the rest `b`. Colours are
/// `0xRRGGBB`, as written in the CSS.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, shift: u32| ((c >> shift) & 0xff) as f32;
    let one = |shift: u32| {
        let v = ch(a, shift) * t + ch(b, shift) * (1.0 - t);
        (v.round().clamp(0.0, 255.0) as u32) << shift
    };
    one(16) | one(8) | one(0)
}

/// `0xRRGGBB` to GDI's `0x00BBGGRR`.
fn colorref(c: u32) -> COLORREF {
    COLORREF(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff))
}

// ---------------------------------------------------------------------------
// State, as the tray pushes it
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default, Clone)]
struct State {
    config: Option<Config>,
    #[serde(default)]
    displays: Vec<DisplayView>,
    #[serde(default)]
    hitboxes: Vec<HitboxView>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    autostart: bool,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct DisplayView {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    primary: bool,
}

#[derive(Deserialize, Default, Clone)]
struct HitboxView {
    corner: String,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn status_text(c: &Config, hitboxes: usize) -> String {
    let assigned = c.corners.iter().filter(|a| !a.is_none()).count();
    if !c.enabled {
        return "Hot corners are paused".into();
    }
    if assigned == 0 {
        return "No corners assigned yet. Try a preset".into();
    }
    let dwell = if c.dwell_ms == 0 {
        "fires instantly".to_string()
    } else {
        format!("after {} ms", c.dwell_ms)
    };
    format!(
        "{assigned} {} assigned, {hitboxes} hot {} on screen, {dwell}",
        if assigned == 1 { "corner" } else { "corners" },
        if hitboxes == 1 { "spot" } else { "spots" }
    )
}

fn mode_hint(c: &Config, displays: usize) -> String {
    let n = displays.max(1);
    if n == 1 {
        return "The four corners of your display".into();
    }
    match c.monitor_mode {
        MonitorMode::OuterCorners => "Only the four outer corners of the whole desktop".into(),
        MonitorMode::EveryMonitor => format!("All four corners of each of your {n} displays"),
        MonitorMode::PrimaryOnly => "Only the primary display; the others stay inert".into(),
    }
}

fn slider_value(c: &Config, k: usize) -> u32 {
    match k {
        0 => c.dwell_ms,
        1 => c.corner_size.max(0) as u32,
        _ => c.cooldown_ms,
    }
}

fn set_slider(c: &mut Config, k: usize, v: u32) {
    match k {
        0 => c.dwell_ms = v,
        1 => c.corner_size = v as i32,
        _ => c.cooldown_ms = v,
    }
}

fn slider_text(k: usize, v: u32) -> String {
    match k {
        0 if v == 0 => "instant".into(),
        0 => format!("{v} ms"),
        1 => format!("{v} px"),
        _ => format!("{:.1} s", v as f64 / 1000.0),
    }
}

/// Snap to the range and its step, as `input[type=range]` does.
fn snap(k: usize, v: f32) -> u32 {
    let (min, max, step) = SLIDERS[k];
    let steps = ((v - min as f32) / step as f32).round().max(0.0) as u32;
    (min + steps * step).clamp(min, max)
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum F {
    /// 13px, the body: selects and the command fields.
    Body,
    /// `h1`: 15px, 600.
    H1,
    /// 11.5px: the subtitle, chips, segments, status, slider values.
    Sub,
    /// 10.5px, 600, tracked and upper case: the caps labels.
    Caps,
    /// `.row .nm`: 12.5px.
    Nm,
    /// `.cmdhead .nm`: 12px, 600.
    CmdNm,
    /// 11px: hints, the footer.
    Small,
    /// A footer link under the pointer: 11px, underlined.
    Link,
    /// `.mini`: 10.5px.
    Mini,
    /// The diagram's "Primary" tag: 9.5px, 600, tracked.
    Tag,
    /// The diagram's dimensions tag: 9.5px.
    TagDims,
}

/// `(px, semibold, underline, tracking in em)` per [`F`], in its order.
const FONT_SPECS: [(f32, bool, bool, f32); 11] = [
    (13.0, false, false, 0.0),
    (15.0, true, false, 0.0),
    (11.5, false, false, 0.0),
    (10.5, true, false, 0.07),
    (12.5, false, false, 0.0),
    (12.0, true, false, 0.0),
    (11.0, false, false, 0.0),
    (11.0, false, true, 0.0),
    (10.5, false, false, 0.0),
    (9.5, true, false, 0.06),
    (9.5, false, false, 0.0),
];

struct Fonts {
    fonts: Vec<HFONT>,
    scale: f32,
}

impl Fonts {
    /// The web page asked for Segoe UI Variable Text, then Segoe UI. GDI sees the
    /// variable font's named instances as families of their own, so the
    /// semibold one is asked for by name rather than synthesised from a
    /// weight; a family that is not installed is detected and skipped.
    fn new(scale: f32) -> Fonts {
        let dc = unsafe { CreateCompatibleDC(None) };
        let fonts = FONT_SPECS
            .iter()
            .map(|&(px, semibold, underline, _)| {
                let faces: &[&str] = if semibold {
                    &[
                        "Segoe UI Variable Text Semibold",
                        "Segoe UI Semibold",
                        "Segoe UI",
                    ]
                } else {
                    &["Segoe UI Variable Text", "Segoe UI"]
                };
                let height = -((px * scale).round() as i32).max(1);
                let weight = if semibold { 600 } else { 400 };
                let mut last = HFONT::default();
                for face in faces {
                    let name = wide(face);
                    let font = unsafe {
                        CreateFontW(
                            height,
                            0,
                            0,
                            0,
                            weight,
                            0,
                            underline as u32,
                            0,
                            DEFAULT_CHARSET,
                            OUT_DEFAULT_PRECIS,
                            CLIP_DEFAULT_PRECIS,
                            CLEARTYPE_QUALITY,
                            0,
                            pcwstr(&name),
                        )
                    };
                    if !last.is_invalid() {
                        unsafe {
                            let _ = DeleteObject(last.into());
                        }
                    }
                    last = font;
                    if face_is(dc, font, face) {
                        break;
                    }
                }
                last
            })
            .collect();
        unsafe {
            let _ = DeleteDC(dc);
        }
        Fonts { fonts, scale }
    }

    fn get(&self, f: F) -> HFONT {
        self.fonts[f as usize]
    }

    /// The letter spacing of `f`, in device pixels.
    fn tracking(&self, f: F) -> i32 {
        let (px, _, _, em) = FONT_SPECS[f as usize];
        (px * em * self.scale).round() as i32
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in &self.fonts {
            unsafe {
                let _ = DeleteObject((*f).into());
            }
        }
    }
}

fn face_is(dc: HDC, font: HFONT, face: &str) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe {
        let old = SelectObject(dc, font.into());
        let n = GetTextFaceW(dc, Some(&mut buf));
        SelectObject(dc, old);
        n
    };
    let got = String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1).min(buf.len())]);
    got.trim_end_matches('\0').eq_ignore_ascii_case(face)
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A rectangle in the web page's CSS pixels, in content coordinates (scrolling
/// is applied when painting and hit-testing).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Area {
    const fn new(x: f32, y: f32, w: f32, h: f32) -> Area {
        Area { x, y, w, h }
    }
    fn r(&self) -> f32 {
        self.x + self.w
    }
    fn b(&self) -> f32 {
        self.y + self.h
    }
    fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.r() && y >= self.y && y < self.b()
    }
    fn grow(&self, d: f32) -> Area {
        Area::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

/// Everything that takes the keyboard or the mouse, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Enabled,
    Preset(usize),
    Test(usize),
    Select(usize),
    CustomTest(usize),
    Command(usize),
    Args(usize),
    Slider(usize),
    Modifier,
    Mode(usize),
    Fullscreen,
    Dragging,
    Autostart,
    Link(usize),
}

impl Ctl {
    /// Index into the window's edit controls, for the two that are one.
    fn edit(self) -> Option<usize> {
        match self {
            Ctl::Command(i) => Some(i * 2),
            Ctl::Args(i) => Some(i * 2 + 1),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CustomRow {
    row: Area,
    first: bool,
    test: Area,
    command: Area,
    args: Area,
}

#[derive(Clone, Copy, Default)]
struct Row {
    row: Area,
    first: bool,
    /// Room for the name and hint, left of the control.
    info: Area,
}

#[derive(Default)]
struct Layout {
    mark: Area,
    titles: Area,
    enabled: Area,
    stage: Area,
    diagram: Area,
    status: Area,
    presets_label: Area,
    chips: [Area; 3],
    cards: [Area; 4],
    card_tests: [Area; 4],
    card_selects: [Area; 4],
    custom: Option<Area>,
    custom_rows: [Option<CustomRow>; 4],
    trigger: Area,
    trigger_rows: [Row; 4],
    sliders: [Area; 3],
    outputs: [Area; 3],
    modifier: Area,
    scope: Area,
    scope_rows: [Row; 4],
    seg: Area,
    segs: [Area; 3],
    switches: [Area; 3],
    footer: Area,
    version: Area,
    toast: Area,
    links: [Area; 3],
    height: f32,
    /// Focusable controls in Tab order, with where they are.
    ctls: Vec<(Ctl, Area)>,
}

impl Layout {
    fn area(&self, c: Ctl) -> Option<Area> {
        self.ctls.iter().find(|(k, _)| *k == c).map(|(_, a)| *a)
    }
}

/// Width of the scrollbar, kept clear on the right.
const BAR: f32 = 10.0;
const ROW_H: f32 = 60.0;
const CMD_ROW_H: f32 = 85.0;
const CARD_H: f32 = 88.0;
const H2_H: f32 = 28.0;

fn diagram_height(displays: &[DisplayView], width: f32) -> f32 {
    let (w, h) = extent(displays);
    if w <= 0.0 || h <= 0.0 {
        return 190.0;
    }
    (width * h / w).round().clamp(140.0, 268.0)
}

fn extent(displays: &[DisplayView]) -> (f32, f32) {
    let l = displays.iter().map(|d| d.left).min().unwrap_or(0);
    let t = displays.iter().map(|d| d.top).min().unwrap_or(0);
    let r = displays.iter().map(|d| d.right).max().unwrap_or(0);
    let b = displays.iter().map(|d| d.bottom).max().unwrap_or(0);
    ((r - l) as f32, (b - t) as f32)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

const WM_PUSH: u32 = WM_APP + 1;
const TIMER_SAVE: usize = 1;
const TIMER_TOAST: usize = 2;
const EDIT_ID: i32 = 100;

/// In `Win32_UI_Controls` in the windows crate, which nothing else needs.
const WM_MOUSELEAVE: u32 = 0x02A3;
const EM_SETSEL: u32 = 0x00B1;
const EM_SETMARGINS: u32 = 0x00D3;
const EC_LEFTMARGIN: usize = 1;
const EC_RIGHTMARGIN: usize = 2;
const EN_SETFOCUS: u32 = 0x0100;
const EN_KILLFOCUS: u32 = 0x0200;
const EN_CHANGE: u32 = 0x0300;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Slider(usize),
    /// The scrollbar thumb, held this far below its top.
    Bar(f32),
}

/// Work for after the window's state is released; see the module comment.
enum Fx {
    Focus(HWND),
    Menu(Ctl),
    Capture,
    Release,
}

struct Win {
    hwnd: HWND,
    dpi: u32,
    dark: bool,
    fonts: Fonts,
    /// Anti-aliased shapes; `None` if Direct2D would not start, and GDI
    /// draws them instead.
    d2d: Option<D2d>,
    /// A memory DC for measuring text.
    mdc: HDC,
    state: State,
    lay: Layout,
    scroll: f32,
    hover: Option<Ctl>,
    hover_card: Option<usize>,
    hover_bar: bool,
    focus: Option<Ctl>,
    /// Focus came from the keyboard, so it shows a ring (`:focus-visible`).
    focus_visible: bool,
    pressed: Option<Ctl>,
    drag: Drag,
    tracking: bool,
    edits: [HWND; 8],
    placed: [Option<(i32, i32, i32, i32)>; 8],
    toast: String,
    toast_bad: bool,
    save_pending: bool,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
    /// Set while the edit controls' text is being set from the state, so
    /// their change notifications do not echo it back as an edit.
    static SYNCING: Cell<bool> = const { Cell::new(false) };
    /// What `WM_CTLCOLOREDIT` and the edits' placeholders need, kept outside
    /// `WIN` because both are asked for from inside other calls.
    static EDIT_BRUSH: Cell<isize> = const { Cell::new(0) };
    static EDIT_COLORS: Cell<(u32, u32, u32)> = const { Cell::new((0, 0, 0)) };
    static EDIT_FONT: Cell<isize> = const { Cell::new(0) };
    static EDIT_PROC: Cell<isize> = const { Cell::new(0) };
}

/// Run `f` on the window's state, unless it is missing or already borrowed
/// further up the stack.
fn with_win<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WIN.with(|w| {
        let mut guard = w.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

fn run_fx(hwnd: HWND, fx: Vec<Fx>) {
    for f in fx {
        match f {
            Fx::Focus(h) => unsafe {
                let _ = SetFocus(Some(h));
            },
            Fx::Menu(c) => open_menu(hwnd, c),
            Fx::Capture => unsafe {
                SetCapture(hwnd);
            },
            Fx::Release => unsafe {
                let _ = ReleaseCapture();
            },
        }
    }
}

impl Win {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn t(&self) -> &'static Tokens {
        tokens(self.dark)
    }

    fn config(&self) -> Config {
        self.state.config.clone().unwrap_or_default()
    }

    /// The client area in CSS pixels.
    fn view(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let s = self.scale();
        (
            (rc.right - rc.left) as f32 / s,
            (rc.bottom - rc.top) as f32 / s,
        )
    }

    fn max_scroll(&self) -> f32 {
        (self.lay.height - self.view().1).max(0.0)
    }

    /// Text width in CSS pixels.
    fn tw(&self, f: F, text: &str) -> f32 {
        let buf: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        unsafe {
            let old = SelectObject(self.mdc, self.fonts.get(f).into());
            SetTextCharacterExtra(self.mdc, self.fonts.tracking(f));
            let _ = GetTextExtentPoint32W(self.mdc, &buf, &mut size);
            SetTextCharacterExtra(self.mdc, 0);
            SelectObject(self.mdc, old);
        }
        size.cx as f32 / self.scale()
    }

    // -- Layout -------------------------------------------------------------

    /// The web page's box model, worked through for the current width and state.
    fn layout(&self) -> Layout {
        let c = self.config();
        let mut l = Layout::default();
        let (vw, _) = self.view();
        let avail = (vw - BAR).max(0.0);
        let wrap = avail.min(640.0);
        let x0 = (avail - wrap) / 2.0 + 24.0;
        let iw = (wrap - 48.0).max(200.0);
        let right = x0 + iw;
        let mut ctls = vec![];

        // Header: 28px mark, titles, the master switch.
        let mut y = 20.0;
        l.mark = Area::new(x0, y + 6.0, 28.0, 28.0);
        l.enabled = Area::new(right - 38.0, y + 9.0, 38.0, 22.0);
        l.titles = Area::new(x0 + 39.0, y, iw - 39.0 - 49.0, 40.0);
        ctls.push((Ctl::Enabled, l.enabled));
        y += 40.0 + 18.0;

        // Stage: 1px border, 14px padding, the diagram, the status bar.
        let dw = iw - 30.0;
        let dh = diagram_height(&self.state.displays, dw);
        l.stage = Area::new(x0, y, iw, 1.0 + 14.0 + dh + 30.0 + 1.0);
        l.diagram = Area::new(x0 + 15.0, y + 15.0, dw, dh);
        l.status = Area::new(x0 + 17.0, y + 15.0 + dh, dw - 4.0, 30.0);
        y = l.stage.b() + 12.0;

        // Presets.
        let lw = self.tw(F::Caps, "PRESETS");
        l.presets_label = Area::new(x0, y, lw, 27.0);
        let mut x = x0 + lw + 2.0 + 7.0;
        for (k, (name, _)) in PRESETS.iter().enumerate() {
            let w = self.tw(F::Sub, name) + 24.0;
            l.chips[k] = Area::new(x, y, w, 27.0);
            ctls.push((Ctl::Preset(k), l.chips[k]));
            x += w + 7.0;
        }
        y += 27.0 + 12.0;

        // The corner cards, two by two.
        let cw = (iw - 9.0) / 2.0;
        let test_w = self.tw(F::Mini, "Test") + 16.0;
        for i in 0..4 {
            let card = Area::new(
                x0 + (i % 2) as f32 * (cw + 9.0),
                y + (i / 2) as f32 * (CARD_H + 9.0),
                cw,
                CARD_H,
            );
            let (ix, iy, irw) = (card.x + 13.0, card.y + 11.0, cw - 26.0);
            l.cards[i] = card;
            l.card_tests[i] = Area::new(ix + irw - test_w, iy, test_w, 22.0);
            l.card_selects[i] = Area::new(ix + 24.0, iy + 30.0, irw - 24.0, 34.0);
            if !c.corners[i].is_none() {
                ctls.push((Ctl::Test(i), l.card_tests[i]));
            }
            ctls.push((Ctl::Select(i), l.card_selects[i]));
        }
        y += 2.0 * CARD_H + 9.0 + 14.0;

        // Custom commands, while any corner has one.
        let custom: Vec<usize> = (0..4)
            .filter(|&i| matches!(c.corners[i], Action::Custom { .. }))
            .collect();
        if !custom.is_empty() {
            let top = y;
            let mut ry = y + H2_H;
            for (n, &i) in custom.iter().enumerate() {
                if n > 0 {
                    ry += 1.0;
                }
                let row = Area::new(x0, ry, iw, CMD_ROW_H);
                let gy = ry + 11.0 + 22.0 + 7.0;
                let cr = CustomRow {
                    row,
                    first: n == 0,
                    test: Area::new(right - 14.0 - test_w, ry + 11.0, test_w, 22.0),
                    command: Area::new(x0 + 14.0, gy, iw - 28.0 - 128.0 - 8.0, 34.0),
                    args: Area::new(right - 14.0 - 128.0, gy, 128.0, 34.0),
                };
                ctls.push((Ctl::CustomTest(i), cr.test));
                ctls.push((Ctl::Command(i), cr.command));
                ctls.push((Ctl::Args(i), cr.args));
                l.custom_rows[i] = Some(cr);
                ry += CMD_ROW_H;
            }
            let panel = Area::new(x0, top, iw, ry + 1.0 - top);
            l.custom = Some(panel);
            y = panel.b() + 12.0;
        }

        // A panel of `.row`s, each with a control of `ctl_w` on the right.
        let rows = |top: f32, widths: [f32; 4]| -> ([Row; 4], f32) {
            let mut out = [Row::default(); 4];
            let mut ry = top + H2_H;
            for (n, w) in widths.iter().enumerate() {
                if n > 0 {
                    ry += 1.0;
                }
                out[n] = Row {
                    row: Area::new(x0, ry, iw, ROW_H),
                    first: n == 0,
                    info: Area::new(x0 + 14.0, ry + 11.0, (iw - 28.0 - w - 14.0).max(40.0), 38.0),
                };
                ry += ROW_H;
            }
            (out, ry + 1.0)
        };

        // Trigger.
        let slider_w = 128.0 + 11.0 + 50.0;
        let (trigger_rows, bottom) = rows(y, [slider_w, slider_w, slider_w, 132.0]);
        l.trigger = Area::new(x0, y, iw, bottom - y);
        for (k, row) in trigger_rows.iter().take(3).enumerate() {
            let cy = row.row.cy();
            l.outputs[k] = Area::new(right - 14.0 - 50.0, cy - 9.0, 50.0, 18.0);
            l.sliders[k] = Area::new(right - 14.0 - slider_w, cy - 9.0, 128.0, 18.0);
            ctls.push((Ctl::Slider(k), l.sliders[k]));
        }
        l.modifier = Area::new(
            right - 14.0 - 132.0,
            trigger_rows[3].row.cy() - 17.0,
            132.0,
            34.0,
        );
        ctls.push((Ctl::Modifier, l.modifier));
        l.trigger_rows = trigger_rows;
        y = bottom + 12.0;

        // Scope.
        let seg_widths: Vec<f32> = MODES
            .iter()
            .map(|(_, t)| self.tw(F::Sub, t) + 20.0)
            .collect();
        let seg_w = seg_widths.iter().sum::<f32>() + 6.0;
        let (scope_rows, bottom) = rows(y, [seg_w, 38.0, 38.0, 38.0]);
        l.scope = Area::new(x0, y, iw, bottom - y);
        let cy = scope_rows[0].row.cy();
        l.seg = Area::new(right - 14.0 - seg_w, cy - 15.5, seg_w, 31.0);
        let mut sx = l.seg.x + 3.0;
        for (k, w) in seg_widths.iter().enumerate() {
            l.segs[k] = Area::new(sx, l.seg.y + 3.0, *w, 25.0);
            ctls.push((Ctl::Mode(k), l.segs[k]));
            sx += w;
        }
        for (k, ctl) in [Ctl::Fullscreen, Ctl::Dragging, Ctl::Autostart]
            .into_iter()
            .enumerate()
        {
            let cy = scope_rows[k + 1].row.cy();
            l.switches[k] = Area::new(right - 14.0 - 38.0, cy - 11.0, 38.0, 22.0);
            ctls.push((ctl, l.switches[k]));
        }
        l.scope_rows = scope_rows;
        // The panel's 12px margin and the footer's 16px collapse to 16.
        y = bottom + 16.0;

        // Footer: a hairline, the version, the toast, the links.
        l.footer = Area::new(x0, y, iw, 1.0 + 13.0 + 21.0);
        let fy = y + 14.0;
        let mut lx = right;
        for k in (0..3).rev() {
            let w = self.tw(F::Small, LINKS[k].0) + 8.0;
            lx -= w;
            l.links[k] = Area::new(lx, fy, w, 21.0);
            lx -= 10.0;
        }
        let vw_ = self.tw(F::Small, &format!("Cantos {}", self.state.version));
        l.version = Area::new(x0, fy, vw_ + 1.0, 21.0);
        l.toast = Area::new(
            l.version.r() + 10.0,
            fy,
            (lx - l.version.r() - 10.0).max(0.0),
            21.0,
        );
        for k in 0..3 {
            ctls.push((Ctl::Link(k), l.links[k]));
        }
        l.height = l.footer.b() + 28.0;
        l.ctls = ctls;
        l
    }

    /// Recompute the layout after anything that moves things, and keep the
    /// scroll position and the edit controls in step with it.
    fn relayout(&mut self) {
        self.lay = self.layout();
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        // Focus on a control that has gone (a Test button whose corner was
        // cleared, a hidden custom row) goes nowhere.
        if let Some(f) = self.focus {
            if self.lay.area(f).is_none() {
                self.focus = None;
            }
        }
        self.place_edits();
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    // -- The edit controls ----------------------------------------------------

    /// Put each command field inside its drawn box, or hide it.
    fn place_edits(&mut self) {
        let s = self.scale();
        let line = self.body_height();
        for i in 0..4 {
            let row = self.lay.custom_rows[i];
            for field in 0..2 {
                let e = i * 2 + field;
                let target = row.map(|r| {
                    let b = if field == 0 { r.command } else { r.args };
                    let x = ((b.x + 10.0) * s).round() as i32;
                    let w = ((b.w - 20.0) * s).round() as i32;
                    let top = ((b.y - self.scroll) * s).round() as i32;
                    let h = (b.h * s).round() as i32;
                    (x, top + (h - line) / 2, w.max(1), line)
                });
                if target == self.placed[e] {
                    continue;
                }
                let hwnd = self.edits[e];
                unsafe {
                    match target {
                        Some((x, y, w, h)) => {
                            let _ =
                                SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                            if self.placed[e].is_none() {
                                let _ = ShowWindow(hwnd, SW_SHOWNA);
                            }
                        }
                        None => {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                }
                self.placed[e] = target;
            }
        }
    }

    fn body_height(&self) -> i32 {
        let mut tm = TEXTMETRICW::default();
        unsafe {
            let old = SelectObject(self.mdc, self.fonts.get(F::Body).into());
            let _ = GetTextMetricsW(self.mdc, &mut tm);
            SelectObject(self.mdc, old);
        }
        tm.tmHeight
    }

    /// Bring the command fields' text in line with the config, never under
    /// the user's caret: the echo of a save is what they just typed.
    fn sync_edits(&self) {
        let c = self.config();
        let focused = unsafe { GetFocus() };
        SYNCING.with(|s| s.set(true));
        for (i, action) in c.corners.iter().enumerate() {
            let Action::Custom { command, args } = action else {
                continue;
            };
            for (field, want) in [command, args].into_iter().enumerate() {
                let hwnd = self.edits[i * 2 + field];
                if hwnd == focused || edit_text(hwnd) == *want {
                    continue;
                }
                let text = wide(want);
                unsafe {
                    let _ = SetWindowTextW(hwnd, pcwstr(&text));
                }
            }
        }
        SYNCING.with(|s| s.set(false));
    }

    fn set_edit_fonts(&mut self) {
        let font = self.fonts.get(F::Body);
        EDIT_FONT.with(|f| f.set(font.0 as isize));
        for &e in &self.edits {
            unsafe {
                SendMessageW(
                    e,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
                SendMessageW(
                    e,
                    EM_SETMARGINS,
                    Some(WPARAM(EC_LEFTMARGIN | EC_RIGHTMARGIN)),
                    Some(LPARAM(0)),
                );
            }
        }
        // The font changed size, so every field needs placing again.
        self.placed = [None; 8];
    }

    // -- Theme ----------------------------------------------------------------

    fn apply_theme(&mut self) {
        self.dark = traykit::theme::taskbar_is_dark();
        let t = self.t();
        EDIT_COLORS.with(|c| c.set((t.text, t.sunken, t.faint)));
        let brush = unsafe { CreateSolidBrush(colorref(t.sunken)) };
        let old = EDIT_BRUSH.with(|b| b.replace(brush.0 as isize));
        if old != 0 {
            unsafe {
                let _ = DeleteObject(HBRUSH(old as *mut c_void).into());
            }
        }
        // Without this a dark window gets a white title bar, which looks broken.
        let dark: i32 = self.dark as i32;
        unsafe {
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const i32 as *const c_void,
                std::mem::size_of::<i32>() as u32,
            );
            for &e in &self.edits {
                let _ = InvalidateRect(Some(e), None, true);
            }
        }
        self.invalidate();
    }

    // -- Editing --------------------------------------------------------------

    /// Edit the config, show the edit, and save it once edits pause: a
    /// dragged slider is one save, not one per step. The web page waited 140ms;
    /// so does this.
    fn edit(&mut self, f: impl FnOnce(&mut Config)) {
        let Some(c) = self.state.config.as_mut() else {
            return;
        };
        f(c);
        self.relayout();
        self.sync_edits();
        self.save_pending = true;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SAVE, 140, None);
        }
    }

    fn save_now(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SAVE);
        }
        if std::mem::take(&mut self.save_pending) {
            child::send(&serde_json::json!({ "cmd": "set", "config": self.config() }));
        }
    }

    fn show_toast(&mut self, message: &str, bad: bool) {
        self.toast = message.to_string();
        self.toast_bad = bad;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TOAST, 2500, None);
        }
        self.invalidate();
    }

    /// Click, Space or Enter on a control.
    fn activate(&mut self, ctl: Ctl) -> Vec<Fx> {
        match ctl {
            Ctl::Enabled => self.edit(|c| c.enabled = !c.enabled),
            Ctl::Preset(k) => self.edit(|c| {
                for (i, kind) in PRESETS[k].1.iter().enumerate() {
                    c.corners[i] = action_from(kind, &c.corners[i]);
                }
            }),
            Ctl::Test(i) | Ctl::CustomTest(i) => {
                child::send(&serde_json::json!({ "cmd": "test", "corner": i }));
            }
            Ctl::Select(_) | Ctl::Modifier => return vec![Fx::Menu(ctl)],
            Ctl::Command(_) | Ctl::Args(_) => {
                if let Some(e) = ctl.edit() {
                    return vec![Fx::Focus(self.edits[e])];
                }
            }
            Ctl::Slider(_) => {}
            Ctl::Mode(k) => self.edit(|c| c.monitor_mode = MODES[k].0),
            Ctl::Fullscreen => self.edit(|c| c.suppress_fullscreen = !c.suppress_fullscreen),
            Ctl::Dragging => self.edit(|c| c.suppress_while_dragging = !c.suppress_while_dragging),
            Ctl::Autostart => {
                // Run-at-login lives with the OS, so it goes to the tray, not
                // the config; the tray answers with what actually happened.
                self.state.autostart = !self.state.autostart;
                child::send(
                    &serde_json::json!({ "cmd": "autostart", "value": self.state.autostart }),
                );
                self.invalidate();
            }
            Ctl::Link(k) => child::send(&serde_json::json!({ "cmd": LINKS[k].1 })),
        }
        vec![]
    }

    /// The options of a select: `(value, label, separator before)`.
    fn options(&self, ctl: Ctl) -> Vec<(String, String, bool)> {
        match ctl {
            Ctl::Modifier => MODIFIERS
                .iter()
                .map(|(v, l)| (v.to_string(), l.to_string(), false))
                .collect(),
            _ => CATALOG
                .iter()
                .enumerate()
                .flat_map(|(g, (_, items))| {
                    items
                        .iter()
                        .enumerate()
                        .map(move |(n, (v, l))| (v.to_string(), l.to_string(), g > 0 && n == 0))
                })
                .collect(),
        }
    }

    fn selected(&self, ctl: Ctl) -> String {
        let c = self.config();
        match ctl {
            Ctl::Select(i) => kind_of(&c.corners[i]),
            _ => modifier_id(c.modifier),
        }
    }

    fn pick(&mut self, ctl: Ctl, value: &str) {
        match ctl {
            Ctl::Select(i) => self.edit(|c| c.corners[i] = action_from(value, &c.corners[i])),
            Ctl::Modifier => {
                if let Ok(m) = serde_json::from_value::<Modifier>(serde_json::json!(value)) {
                    self.edit(|c| c.modifier = m);
                }
            }
            _ => {}
        }
    }

    // -- Pointer --------------------------------------------------------------

    /// Physical client pixels to content coordinates.
    fn to_content(&self, x: i32, y: i32) -> (f32, f32) {
        let s = self.scale();
        (x as f32 / s, y as f32 / s + self.scroll)
    }

    /// The page's `cursor`: a hand on anything clickable, a text beam on a
    /// command field's padding (the field itself sets its own), and the
    /// arrow elsewhere, the scrollbar included.
    fn cursor_at(&self, px: i32, py: i32) -> PCWSTR {
        let vx = px as f32 / self.scale();
        if self.thumb().is_some() && vx >= self.view().0 - BAR {
            return IDC_ARROW;
        }
        let (x, y) = self.to_content(px, py);
        match self.hit(x, y) {
            None => IDC_ARROW,
            Some(Ctl::Command(_) | Ctl::Args(_)) => IDC_IBEAM,
            Some(_) => IDC_HAND,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Ctl> {
        self.lay
            .ctls
            .iter()
            .find(|(_, a)| a.contains(x, y))
            .map(|(c, _)| *c)
    }

    /// The scrollbar thumb, in view coordinates, if the content scrolls.
    fn thumb(&self) -> Option<Area> {
        let (vw, vh) = self.view();
        let max = self.max_scroll();
        if max <= 0.0 {
            return None;
        }
        let h = (vh * vh / self.lay.height).max(30.0);
        let y = self.scroll / max * (vh - h);
        Some(Area::new(vw - BAR, y, BAR, h))
    }

    fn set_scroll(&mut self, v: f32) {
        let v = v.clamp(0.0, self.max_scroll());
        if (v - self.scroll).abs() > f32::EPSILON {
            self.scroll = v;
            self.place_edits();
            self.invalidate();
        }
    }

    fn mouse_move(&mut self, px: i32, py: i32) {
        let (x, y) = self.to_content(px, py);
        match self.drag {
            Drag::Slider(k) => {
                self.slide_to(k, x);
                return;
            }
            Drag::Bar(grab) => {
                let (_, vh) = self.view();
                if let Some(t) = self.thumb() {
                    let top = py as f32 / self.scale() - grab;
                    let room = (vh - t.h).max(1.0);
                    self.set_scroll(top / room * self.max_scroll());
                }
                return;
            }
            Drag::None => {}
        }
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking = true;
        }
        let vx = px as f32 / self.scale();
        let on_bar = self.thumb().is_some() && vx >= self.view().0 - BAR;
        let hover = if on_bar { None } else { self.hit(x, y) };
        let card = (0..4).find(|&i| !on_bar && self.lay.cards[i].contains(x, y));
        if hover != self.hover || card != self.hover_card || on_bar != self.hover_bar {
            self.hover = hover;
            self.hover_card = card;
            self.hover_bar = on_bar;
            self.invalidate();
        }
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        if self.drag == Drag::None {
            self.hover = None;
            self.hover_card = None;
            self.hover_bar = false;
            self.invalidate();
        }
    }

    fn mouse_down(&mut self, px: i32, py: i32) -> Vec<Fx> {
        let (x, y) = self.to_content(px, py);
        let (vx, vy) = (px as f32 / self.scale(), py as f32 / self.scale());
        if let Some(t) = self.thumb() {
            if vx >= t.x {
                if t.contains(vx, vy) {
                    self.drag = Drag::Bar(vy - t.y);
                    return vec![Fx::Capture];
                }
                // A click in the track pages, as a scrollbar does.
                let page = self.view().1 - 40.0;
                let dir = if vy < t.y { -1.0 } else { 1.0 };
                self.set_scroll(self.scroll + dir * page);
                return vec![];
            }
        }
        self.focus_visible = false;
        let Some(ctl) = self.hit(x, y) else {
            // Clicking nothing blurs whatever had focus, as on the web page.
            self.focus = None;
            self.invalidate();
            return vec![Fx::Focus(self.hwnd)];
        };
        if let Some(e) = ctl.edit() {
            self.focus = Some(ctl);
            return vec![Fx::Focus(self.edits[e])];
        }
        self.focus = Some(ctl);
        self.invalidate();
        let mut fx = vec![Fx::Focus(self.hwnd)];
        match ctl {
            Ctl::Slider(k) => {
                self.drag = Drag::Slider(k);
                self.slide_to(k, x);
                fx.push(Fx::Capture);
            }
            // A select opens on press, as a native one does.
            Ctl::Select(_) | Ctl::Modifier => fx.extend(self.activate(ctl)),
            _ => {
                self.pressed = Some(ctl);
                fx.push(Fx::Capture);
            }
        }
        fx
    }

    fn mouse_up(&mut self, px: i32, py: i32) -> Vec<Fx> {
        let (x, y) = self.to_content(px, py);
        let mut fx = vec![];
        if self.drag != Drag::None {
            self.drag = Drag::None;
            fx.push(Fx::Release);
        }
        if let Some(p) = self.pressed.take() {
            fx.push(Fx::Release);
            if self.hit(x, y) == Some(p) {
                fx.extend(self.activate(p));
            }
        }
        fx
    }

    fn slide_to(&mut self, k: usize, x: f32) {
        let a = self.lay.sliders[k];
        let (min, max, _) = SLIDERS[k];
        let t = ((x - a.x - 6.5) / (a.w - 13.0)).clamp(0.0, 1.0);
        let v = snap(k, min as f32 + t * (max - min) as f32);
        if v != slider_value(&self.config(), k) {
            self.edit(|c| set_slider(c, k, v));
        }
    }

    // -- Keyboard -------------------------------------------------------------

    /// Tab and Shift+Tab, through every control including the fields.
    fn tab(&mut self, back: bool) -> Vec<Fx> {
        let n = self.lay.ctls.len();
        if n == 0 {
            return vec![];
        }
        let at = self
            .focus
            .and_then(|f| self.lay.ctls.iter().position(|(c, _)| *c == f));
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        let (ctl, area) = self.lay.ctls[next];
        self.focus = Some(ctl);
        self.focus_visible = true;
        self.reveal(area);
        self.invalidate();
        match ctl.edit() {
            Some(e) => {
                let hwnd = self.edits[e];
                // Select the field's text, as tabbing into an input does.
                unsafe {
                    SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                }
                vec![Fx::Focus(hwnd)]
            }
            None => vec![Fx::Focus(self.hwnd)],
        }
    }

    /// Scroll just enough to show `a`.
    fn reveal(&mut self, a: Area) {
        let (_, vh) = self.view();
        let margin = 12.0;
        if a.y - margin < self.scroll {
            self.set_scroll(a.y - margin);
        } else if a.b() + margin > self.scroll + vh {
            self.set_scroll(a.b() + margin - vh);
        }
    }

    /// A key on the window itself; `repeat` is auto-repeat, which must not
    /// toggle a switch back and forth.
    fn key(&mut self, vk: VIRTUAL_KEY, repeat: bool) -> Vec<Fx> {
        let (_, vh) = self.view();
        match (self.focus, vk) {
            (Some(Ctl::Slider(k)), _) => {
                let (min, max, step) = SLIDERS[k];
                let v = slider_value(&self.config(), k) as i64;
                let big = (((max - min) / 10) / step).max(1) as i64 * step as i64;
                let v = match vk {
                    VK_LEFT | VK_DOWN => v - step as i64,
                    VK_RIGHT | VK_UP => v + step as i64,
                    VK_PRIOR => v + big,
                    VK_NEXT => v - big,
                    VK_HOME => min as i64,
                    VK_END => max as i64,
                    _ => return vec![],
                };
                let v = v.clamp(min as i64, max as i64) as u32;
                self.edit(|c| set_slider(c, k, v));
                vec![]
            }
            (Some(ctl @ (Ctl::Select(_) | Ctl::Modifier)), VK_UP | VK_DOWN) => {
                // Arrows step through the options without opening the list,
                // as a closed select does on Windows.
                let opts = self.options(ctl);
                let now = self.selected(ctl);
                let at = opts.iter().position(|(v, _, _)| *v == now).unwrap_or(0);
                let to = if vk == VK_UP {
                    at.saturating_sub(1)
                } else {
                    (at + 1).min(opts.len() - 1)
                };
                if to != at {
                    let value = opts[to].0.clone();
                    self.pick(ctl, &value);
                }
                vec![]
            }
            (Some(ctl @ (Ctl::Select(_) | Ctl::Modifier)), VK_F4) => {
                vec![Fx::Menu(ctl)]
            }
            (Some(_), VK_SPACE | VK_RETURN) if repeat => vec![],
            (Some(ctl), VK_SPACE | VK_RETURN) => self.activate(ctl),
            (_, VK_PRIOR) => {
                self.set_scroll(self.scroll - (vh - 40.0));
                vec![]
            }
            (_, VK_NEXT) => {
                self.set_scroll(self.scroll + (vh - 40.0));
                vec![]
            }
            (_, VK_HOME) => {
                self.set_scroll(0.0);
                vec![]
            }
            (_, VK_END) => {
                self.set_scroll(f32::MAX);
                vec![]
            }
            (_, VK_UP) => {
                self.set_scroll(self.scroll - 40.0);
                vec![]
            }
            (_, VK_DOWN) => {
                self.set_scroll(self.scroll + 40.0);
                vec![]
            }
            _ => vec![],
        }
    }

    // -- Calls from the tray --------------------------------------------------

    fn apply(&mut self, push: child::Push) {
        let arg = |i: usize| push.args.get(i).cloned().unwrap_or_default();
        match push.push.as_str() {
            "state" => {
                match serde_json::from_value::<State>(arg(0)) {
                    Ok(s) => self.state = s,
                    Err(e) => lerror!("settings window: bad state push ({e})"),
                }
                self.relayout();
                self.sync_edits();
            }
            "geometry" => {
                let g = arg(0);
                if let Ok(d) = serde_json::from_value(g["displays"].clone()) {
                    self.state.displays = d;
                }
                if let Ok(h) = serde_json::from_value(g["hitboxes"].clone()) {
                    self.state.hitboxes = h;
                }
                self.relayout();
            }
            "toast" => self.show_toast(
                arg(0).as_str().unwrap_or(""),
                arg(1).as_bool().unwrap_or(false),
            ),
            "autostart" => {
                self.state.autostart = arg(0).as_bool().unwrap_or(false);
                self.invalidate();
                if let Some(e) = arg(1).as_str() {
                    self.show_toast(e, true);
                }
            }
            "raise" => unsafe {
                if IsIconic(self.hwnd).as_bool() {
                    let _ = ShowWindow(self.hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(self.hwnd);
            },
            other => lerror!("settings window: unknown push {other:?}"),
        }
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.mdc);
        }
    }
}

fn edit_text(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

/// A select's options as a native popup menu under it, with the groups of
/// the catalog between separators and a check on the current one.
fn open_menu(hwnd: HWND, ctl: Ctl) {
    let Some((opts, now, at)) = with_win(|w| {
        let a = w.lay.area(ctl)?;
        let s = w.scale();
        let mut pt = POINT {
            x: (a.x * s).round() as i32,
            y: ((a.b() - w.scroll + 2.0) * s).round() as i32,
        };
        unsafe {
            let _ = ClientToScreen(w.hwnd, &mut pt);
        }
        Some((w.options(ctl), w.selected(ctl), pt))
    })
    .flatten() else {
        return;
    };
    let picked = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (n, (value, label, sep)) in opts.iter().enumerate() {
            if *sep {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            let flags = if *value == now {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let text = wide(label);
            let _ = AppendMenuW(menu, flags, n + 1, pcwstr(&text));
        }
        let flags = TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN | TPM_NONOTIFY;
        let picked = TrackPopupMenuEx(menu, flags.0, at.x, at.y, hwnd, None).0;
        let _ = DestroyMenu(menu);
        picked
    };
    if picked > 0 {
        let value = opts[picked as usize - 1].0.clone();
        with_win(|w| w.pick(ctl, &value));
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// GDI drawing in CSS pixels: every coordinate is scaled to the window's DPI
/// and shifted by the scroll position on the way out.
struct Painter<'a> {
    dc: HDC,
    s: f32,
    dy: f32,
    fonts: &'a Fonts,
    d2d: Option<&'a D2d>,
    /// The stage's `opacity: .45` while paused: colours are mixed with the
    /// window background instead, which on a flat backdrop is the same thing.
    fade: Option<u32>,
}

impl Painter<'_> {
    fn px(&self, v: f32) -> i32 {
        (v * self.s).round() as i32
    }

    fn rect(&self, a: Area) -> RECT {
        RECT {
            left: self.px(a.x),
            top: self.px(a.y + self.dy),
            right: self.px(a.r()),
            bottom: self.px(a.b() + self.dy),
        }
    }

    /// The device-pixel box of `a`, for Direct2D.
    fn rectf(&self, a: Area) -> (f32, f32, f32, f32) {
        let r = self.rect(a);
        (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
    }

    fn rgb(&self, c: u32) -> u32 {
        match self.fade {
            Some(bg) => mix(c, bg, 0.45),
            None => c,
        }
    }

    fn col(&self, c: u32) -> COLORREF {
        colorref(self.rgb(c))
    }

    /// One CSS pixel, at least one device pixel.
    fn hair(&self) -> i32 {
        self.px(1.0).max(1)
    }

    fn fill(&self, a: Area, c: u32) {
        unsafe {
            let brush = CreateSolidBrush(self.col(c));
            FillRect(self.dc, &self.rect(a), brush);
            let _ = DeleteObject(brush.into());
        }
    }

    /// A rounded box, filled and/or with a border inside its edge.
    fn round(&self, a: Area, radius: f32, fill: Option<u32>, border: Option<u32>) {
        self.round_w(a, radius, fill, border, 1.0);
    }

    fn round_w(&self, a: Area, radius: f32, fill: Option<u32>, border: Option<u32>, width: f32) {
        if let Some(d) = self.d2d {
            let fill = fill.map(|c| self.rgb(c));
            let border = border.map(|c| (self.rgb(c), self.px(width).max(1) as f32));
            if d.round_rect(self.dc, self.rectf(a), radius * self.s, fill, border) {
                return;
            }
        }
        unsafe {
            let pen: HGDIOBJ = match border {
                Some(c) => CreatePen(PS_INSIDEFRAME, self.px(width).max(1), self.col(c)).into(),
                None => GetStockObject(NULL_PEN),
            };
            let brush: HGDIOBJ = match fill {
                Some(c) => CreateSolidBrush(self.col(c)).into(),
                None => GetStockObject(NULL_BRUSH),
            };
            let old_pen = SelectObject(self.dc, pen);
            let old_brush = SelectObject(self.dc, brush);
            let r = self.rect(a);
            // Without a pen GDI leaves out the right and bottom edges.
            let extra = border.is_none() as i32;
            let d = self.px(2.0 * radius);
            let _ = RoundRect(
                self.dc,
                r.left,
                r.top,
                r.right + extra,
                r.bottom + extra,
                d,
                d,
            );
            SelectObject(self.dc, old_pen);
            SelectObject(self.dc, old_brush);
            if border.is_some() {
                let _ = DeleteObject(pen);
            }
            if fill.is_some() {
                let _ = DeleteObject(brush);
            }
        }
    }

    fn circle(&self, a: Area, fill: u32, border: Option<u32>) {
        if let Some(d) = self.d2d {
            let border = border.map(|c| (self.rgb(c), self.hair() as f32));
            if d.ellipse(self.dc, self.rectf(a), self.rgb(fill), border) {
                return;
            }
        }
        unsafe {
            let pen: HGDIOBJ = match border {
                Some(c) => CreatePen(PS_INSIDEFRAME, self.hair(), self.col(c)).into(),
                None => GetStockObject(NULL_PEN),
            };
            let brush = CreateSolidBrush(self.col(fill));
            let old_pen = SelectObject(self.dc, pen);
            let old_brush = SelectObject(self.dc, brush.into());
            let r = self.rect(a);
            let extra = border.is_none() as i32;
            let _ = Ellipse(self.dc, r.left, r.top, r.right + extra, r.bottom + extra);
            SelectObject(self.dc, old_pen);
            SelectObject(self.dc, old_brush);
            if border.is_some() {
                let _ = DeleteObject(pen);
            }
            let _ = DeleteObject(brush.into());
        }
    }

    /// The web page's focus ring: 2px of accent, `offset` outside the control.
    fn ring(&self, a: Area, radius: f32, offset: f32, accent: u32) {
        let d = offset + 2.0;
        self.round_w(a.grow(d), radius + d, None, Some(accent), 2.0);
    }

    fn text(&self, f: F, s: &str, a: Area, c: u32, align: DRAW_TEXT_FORMAT) {
        let mut buf: Vec<u16> = s.encode_utf16().collect();
        if buf.is_empty() {
            return;
        }
        let mut r = self.rect(a);
        unsafe {
            let old = SelectObject(self.dc, self.fonts.get(f).into());
            SetTextColor(self.dc, self.col(c));
            SetBkMode(self.dc, TRANSPARENT);
            SetTextCharacterExtra(self.dc, self.fonts.tracking(f));
            DrawTextW(
                self.dc,
                &mut buf,
                &mut r,
                align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SetTextCharacterExtra(self.dc, 0);
            SelectObject(self.dc, old);
        }
    }

    /// A stroked polyline through points in CSS pixels.
    fn stroke(&self, pts: &[(f32, f32)], c: u32, width: f32, square: bool) {
        if pts.len() < 2 {
            return;
        }
        if let Some(d) = self.d2d {
            let device: Vec<(f32, f32)> = pts
                .iter()
                .map(|&(x, y)| (x * self.s, (y + self.dy) * self.s))
                .collect();
            if d.polyline(
                self.dc,
                &device,
                self.rgb(c),
                (width * self.s).max(1.0),
                square,
            ) {
                return;
            }
        }
        let points: Vec<POINT> = pts
            .iter()
            .map(|&(x, y)| POINT {
                x: self.px(x),
                y: self.px(y + self.dy),
            })
            .collect();
        let style = if square {
            PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_SQUARE | PS_JOIN_MITER
        } else {
            PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND
        };
        let brush = LOGBRUSH {
            lbStyle: BS_SOLID,
            lbColor: self.col(c),
            lbHatch: 0,
        };
        unsafe {
            let pen = ExtCreatePen(
                PEN_STYLE(style.0),
                (width * self.s).round().max(1.0) as u32,
                &brush,
                None,
            );
            let old = SelectObject(self.dc, pen.into());
            let _ = Polyline(self.dc, &points);
            SelectObject(self.dc, old);
            let _ = DeleteObject(pen.into());
        }
    }

    /// One of the web page's SVG glyphs, its units scaled by `unit`, at `(x, y)`.
    fn glyph(&self, d: &str, (x, y): (f32, f32), unit: f32, c: u32, width: f32, square: bool) {
        for sub in svg_path(d) {
            let pts: Vec<(f32, f32)> = sub
                .iter()
                .map(|&(px, py)| (x + px * unit, y + py * unit))
                .collect();
            self.stroke(&pts, c, width, square);
        }
    }
}

impl Win {
    fn paint(&self, dc: HDC, width: i32, height: i32) {
        let t = self.t();
        let s = self.scale();
        let c = self.config();
        let mut p = Painter {
            dc,
            s,
            dy: 0.0,
            fonts: &self.fonts,
            d2d: self.d2d.as_ref(),
            fade: None,
        };
        p.fill(
            Area::new(0.0, 0.0, width as f32 / s + 1.0, height as f32 / s + 1.0),
            t.bg,
        );
        p.dy = -self.scroll;
        let l = &self.lay;
        let ring = |p: &Painter, ctl: Ctl, a: Area, radius: f32, offset: f32| {
            if self.focus_visible && self.focus == Some(ctl) {
                p.ring(a, radius, offset, t.accent);
            }
        };

        // Header.
        self.paint_mark(&p, l.mark);
        let ty = l.titles.y + 0.5;
        p.text(
            F::H1,
            "Cantos",
            Area::new(l.titles.x, ty, l.titles.w, 22.5),
            t.text,
            DT_LEFT,
        );
        p.text(
            F::Sub,
            "Throw the cursor into a corner to trigger an action",
            Area::new(l.titles.x, ty + 22.5, l.titles.w, 17.0),
            t.dim,
            DT_LEFT,
        );
        self.paint_switch(&p, l.enabled, c.enabled);
        ring(&p, Ctl::Enabled, l.enabled, 11.0, 2.0);

        // Stage, faded while hot corners are paused.
        p.fade = (!c.enabled).then_some(t.bg);
        p.round(l.stage, 12.0, Some(t.raised), Some(t.line_soft));
        self.paint_diagram(&p, l.diagram);
        let armed = c.enabled && c.corners.iter().any(|a| !a.is_none());
        let sy = l.status.y + 4.0 + 8.5;
        p.circle(
            Area::new(l.status.x, sy - 3.0, 6.0, 6.0),
            if armed { t.accent } else { t.faint },
            None,
        );
        p.text(
            F::Sub,
            &status_text(&c, self.state.hitboxes.len()),
            Area::new(l.status.x + 14.0, sy - 8.5, l.status.w - 14.0, 17.0),
            t.dim,
            DT_LEFT,
        );
        p.fade = None;

        // Presets.
        p.text(F::Caps, "PRESETS", l.presets_label, t.faint, DT_LEFT);
        for (k, (name, _)) in PRESETS.iter().enumerate() {
            let a = l.chips[k];
            let hot = self.hover == Some(Ctl::Preset(k));
            p.round(
                a,
                a.h / 2.0,
                Some(t.raised),
                Some(if hot { t.faint } else { t.line }),
            );
            p.text(F::Sub, name, a, if hot { t.text } else { t.dim }, DT_CENTER);
            ring(&p, Ctl::Preset(k), a, a.h / 2.0, 2.0);
        }

        // Cards.
        for i in 0..4 {
            self.paint_card(&p, i, &c);
        }

        // Custom commands.
        if let Some(panel) = l.custom {
            self.paint_panel(&p, panel, "CUSTOM COMMANDS");
            for (i, row) in l.custom_rows.iter().enumerate() {
                let Some(r) = row else { continue };
                if !r.first {
                    p.fill(
                        Area::new(r.row.x + 1.0, r.row.y - 1.0, r.row.w - 2.0, 1.0),
                        t.line_soft,
                    );
                }
                let head = Area::new(r.row.x + 14.0, r.row.y + 11.0, r.row.w - 28.0, 22.0);
                p.text(F::CmdNm, CORNER_NAMES[i], head, t.text, DT_LEFT);
                self.paint_mini(&p, Ctl::CustomTest(i), r.test, true);
                for (field, a) in [(0, r.command), (1, r.args)] {
                    let ctl = if field == 0 {
                        Ctl::Command(i)
                    } else {
                        Ctl::Args(i)
                    };
                    let focused = unsafe { GetFocus() } == self.edits[i * 2 + field];
                    self.paint_box(&p, a, self.hover == Some(ctl), focused);
                }
            }
        }

        // Trigger.
        self.paint_panel(&p, l.trigger, "TRIGGER");
        let trigger_text = [
            (
                "Dwell",
                "How long the cursor must rest before a corner fires",
            ),
            ("Corner size", "Edge length of the sensitive square"),
            ("Cooldown", "Minimum quiet time after a corner fires"),
            (
                "Hold a key",
                "Require a modifier, so corners never fire by accident",
            ),
        ];
        for (row, (name, hint)) in l.trigger_rows.iter().zip(trigger_text) {
            self.paint_row(&p, row, name, hint);
        }
        for k in 0..3 {
            let v = slider_value(&c, k);
            self.paint_slider(&p, k, v);
            p.text(F::Sub, &slider_text(k, v), l.outputs[k], t.dim, DT_RIGHT);
        }
        let modifier = MODIFIERS
            .iter()
            .find(|(id, _)| *id == modifier_id(c.modifier))
            .map(|(_, l)| *l)
            .unwrap_or("Not required");
        self.paint_select(&p, Ctl::Modifier, l.modifier, modifier);

        // Scope.
        self.paint_panel(&p, l.scope, "SCOPE");
        let hint = mode_hint(&c, self.state.displays.len());
        let scope_text = [
            ("Active corners", hint.as_str()),
            (
                "Pause in fullscreen",
                "Never fire over a fullscreen game or video",
            ),
            (
                "Pause while dragging",
                "Snapping a window to a corner stays a snap",
            ),
            (
                "Start with Windows",
                "Launch automatically when you sign in",
            ),
        ];
        for (row, (name, hint)) in l.scope_rows.iter().zip(scope_text) {
            self.paint_row(&p, row, name, hint);
        }
        p.round(l.seg, 8.0, Some(t.sunken), Some(t.line));
        for (k, (mode, label)) in MODES.iter().enumerate() {
            let a = l.segs[k];
            let on = c.monitor_mode == *mode;
            if on {
                p.round(a, 6.0, Some(t.raised), None);
            }
            let hot = on || self.hover == Some(Ctl::Mode(k));
            p.text(
                F::Sub,
                label,
                a,
                if hot { t.text } else { t.dim },
                DT_CENTER,
            );
            ring(&p, Ctl::Mode(k), a, 6.0, 1.0);
        }
        let switches = [
            (Ctl::Fullscreen, c.suppress_fullscreen),
            (Ctl::Dragging, c.suppress_while_dragging),
            (Ctl::Autostart, self.state.autostart),
        ];
        for (k, (ctl, on)) in switches.into_iter().enumerate() {
            self.paint_switch(&p, l.switches[k], on);
            ring(&p, ctl, l.switches[k], 11.0, 2.0);
        }

        // Footer.
        p.fill(
            Area::new(l.footer.x, l.footer.y, l.footer.w, 1.0),
            t.line_soft,
        );
        if !self.state.version.is_empty() {
            let v = format!("Cantos {}", self.state.version);
            p.text(F::Small, &v, l.version, t.faint, DT_LEFT);
        }
        if !self.toast.is_empty() {
            let c = if self.toast_bad { t.danger } else { t.dim };
            p.text(F::Small, &self.toast, l.toast, c, DT_RIGHT);
        }
        for (k, (label, _)) in LINKS.iter().enumerate() {
            let a = l.links[k];
            let hot = self.hover == Some(Ctl::Link(k));
            let (f, c) = if hot {
                (F::Link, t.text)
            } else {
                (F::Small, t.dim)
            };
            p.text(f, label, a, c, DT_CENTER);
            ring(&p, Ctl::Link(k), a, 4.0, 1.0);
        }

        // The scrollbar: the web page's 4px thumb in a 10px gutter.
        if let Some(th) = self.thumb() {
            p.dy = 0.0;
            let c = if self.hover_bar || matches!(self.drag, Drag::Bar(_)) {
                t.faint
            } else {
                t.line
            };
            p.round(
                Area::new(th.x + 3.0, th.y + 3.0, 4.0, th.h - 6.0),
                2.0,
                Some(c),
                None,
            );
        }
    }

    /// The web page's inline mark: a dark tile with the lit quarter-disc.
    fn paint_mark(&self, p: &Painter, a: Area) {
        let k = a.w / 64.0;
        let rad = 40.0 * k;
        let t = self.t();
        let border = mix(t.text, t.bg, 0.18);
        if let Some(d) = p.d2d {
            let disc = ((a.x * p.s, (a.y + p.dy) * p.s), rad * p.s, 0x6ea8fe);
            if d.mark(p.dc, p.rectf(a), 15.0 * k * p.s, 0x12141f, disc) {
                p.round(a, 15.0 * k, None, Some(border));
                return;
            }
        }
        let r = p.rect(a);
        let d = p.px(30.0 * k);
        unsafe {
            let rgn = CreateRoundRectRgn(r.left, r.top, r.right + 1, r.bottom + 1, d, d);
            SelectClipRgn(p.dc, Some(rgn));
            p.fill(a, 0x12141f);
            p.circle(
                Area::new(a.x - rad, a.y - rad, 2.0 * rad, 2.0 * rad),
                0x6ea8fe,
                None,
            );
            SelectClipRgn(p.dc, None);
            let _ = DeleteObject(rgn.into());
        }
        p.round(a, 15.0 * k, None, Some(border));
    }

    fn paint_switch(&self, p: &Painter, a: Area, on: bool) {
        let t = self.t();
        if on {
            p.round(a, 11.0, Some(t.accent), None);
        } else {
            p.round(a, 11.0, Some(t.sunken), Some(t.line));
        }
        let x = if on { a.x + 19.0 } else { a.x + 3.0 };
        p.circle(
            Area::new(x, a.y + 3.0, 15.0, 15.0),
            if on { 0xffffff } else { t.faint },
            None,
        );
    }

    fn paint_panel(&self, p: &Painter, a: Area, title: &str) {
        let t = self.t();
        p.round(a, 12.0, Some(t.raised), Some(t.line_soft));
        p.text(
            F::Caps,
            title,
            Area::new(a.x + 15.0, a.y + 12.0, a.w - 30.0, 16.0),
            t.faint,
            DT_LEFT,
        );
    }

    fn paint_row(&self, p: &Painter, row: &Row, name: &str, hint: &str) {
        let t = self.t();
        if !row.first {
            p.fill(
                Area::new(row.row.x + 1.0, row.row.y - 1.0, row.row.w - 2.0, 1.0),
                t.line_soft,
            );
        }
        let i = row.info;
        p.text(F::Nm, name, Area::new(i.x, i.y, i.w, 19.0), t.text, DT_LEFT);
        p.text(
            F::Small,
            hint,
            Area::new(i.x, i.y + 21.0, i.w, 17.0),
            t.dim,
            DT_LEFT,
        );
    }

    /// `.mini`: a borderless button that gains a border under the pointer.
    fn paint_mini(&self, p: &Painter, ctl: Ctl, a: Area, enabled: bool) {
        let t = self.t();
        let hot = enabled && self.hover == Some(ctl);
        if hot {
            p.round(a, 6.0, None, Some(t.line));
        }
        let c = if !enabled {
            mix(t.faint, t.raised, 0.3)
        } else if hot {
            t.text
        } else {
            t.faint
        };
        p.text(F::Mini, "Test", a, c, DT_CENTER);
        if enabled && self.focus_visible && self.focus == Some(ctl) {
            p.ring(a, 6.0, 1.0, t.accent);
        }
    }

    /// The sunken box of a select or text field.
    fn paint_box(&self, p: &Painter, a: Area, hover: bool, focused: bool) {
        let t = self.t();
        if focused {
            p.round(a.grow(3.0), 10.0, Some(mix(t.accent, t.raised, 0.2)), None);
        }
        let border = if focused {
            t.accent
        } else if hover {
            t.faint
        } else {
            t.line
        };
        p.round(a, 7.0, Some(t.sunken), Some(border));
    }

    fn paint_select(&self, p: &Painter, ctl: Ctl, a: Area, label: &str) {
        let t = self.t();
        let focused = self.focus_visible && self.focus == Some(ctl);
        self.paint_box(p, a, self.hover == Some(ctl), focused);
        let text = Area::new(a.x + 10.0, a.y, a.w - 10.0 - 24.0, a.h);
        p.text(F::Body, label, text, t.text, DT_LEFT);
        // The web page's chevron: two 5px strokes, 10px in from the right.
        let (cx, cy) = (a.r() - 13.0, a.cy() + 1.0);
        p.stroke(
            &[(cx - 3.5, cy - 2.0), (cx, cy + 1.5), (cx + 3.5, cy - 2.0)],
            t.dim,
            1.25,
            false,
        );
    }

    fn paint_slider(&self, p: &Painter, k: usize, v: u32) {
        let t = self.t();
        let a = self.lay.sliders[k];
        let (min, max, _) = SLIDERS[k];
        let frac = (v.saturating_sub(min)) as f32 / (max - min) as f32;
        let cx = a.x + 6.5 + frac * (a.w - 13.0);
        let ty = a.cy() - 1.5;
        p.round(Area::new(a.x, ty, a.w, 3.0), 1.5, Some(t.sunken), None);
        p.round(Area::new(a.x, ty, cx - a.x, 3.0), 1.5, Some(t.accent), None);
        // The thumb, with a hint of its drop shadow.
        let thumb = Area::new(cx - 6.5, a.cy() - 6.5, 13.0, 13.0);
        p.circle(
            Area::new(thumb.x, thumb.y + 1.0, 13.0, 13.0),
            mix(0, t.raised, 0.25),
            None,
        );
        p.circle(thumb, 0xffffff, Some(mix(0, 0xffffff, 0.3)));
        if self.focus_visible && self.focus == Some(Ctl::Slider(k)) {
            p.ring(thumb, 6.5, 2.0, t.accent);
        }
    }

    fn paint_card(&self, p: &Painter, i: usize, c: &Config) {
        let t = self.t();
        let l = &self.lay;
        let a = l.cards[i];
        let kind = kind_of(&c.corners[i]);
        let on = kind != "none";
        let within = matches!(self.focus, Some(Ctl::Test(j) | Ctl::Select(j)) if j == i);
        let border = if on {
            t.accent_dim
        } else if self.hover_card == Some(i) || within {
            t.line
        } else {
            t.line_soft
        };
        p.round(a, 11.0, Some(t.raised), Some(border));
        let (ix, iy) = (a.x + 13.0, a.y + 11.0);
        let glyph_c = if on { t.accent } else { t.faint };
        // `.cnr`: the corner's bracket, 11px in a 12-unit box.
        p.glyph(
            CORNER_GLYPHS[i],
            (ix, iy + 5.5),
            11.0 / 12.0,
            glyph_c,
            2.0,
            true,
        );
        let name = CORNER_NAMES[i].to_uppercase();
        let name_area = Area::new(ix + 17.0, iy, l.card_tests[i].x - ix - 17.0, 22.0);
        p.text(
            F::Caps,
            &name,
            name_area,
            if on { t.dim } else { t.faint },
            DT_LEFT,
        );
        self.paint_mini(p, Ctl::Test(i), l.card_tests[i], on);
        // `.glyph`: the action's symbol, 16px.
        let sel = l.card_selects[i];
        p.glyph(
            glyph_for(&kind),
            (ix, sel.cy() - 8.0),
            1.0,
            glyph_c,
            1.5,
            false,
        );
        let label = CATALOG
            .iter()
            .flat_map(|(_, items)| items.iter())
            .find(|(k, _)| *k == kind)
            .map(|(_, l)| *l)
            .unwrap_or("Nothing");
        self.paint_select(p, Ctl::Select(i), sel, label);
    }

    /// The display arrangement to scale, with a bracket on each live corner;
    /// the hovered card's brackets light up, as `paintHighlight` does.
    fn paint_diagram(&self, p: &Painter, area: Area) {
        let t = self.t();
        let displays = &self.state.displays;
        if displays.is_empty() {
            return;
        }
        let l = displays.iter().map(|d| d.left).min().unwrap_or(0) as f32;
        let top = displays.iter().map(|d| d.top).min().unwrap_or(0) as f32;
        let (w, h) = extent(displays);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let pad = 24.0;
        let scale = ((area.w - 2.0 * pad) / w).min((area.h - 2.0 * pad) / h);
        let ox = area.x + (area.w - w * scale) / 2.0;
        let oy = area.y + (area.h - h * scale) / 2.0;
        let map = |x: f32, y: f32| (ox + (x - l) * scale, oy + (y - top) * scale);
        let primary = mix(t.accent, t.raised, 0.14);
        let secondary = mix(t.text, t.raised, 0.06);

        for d in displays {
            let (x0, y0) = map(d.left as f32, d.top as f32);
            let (x1, y1) = map(d.right as f32, d.bottom as f32);
            let screen = Area::new(x0, y0, x1 - x0, y1 - y0);
            let fill = if d.primary { primary } else { secondary };
            p.round_w(screen, 8.0, Some(fill), Some(t.line), 1.25);
            if d.primary {
                // A hint of the taskbar, so a lone display reads as a screen.
                let bw = (x1 - x0) * 0.22;
                let bar = Area::new((x0 + x1) / 2.0 - bw / 2.0, y1 - 14.0, bw, 4.0);
                p.round(bar, 2.0, Some(mix(t.faint, fill, 0.5)), None);
            }
            if displays.len() > 1 {
                let (f, label) = if d.primary {
                    (F::Tag, "PRIMARY".to_string())
                } else {
                    (
                        F::TagDims,
                        format!("{} \u{00D7} {}", d.right - d.left, d.bottom - d.top),
                    )
                };
                p.text(f, &label, screen, t.faint, DT_CENTER);
            }
        }

        let c = self.config();
        for hb in &self.state.hitboxes {
            let Some(i) = CORNER_KEYS.iter().position(|k| *k == hb.corner) else {
                continue;
            };
            // Anchored on the hitbox's outermost pixel: the bracket sits
            // exactly where the cursor has to reach.
            let left = i % 2 == 0;
            let upper = i < 2;
            let x = if left { hb.left } else { hb.right } as f32;
            let y = if upper { hb.top } else { hb.bottom } as f32;
            let (px, py) = map(x, y);
            let dx = if left { 1.0 } else { -1.0 };
            let dy = if upper { 1.0 } else { -1.0 };
            let colour = if self.hover_card == Some(i) {
                t.accent
            } else if !c.corners[i].is_none() {
                t.accent_dim
            } else {
                t.line
            };
            p.stroke(
                &[(px, py + dy * 15.0), (px, py), (px + dx * 15.0, py)],
                colour,
                2.0,
                true,
            );
        }
    }
}

/// The few SVG path commands the web page's glyphs use (M L H V Z A, either
/// case), flattened to polylines.
fn svg_path(d: &str) -> Vec<Vec<(f32, f32)>> {
    enum Tok {
        Cmd(char),
        Num(f32),
    }
    let mut toks = vec![];
    let chars: Vec<char> = d.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_alphabetic() {
            toks.push(Tok::Cmd(ch));
            i += 1;
        } else if ch.is_ascii_digit() || ch == '-' || ch == '.' {
            let start = i;
            i += 1;
            let mut dot = ch == '.';
            while i < chars.len() {
                let c = chars[i];
                if c.is_ascii_digit() || (c == '.' && !dot) {
                    dot |= c == '.';
                    i += 1;
                } else {
                    break;
                }
            }
            let s: String = chars[start..i].iter().collect();
            toks.push(Tok::Num(s.parse().unwrap_or(0.0)));
        } else {
            i += 1;
        }
    }

    let mut out: Vec<Vec<(f32, f32)>> = vec![];
    let mut cur: Vec<(f32, f32)> = vec![];
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    let mut cmd = 'M';
    let mut k = 0;
    let num = |k: &mut usize| -> Option<f32> {
        match toks.get(*k) {
            Some(Tok::Num(v)) => {
                *k += 1;
                Some(*v)
            }
            _ => None,
        }
    };
    while k < toks.len() {
        if let Tok::Cmd(c) = toks[k] {
            cmd = c;
            k += 1;
            if c == 'z' || c == 'Z' {
                cur.push((sx, sy));
                x = sx;
                y = sy;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (bx, by) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                let (Some(a), Some(b)) = (num(&mut k), num(&mut k)) else {
                    break;
                };
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                x = bx + a;
                y = by + b;
                sx = x;
                sy = y;
                cur.push((x, y));
                // Further pairs after a move are lines.
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                let (Some(a), Some(b)) = (num(&mut k), num(&mut k)) else {
                    break;
                };
                x = bx + a;
                y = by + b;
                cur.push((x, y));
            }
            'H' => {
                let Some(a) = num(&mut k) else { break };
                x = bx + a;
                cur.push((x, y));
            }
            'V' => {
                let Some(b) = num(&mut k) else { break };
                y = by + b;
                cur.push((x, y));
            }
            'A' => {
                let vals: Vec<f32> = (0..7).filter_map(|_| num(&mut k)).collect();
                if vals.len() < 7 {
                    break;
                }
                let end = (bx + vals[5], by + vals[6]);
                cur.extend(arc_points(
                    (x, y),
                    vals[0],
                    vals[1],
                    vals[3] != 0.0,
                    vals[4] != 0.0,
                    end,
                ));
                x = end.0;
                y = end.1;
            }
            _ => {
                k += 1;
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// An SVG elliptical arc (no rotation, which is all the glyphs use) as
/// points, by the endpoint-to-centre conversion in the SVG spec.
fn arc_points(
    from: (f32, f32),
    rx: f32,
    ry: f32,
    large: bool,
    sweep: bool,
    to: (f32, f32),
) -> Vec<(f32, f32)> {
    use std::f32::consts::PI;
    let (x1, y1) = from;
    let (x2, y2) = to;
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        return vec![to];
    }
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let lambda = dx * dx / (rx * rx) + dy * dy / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * dy * dy - ry * ry * dx * dx;
    let den = rx * rx * dy * dy + ry * ry * dx * dx;
    let mut co = if den > 0.0 {
        (num / den).max(0.0).sqrt()
    } else {
        0.0
    };
    if large == sweep {
        co = -co;
    }
    let cxp = co * rx * dy / ry;
    let cyp = -co * ry * dx / rx;
    let cx = cxp + (x1 + x2) / 2.0;
    let cy = cyp + (y1 + y2) / 2.0;
    let t1 = ((dy - cyp) / ry).atan2((dx - cxp) / rx);
    let t2 = ((-dy - cyp) / ry).atan2((-dx - cxp) / rx);
    let mut dt = t2 - t1;
    if !sweep && dt > 0.0 {
        dt -= 2.0 * PI;
    } else if sweep && dt < 0.0 {
        dt += 2.0 * PI;
    }
    let n = ((dt.abs() / (PI / 12.0)).ceil() as usize).max(2);
    (1..=n)
        .map(|k| {
            let a = t1 + dt * k as f32 / n as f32;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Window procedures
// ---------------------------------------------------------------------------

fn lo(v: isize) -> i32 {
    (v & 0xffff) as u16 as i16 as i32
}

fn hi(v: isize) -> i32 {
    ((v >> 16) & 0xffff) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_PAINT => paint(hwnd),
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_SIZE => with_win(|w| w.relayout()).map(|_| LRESULT(0)),
        WM_GETMINMAXINFO => {
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let info = lp.0 as *mut MINMAXINFO;
            if !info.is_null() {
                let min = (560 * dpi / 96) as i32;
                unsafe {
                    (*info).ptMinTrackSize = POINT { x: min, y: min };
                }
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let suggested = lp.0 as *const RECT;
            if !suggested.is_null() {
                let r = unsafe { *suggested };
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            with_win(|w| {
                w.dpi = hi(wp.0 as isize).max(96) as u32;
                w.fonts = Fonts::new(w.scale());
                w.set_edit_fonts();
                w.relayout();
            });
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            let what = lp.0 as *const u16;
            if !what.is_null()
                && unsafe { PCWSTR(what).to_string() }.ok().as_deref() == Some("ImmersiveColorSet")
            {
                with_win(|w| w.apply_theme());
            }
            None
        }
        WM_MOUSEMOVE => with_win(|w| w.mouse_move(lo(lp.0), hi(lp.0))).map(|_| LRESULT(0)),
        WM_MOUSELEAVE => with_win(|w| w.mouse_leave()).map(|_| LRESULT(0)),
        // Only the client area of this window: the frame's resize edges and
        // the command fields keep their own cursors.
        WM_SETCURSOR if lo(lp.0) == HTCLIENT as i32 && wp.0 == hwnd.0 as usize => set_cursor(hwnd),
        WM_LBUTTONDOWN => {
            let fx = with_win(|w| w.mouse_down(lo(lp.0), hi(lp.0)));
            fx.map(|fx| {
                run_fx(hwnd, fx);
                LRESULT(0)
            })
        }
        WM_LBUTTONUP => {
            let fx = with_win(|w| w.mouse_up(lo(lp.0), hi(lp.0)));
            fx.map(|fx| {
                run_fx(hwnd, fx);
                LRESULT(0)
            })
        }
        WM_CAPTURECHANGED => {
            // Capture taken away mid-drag (a menu, Alt+Tab) ends the drag.
            with_win(|w| {
                w.drag = Drag::None;
                w.pressed = None;
            });
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL => {
            let delta = hi(wp.0 as isize) as f32;
            // Chromium's 100px a notch, so it scrolls like the web page did.
            with_win(|w| w.set_scroll(w.scroll - delta / 120.0 * 100.0)).map(|_| LRESULT(0))
        }
        WM_KEYDOWN => {
            let repeat = (lp.0 >> 30) & 1 == 1;
            let fx = with_win(|w| w.key(VIRTUAL_KEY(wp.0 as u16), repeat));
            fx.map(|fx| {
                run_fx(hwnd, fx);
                LRESULT(0)
            })
        }
        // Alt+Down opens a select, as on a native one; it arrives as a
        // system key because of the Alt.
        WM_SYSKEYDOWN if wp.0 == VK_DOWN.0 as usize => {
            let menu = with_win(|w| match w.focus {
                Some(c @ (Ctl::Select(_) | Ctl::Modifier)) => Some(c),
                _ => None,
            })
            .flatten();
            menu.map(|c| {
                run_fx(hwnd, vec![Fx::Menu(c)]);
                LRESULT(0)
            })
        }
        WM_SETFOCUS => {
            // Coming back to the window puts the caret back in the field it
            // was in; the window itself is otherwise the focus.
            let target = with_win(|w| w.focus.and_then(|f| f.edit()).map(|e| w.edits[e])).flatten();
            if let Some(e) = target {
                run_fx(hwnd, vec![Fx::Focus(e)]);
            }
            Some(LRESULT(0))
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            let code = ((wp.0 >> 16) & 0xffff) as u32;
            let e = (id - EDIT_ID) as usize;
            if (0..8).contains(&(id - EDIT_ID)) {
                on_edit_command(e, code);
                Some(LRESULT(0))
            } else {
                None
            }
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut c_void);
            let (text, sunken, _) = EDIT_COLORS.with(|c| c.get());
            unsafe {
                SetTextColor(dc, colorref(text));
                SetBkColor(dc, colorref(sunken));
            }
            Some(LRESULT(EDIT_BRUSH.with(|b| b.get())))
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_SAVE => {
                    with_win(|w| w.save_now());
                }
                TIMER_TOAST => {
                    with_win(|w| {
                        unsafe {
                            let _ = KillTimer(Some(w.hwnd), TIMER_TOAST);
                        }
                        w.toast.clear();
                        w.invalidate();
                    });
                }
                _ => {}
            }
            Some(LRESULT(0))
        }
        WM_PUSH => {
            let push = unsafe { Box::from_raw(lp.0 as *mut child::Push) };
            with_win(|w| w.apply(*push));
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            // An edit still waiting out its debounce is saved, not dropped.
            with_win(|w| w.save_now());
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    };
    match handled {
        Some(r) => r,
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Show the cursor for whatever is under the pointer, as `cursor_at` picks.
fn set_cursor(hwnd: HWND) -> Option<LRESULT> {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    let id = WIN.with(|w| {
        let guard = w.try_borrow().ok()?;
        Some(guard.as_ref()?.cursor_at(pt.x, pt.y))
    })?;
    let cursor = unsafe { LoadCursorW(None, id) }.ok()?;
    unsafe { SetCursor(Some(cursor)) };
    Some(LRESULT(1))
}

fn paint(hwnd: HWND) -> Option<LRESULT> {
    WIN.with(|w| {
        let guard = w.try_borrow().ok()?;
        let win = guard.as_ref()?;
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let (width, height) = (rc.right.max(1), rc.bottom.max(1));
            // Drawn off-screen and copied in one go, so nothing flickers.
            let mem = CreateCompatibleDC(Some(dc));
            let bmp = CreateCompatibleBitmap(dc, width, height);
            let old = SelectObject(mem, bmp.into());
            win.paint(mem, width, height);
            let _ = BitBlt(dc, 0, 0, width, height, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
        Some(LRESULT(0))
    })
}

/// A notification from command field `e` (corner `e / 2`).
fn on_edit_command(e: usize, code: u32) {
    let i = e / 2;
    let ctl = if e % 2 == 0 {
        Ctl::Command(i)
    } else {
        Ctl::Args(i)
    };
    match code {
        EN_CHANGE => {
            if SYNCING.with(|s| s.get()) {
                return;
            }
            with_win(|w| {
                let command = edit_text(w.edits[i * 2]);
                let args = edit_text(w.edits[i * 2 + 1]);
                w.edit(|c| c.corners[i] = Action::Custom { command, args });
            });
        }
        EN_SETFOCUS => {
            with_win(|w| {
                w.focus = Some(ctl);
                if let Some(a) = w.lay.area(ctl) {
                    w.reveal(a);
                }
                w.invalidate();
            });
        }
        EN_KILLFOCUS => {
            with_win(|w| w.invalidate());
        }
        _ => {}
    }
}

/// The command fields' procedure: the stock `EDIT` one, plus a placeholder
/// (the web page's, which needs common controls 6 to come for free), and the
/// pointer for the boxes' hover border.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Tab is the window's, and Enter and Escape mean nothing here; the
        // stock control would beep at them.
        WM_CHAR if matches!(wp.0, 0x09 | 0x0d | 0x1b | 0x01) => return LRESULT(0),
        WM_KEYDOWN if wp.0 == b'A' as usize && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
            unsafe {
                SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let id = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgCtrlID(hwnd) };
            let e = (id - EDIT_ID).max(0) as usize;
            let ctl = if e % 2 == 0 {
                Ctl::Command(e / 2)
            } else {
                Ctl::Args(e / 2)
            };
            with_win(|w| {
                if w.hover != Some(ctl) {
                    w.hover = Some(ctl);
                    w.hover_card = None;
                    w.invalidate();
                }
            });
        }
        _ => {}
    }
    let old: WNDPROC = unsafe { std::mem::transmute(EDIT_PROC.with(|p| p.get())) };
    let result = unsafe { CallWindowProcW(old, hwnd, msg, wp, lp) };
    if msg == WM_PAINT && unsafe { GetWindowTextLengthW(hwnd) } == 0 {
        draw_placeholder(hwnd);
    }
    result
}

fn draw_placeholder(hwnd: HWND) {
    let id = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgCtrlID(hwnd) };
    let text: Vec<u16> = PLACEHOLDERS[((id - EDIT_ID).max(0) % 2) as usize]
        .encode_utf16()
        .collect();
    let (_, _, faint) = EDIT_COLORS.with(|c| c.get());
    unsafe {
        let dc = GetDC(Some(hwnd));
        let old = SelectObject(dc, HGDIOBJ(EDIT_FONT.with(|f| f.get()) as *mut c_void));
        SetTextColor(dc, colorref(faint));
        SetBkMode(dc, TRANSPARENT);
        let _ = TextOutW(dc, 0, 0, &text);
        SelectObject(dc, old);
        ReleaseDC(Some(hwnd), dc);
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

fn start() -> Result<HWND, String> {
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| format!("no module handle: {e}"))?
        .into();
    let class = w!("CantosSettings");
    // The icon the build embeds as resource 1 (build.rs).
    let icon_id = PCWSTR(1 as *const u16);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: class,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err("could not register the window class".into());
    }
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Cantos"),
            style,
            0,
            0,
            680,
            828,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|e| format!("could not create the window: {e}"))?;

    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
        let edge = unsafe { GetSystemMetricsForDpi(metric, dpi) };
        if let Ok(icon) = unsafe {
            LoadImageW(
                Some(instance),
                icon_id,
                IMAGE_ICON,
                edge,
                edge,
                LR_DEFAULTCOLOR,
            )
        } {
            let icon = HICON(icon.0);
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
        }
    }

    // The command fields, hidden until a corner runs a custom command.
    let mut edits = [HWND::default(); 8];
    for (e, slot) in edits.iter_mut().enumerate() {
        let edit = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                10,
                10,
                Some(hwnd),
                Some(HMENU((EDIT_ID + e as i32) as usize as *mut c_void)),
                Some(instance),
                None,
            )
        }
        .map_err(|e| format!("could not create a text field: {e}"))?;
        let old = unsafe { SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize) };
        EDIT_PROC.with(|p| p.set(old));
        *slot = edit;
    }

    let mut win = Win {
        hwnd,
        dpi,
        dark: true,
        fonts: Fonts::new(dpi as f32 / 96.0),
        d2d: D2d::new()
            .map_err(|e| lerror!("Direct2D unavailable, shapes drawn with GDI: {e}"))
            .ok(),
        mdc: unsafe { CreateCompatibleDC(None) },
        state: State::default(),
        lay: Layout::default(),
        scroll: 0.0,
        hover: None,
        hover_card: None,
        hover_bar: false,
        focus: None,
        focus_visible: false,
        pressed: None,
        drag: Drag::None,
        tracking: false,
        edits,
        placed: [None; 8],
        toast: String::new(),
        toast_bad: false,
        save_pending: false,
    };
    win.apply_theme();
    win.set_edit_fonts();

    // 680x828 of client area at this DPI, centred on the work area.
    let s = dpi as f32 / 96.0;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: (680.0 * s).round() as i32,
        bottom: (828.0 * s).round() as i32,
    };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WINDOW_EX_STYLE::default(), dpi);
    }
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(monitor, &mut mi)
            .as_bool()
            .then_some(mi.rcWork)
    }
    .unwrap_or(RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    });
    let h = h.min(work.bottom - work.top);
    let x = work.left + ((work.right - work.left) - w).max(0) / 2;
    let y = work.top + ((work.bottom - work.top) - h).max(0) / 2;
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }

    WIN.with(|cell| *cell.borrow_mut() = Some(win));
    with_win(|w| w.relayout());
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(hwnd)
}

/// `cantos --settings-window`: run the window until it is closed.
pub fn run() {
    linfo!(
        "settings window process started (pid {})",
        std::process::id()
    );
    let hwnd = match start() {
        Ok(h) => h,
        Err(e) => {
            lerror!("settings window: {e}");
            return;
        }
    };

    // The tray's calls arrive on stdin and are posted to the window's thread.
    // End of input means the tray has gone, so the window goes too.
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let hwnd = HWND(target as *mut c_void);
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<child::Push>(&line) {
                Ok(push) => {
                    let raw = Box::into_raw(Box::new(push));
                    let posted = unsafe {
                        PostMessageW(Some(hwnd), WM_PUSH, WPARAM(0), LPARAM(raw as isize))
                    };
                    if posted.is_err() {
                        // The window is gone; take the box back and stop.
                        drop(unsafe { Box::from_raw(raw) });
                        return;
                    }
                }
                Err(e) => lerror!("settings window: unreadable line from the tray ({e})"),
            }
        }
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    });

    child::send(&serde_json::json!({ "cmd": "ready" }));

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        // Tab moves through the window's controls wherever focus is, the
        // command fields included, so it is taken before any window sees it.
        if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_TAB.0 as usize {
            let back = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if let Some(fx) = with_win(|w| w.tab(back)) {
                run_fx(hwnd, fx);
            }
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    WIN.with(|cell| cell.borrow_mut().take());
}
