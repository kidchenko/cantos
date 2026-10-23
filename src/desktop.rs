//! "What is the desktop doing right now?" The OS queries the trigger needs
//! in order to decide whether a corner may fire.
//!
//! This is the only place that asks the OS about live state. Keeping it apart
//! from [`crate::trigger`] is what lets the corner rules be tested: nothing is
//! ever held down or fullscreen during `cargo test`, so a state machine that
//! called these functions directly could only ever exercise the "nothing is in
//! the way" path. The [`Desktop`] trait lets the tests answer for it.
//!
//! Not to be confused with [`crate::actions`], which asks the OS to *do*
//! things. Nothing here has side effects.
//!
//! On macOS this only answers for corners with a Cantos action; the Dock
//! detects its own (see [`crate::native`]).

use crate::config::Modifier;

pub use imp::{cursor_pos, System};

/// The desktop state a corner's decision depends on.
///
/// Every method is a question with a "no" that means "go ahead", so a test
/// that wants the unobstructed path can answer `false` to everything.
pub trait Desktop {
    /// Is the key the user asked to hold actually down?
    fn modifier_held(&self, m: Modifier) -> bool;
    /// Is any mouse button down: a drag, a selection, a snap in progress?
    fn mouse_button_down(&self) -> bool;
    /// Is a game or a video player filling a screen?
    fn fullscreen_active(&self) -> bool;
}

/// Does `window` reach or pass every edge of `monitor`? The geometric half of
/// fullscreen detection on Windows and macOS.
///
/// `>=` rather than `==`: some players size themselves marginally larger than
/// the monitor to hide their borders.
#[cfg(any(windows, target_os = "macos", test))]
pub fn rect_covers(window: &crate::geometry::Rect, monitor: &crate::geometry::Rect) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::{rect_covers, Desktop};
    use crate::config::Modifier;
    use crate::geometry::{Point, Rect};
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU,
        VK_RBUTTON, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
        IsZoomed, GWL_STYLE, WS_CAPTION, WS_THICKFRAME,
    };

    /// The real thing: answers by asking Windows.
    pub struct System;

    impl Desktop for System {
        fn modifier_held(&self, m: Modifier) -> bool {
            match m {
                Modifier::None => true,
                Modifier::Ctrl => key_down(VK_CONTROL),
                Modifier::Alt => key_down(VK_MENU),
                Modifier::Shift => key_down(VK_SHIFT),
                Modifier::Win => key_down(VK_LWIN) || key_down(VK_RWIN),
            }
        }

        /// Covers more than window dragging: dragging a file, rubber-band
        /// selecting, and drawing all end up parked somewhere with a button down,
        /// and none of them should trip a corner. `VK_LBUTTON` tracks the
        /// *primary* button, so this stays correct when the buttons are swapped.
        fn mouse_button_down(&self) -> bool {
            key_down(VK_LBUTTON) || key_down(VK_RBUTTON) || key_down(VK_MBUTTON)
        }

        fn fullscreen_active(&self) -> bool {
            shell_says_fullscreen() || foreground_covers_monitor()
        }
    }

    pub fn cursor_pos() -> Option<Point> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p).ok() }.map(|_| Point { x: p.x, y: p.y })
    }

    fn key_down(vk: VIRTUAL_KEY) -> bool {
        // High bit of GetAsyncKeyState is the "currently down" flag.
        unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
    }

    // -- Fullscreen ------------------------------------------------------------
    //
    // Two independent checks, because neither alone is sufficient. The shell's
    // notification state catches exclusive-mode D3D games and presentation mode
    // but misses borderless-windowed apps entirely, so we also measure the
    // foreground window against its monitor.

    fn shell_says_fullscreen() -> bool {
        let Ok(state) = (unsafe { SHQueryUserNotificationState() }) else {
            return false;
        };
        state == QUNS_RUNNING_D3D_FULL_SCREEN
            || state == QUNS_PRESENTATION_MODE
            || state == QUNS_BUSY
    }

    fn foreground_covers_monitor() -> bool {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return false;
        }
        // The desktop itself is permanently "fullscreen"; treating it as such
        // would disable the app whenever nothing else has focus.
        if is_shell_window(hwnd) {
            return false;
        }
        // An ordinary maximised window is exempt; see `is_ordinary_maximised`.
        if is_ordinary_maximised(hwnd) {
            return false;
        }
        let (Some(window), Some(monitor)) = (window_rect(hwnd), monitor_rect(hwnd)) else {
            return false;
        };
        rect_covers(&Rect::from(window), &Rect::from(monitor))
    }

    /// Is this a normal application window someone has maximised?
    ///
    /// Worth its own name because it is the single subtlest rule in the app.
    /// Measuring cannot answer it: `GetWindowRect` reports the frame including the
    /// invisible resize border, so a maximised window overhangs its monitor by
    /// ~8px on every edge it is free to grow into. Wherever the work area equals
    /// the monitor rect (a second display with no taskbar, which is the Windows
    /// 11 default, or an auto-hide taskbar), that overhang alone is enough to
    /// satisfy [`rect_covers`], and every corner on every display goes quiet while
    /// a plainly ordinary maximised window has focus.
    ///
    /// Both halves matter. `IsZoomed` alone would hand a free pass to anything
    /// presenting itself maximised, so the window must also carry real furniture:
    /// a borderless-fullscreen app has neither caption nor resize frame, and is
    /// still caught by the geometry.
    fn is_ordinary_maximised(hwnd: HWND) -> bool {
        unsafe { IsZoomed(hwnd).as_bool() && framed(window_style(hwnd)) }
    }

    fn window_style(hwnd: HWND) -> u32 {
        // x64-only target, so the `Ptr` variant is the right one.
        unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 }
    }

    /// Does this style carry ordinary window furniture?
    ///
    /// Every maximised window on a normal desktop has a caption and a resize
    /// frame. A borderless-fullscreen app has neither (that is what "borderless"
    /// means), so this separates the two without looking at any geometry.
    fn framed(style: u32) -> bool {
        style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0
    }

    fn window_rect(hwnd: HWND) -> Option<RECT> {
        let mut r = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut r).ok().map(|_| r) }
    }

    fn monitor_rect(hwnd: HWND) -> Option<RECT> {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            GetMonitorInfoW(mon, &mut mi)
                .as_bool()
                .then_some(mi.rcMonitor)
        }
    }

    fn is_shell_window(hwnd: HWND) -> bool {
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&buf[..len as usize]);
        matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Windows.UI.Core.CoreWindow"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_maximised_app_window_is_framed() {
            // Measured styles of every maximised window on the test desktop
            // (a browser, a chat app, a terminal): caption plus resize frame.
            assert!(framed(WS_CAPTION.0 | WS_THICKFRAME.0));
            assert!(framed(WS_CAPTION.0));
            assert!(framed(WS_THICKFRAME.0));
        }

        #[test]
        fn a_borderless_window_is_not_framed() {
            // What a fullscreen game presents. Even maximised, this must stay
            // eligible for suppression, hence checking the frame and not just
            // `IsZoomed`.
            use windows::Win32::UI::WindowsAndMessaging::{WS_POPUP, WS_VISIBLE};
            assert!(!framed(WS_POPUP.0 | WS_VISIBLE.0));
        }
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod imp {
    use super::{rect_covers, Desktop};
    use crate::cg;
    use crate::config::Modifier;
    use crate::geometry::{self, Point, Rect};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{ns_string, NSArray, NSDictionary, NSNumber, NSString};

    /// The real thing: answers by asking CoreGraphics.
    pub struct System;

    impl Desktop for System {
        /// `Win` is the platform's own system key, so on a Mac it is Command.
        fn modifier_held(&self, m: Modifier) -> bool {
            let flags = unsafe { cg::CGEventSourceFlagsState(cg::COMBINED_SESSION_STATE) };
            match m {
                Modifier::None => true,
                Modifier::Ctrl => flags & cg::FLAG_CONTROL != 0,
                Modifier::Alt => flags & cg::FLAG_OPTION != 0,
                Modifier::Shift => flags & cg::FLAG_SHIFT != 0,
                Modifier::Win => flags & cg::FLAG_COMMAND != 0,
            }
        }

        /// Left, right and the middle button. Dragging a window, a file or a
        /// selection into a corner all end up parked with one of them down.
        fn mouse_button_down(&self) -> bool {
            (0..3).any(|b| unsafe { cg::CGEventSourceButtonState(cg::COMBINED_SESSION_STATE, b) })
        }

        /// Is the frontmost window covering a whole display?
        ///
        /// A native fullscreen app, a fullscreen video in a browser, and a
        /// game all present as exactly that. A zoomed window does not: it
        /// stops at the menu bar, so (unlike on Windows) the geometry alone
        /// tells the two apart. The exception is a menu bar set to hide
        /// automatically, where a zoomed window does reach the top and is
        /// treated as fullscreen.
        fn fullscreen_active(&self) -> bool {
            let Some(front) = frontmost_window() else {
                return false;
            };
            geometry::monitors()
                .iter()
                .any(|m| rect_covers(&front, &m.rect))
        }
    }

    pub fn cursor_pos() -> Option<Point> {
        cg::cursor()
    }

    /// Bounds of the frontmost ordinary window, from the window server's
    /// front-to-back list.
    ///
    /// Reading bounds, layer and owner needs no permission; only window
    /// titles sit behind Screen Recording, and they are not read.
    fn frontmost_window() -> Option<Rect> {
        let raw = unsafe { cg::CGWindowListCopyWindowInfo(cg::ON_SCREEN_WINDOWS, 0) };
        if raw.is_null() {
            return None;
        }
        // A CFArray of CFDictionary is toll-free bridged to NSArray of
        // NSDictionary, and "Copy" means we own it: `from_raw` takes over the
        // reference and releases it on drop.
        let windows: Retained<NSArray<NSDictionary<NSString, AnyObject>>> =
            unsafe { Retained::from_raw(raw as *mut _) }?;
        let own_pid = std::process::id() as i64;

        windows.iter().find_map(|w| {
            // Layer 0 is where application windows live; the menu bar, the
            // Dock and overlays sit above it.
            if number(&w, ns_string!("kCGWindowLayer"))? != 0 {
                return None;
            }
            // Cantos's own settings window is never what the user is watching.
            if number(&w, ns_string!("kCGWindowOwnerPID"))? == own_pid {
                return None;
            }
            let bounds = w.objectForKey(ns_string!("kCGWindowBounds"))?;
            let bounds = bounds.downcast_ref::<NSDictionary>()?;
            let get = |k: &NSString| {
                bounds
                    .objectForKey(k)
                    .and_then(|v| v.downcast_ref::<NSNumber>().map(|n| n.as_f64()))
            };
            let r = cg::rect(cg::CGRect {
                origin: cg::CGPoint {
                    x: get(ns_string!("X"))?,
                    y: get(ns_string!("Y"))?,
                },
                size: cg::CGSize {
                    width: get(ns_string!("Width"))?,
                    height: get(ns_string!("Height"))?,
                },
            });
            // Some apps keep a transparent sliver of a window at the front;
            // anything this small is not what the user is looking at.
            (r.right - r.left >= 64 && r.bottom - r.top >= 64).then_some(r)
        })
    }

    fn number(dict: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<i64> {
        dict.objectForKey(key)?
            .downcast_ref::<NSNumber>()
            .map(|n| n.as_i64())
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use super::Desktop;
    use crate::config::Modifier;
    use crate::geometry::Point;
    use crate::x11;
    use x11_dl::xlib;

    /// The real thing: answers by asking the X server.
    pub struct System;

    impl Desktop for System {
        /// `Win` is the platform's own system key: Super.
        fn modifier_held(&self, m: Modifier) -> bool {
            let mask = match m {
                Modifier::None => return true,
                Modifier::Ctrl => xlib::ControlMask,
                Modifier::Alt => xlib::Mod1Mask,
                Modifier::Shift => xlib::ShiftMask,
                Modifier::Win => xlib::Mod4Mask,
            };
            pointer_mask() & mask != 0
        }

        fn mouse_button_down(&self) -> bool {
            pointer_mask() & (xlib::Button1Mask | xlib::Button2Mask | xlib::Button3Mask) != 0
        }

        /// Asks the window manager rather than measuring: EWMH has an exact
        /// answer, `_NET_WM_STATE_FULLSCREEN` on the active window, and every
        /// mainstream X11 desktop maintains it.
        fn fullscreen_active(&self) -> bool {
            x11::with(|x| {
                let Some(active) = x
                    .property(x.root, "_NET_ACTIVE_WINDOW", xlib::XA_WINDOW)
                    .and_then(|v| v.first().copied())
                    .filter(|&w| w != 0)
                else {
                    return false;
                };
                let fullscreen = x.atom("_NET_WM_STATE_FULLSCREEN");
                x.property(active, "_NET_WM_STATE", xlib::XA_ATOM)
                    .is_some_and(|states| states.contains(&fullscreen))
            })
            .unwrap_or(false)
        }
    }

    pub fn cursor_pos() -> Option<Point> {
        x11::with(|x| x.pointer().map(|p| p.at)).flatten()
    }

    fn pointer_mask() -> std::ffi::c_uint {
        x11::with(|x| x.pointer().map(|p| p.mask))
            .flatten()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// 1920x1200 display, the arrangement these numbers were measured on.
    const MONITOR: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1200,
    };

    #[test]
    fn a_borderless_fullscreen_window_covers_the_monitor() {
        assert!(rect_covers(&rect(0, 0, 1920, 1200), &MONITOR));
    }

    #[test]
    fn a_window_sized_past_the_monitor_still_counts() {
        // Some players overshoot deliberately to hide their borders.
        assert!(rect_covers(&rect(-2, -2, 1922, 1202), &MONITOR));
    }

    #[test]
    fn a_maximised_window_over_a_taskbar_does_not_cover_the_monitor() {
        // Measured: a maximised window reports its frame inflated by the
        // invisible resize border, here 9px, over a work area 60px short of
        // the monitor. Geometry alone rejects this one.
        assert!(!rect_covers(&rect(-9, -9, 1929, 1149), &MONITOR));
    }

    #[test]
    fn a_maximised_window_with_no_taskbar_looks_like_fullscreen() {
        // The case that broke it. With no taskbar on the display (the
        // Windows 11 default for a second screen), the work area *is* the
        // monitor, and the 9px frame inflation pushes a plainly ordinary
        // maximised window past every edge. Geometry cannot save us here,
        // which is why `is_ordinary_maximised` exists.
        assert!(rect_covers(&rect(-9, -9, 1929, 1209), &MONITOR));
    }

    #[test]
    fn an_ordinary_window_does_not_cover_the_monitor() {
        assert!(!rect_covers(&rect(50, 40, 1348, 1050), &MONITOR));
    }
}
