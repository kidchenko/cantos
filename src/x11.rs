//! The X11 connection Cantos uses on Linux, one per thread.
//!
//! Xlib is loaded at runtime rather than linked (`x11-dl`), so a machine that
//! is missing a library gets a log line instead of a binary that will not
//! start. Each thread that asks (the watcher, the event loop, an action)
//! opens its own `Display` on first use and closes it when the thread ends:
//! an Xlib connection must not be shared between threads, and GTK keeps its
//! own on the main thread anyway.
//!
//! # Wayland
//!
//! Under Wayland an X11 client only sees the pointer while it is over another
//! X11 (XWayland) window, so the cursor position read here stops updating the
//! moment it crosses a native Wayland window. That is a property of Wayland's
//! security model, not something to work around; `main` logs it at startup.

use crate::geometry::{MonitorInfo, Point, Rect};
use std::cell::RefCell;
use std::ffi::{c_int, c_long, c_uchar, c_uint, c_ulong, CString};
use traykit::lwarn;
use x11_dl::xlib::{self, Display, Xlib};
use x11_dl::xrandr::Xrandr;
use x11_dl::xtest::Xf86vmode as XTest;

pub struct X {
    pub xlib: Xlib,
    pub dpy: *mut Display,
    pub root: c_ulong,
    /// Optional extras: without RandR the root window is one display, and
    /// without XTest the keystroke actions say so in the log.
    xrandr: Option<Xrandr>,
    xtest: Option<XTest>,
}

impl Drop for X {
    fn drop(&mut self) {
        unsafe { (self.xlib.XCloseDisplay)(self.dpy) };
    }
}

thread_local! {
    /// Outer `None`: not tried on this thread yet. Inner `None`: tried and
    /// failed, so a missing `DISPLAY` is reported once rather than per tick.
    static CONN: RefCell<Option<Option<X>>> = const { RefCell::new(None) };
}

/// Run `f` against this thread's connection, opening it on first use.
/// `None` when there is no X server to talk to.
pub fn with<R>(f: impl FnOnce(&X) -> R) -> Option<R> {
    CONN.with(|c| {
        if c.borrow().is_none() {
            let opened = X::open();
            *c.borrow_mut() = Some(opened);
        }
        c.borrow().as_ref().and_then(|x| x.as_ref()).map(f)
    })
}

/// Pointer position and the modifier/button mask, from one round trip.
pub struct Pointer {
    pub at: Point,
    pub mask: c_uint,
}

impl X {
    fn open() -> Option<X> {
        let xlib = match Xlib::open() {
            Ok(l) => l,
            Err(e) => {
                lwarn!("could not load libX11 ({e}); hot corners need an X11 session");
                return None;
            }
        };
        let dpy = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
        if dpy.is_null() {
            lwarn!("could not open the X display (is DISPLAY set?); hot corners need X11");
            return None;
        }
        let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
        Some(X {
            xlib,
            dpy,
            root,
            xrandr: Xrandr::open().ok(),
            xtest: XTest::open().ok(),
        })
    }

    pub fn pointer(&self) -> Option<Pointer> {
        let (mut root, mut child) = (0, 0);
        let (mut rx, mut ry, mut wx, mut wy) = (0, 0, 0, 0);
        let mut mask: c_uint = 0;
        let same_screen = unsafe {
            (self.xlib.XQueryPointer)(
                self.dpy, self.root, &mut root, &mut child, &mut rx, &mut ry, &mut wx, &mut wy,
                &mut mask,
            )
        };
        (same_screen != 0).then_some(Pointer {
            at: Point { x: rx, y: ry },
            mask,
        })
    }

    pub fn root_size(&self) -> (i32, i32) {
        unsafe {
            let screen = (self.xlib.XDefaultScreen)(self.dpy);
            (
                (self.xlib.XDisplayWidth)(self.dpy, screen),
                (self.xlib.XDisplayHeight)(self.dpy, screen),
            )
        }
    }

    pub fn monitors(&self) -> Option<Vec<MonitorInfo>> {
        let xrandr = self.xrandr.as_ref()?;
        let mut n: c_int = 0;
        let list = unsafe { (xrandr.XRRGetMonitors)(self.dpy, self.root, xlib::True, &mut n) };
        if list.is_null() {
            return None;
        }
        let out: Vec<MonitorInfo> = unsafe { std::slice::from_raw_parts(list, n.max(0) as usize) }
            .iter()
            .map(|m| MonitorInfo {
                rect: Rect {
                    left: m.x,
                    top: m.y,
                    right: m.x + m.width,
                    bottom: m.y + m.height,
                },
                is_primary: m.primary != 0,
            })
            .collect();
        unsafe { (xrandr.XRRFreeMonitors)(list) };
        (!out.is_empty()).then_some(out)
    }

    pub fn atom(&self, name: &str) -> c_ulong {
        let name = CString::new(name).unwrap_or_default();
        unsafe { (self.xlib.XInternAtom)(self.dpy, name.as_ptr(), xlib::False) }
    }

    /// A 32-bit-format property as a list. Xlib hands format-32 data back as
    /// C `long`s whatever the platform's word size, hence `c_ulong` here.
    pub fn property(&self, window: c_ulong, name: &str, kind: c_ulong) -> Option<Vec<c_ulong>> {
        let (mut actual_type, mut actual_format) = (0, 0);
        let (mut count, mut remaining) = (0, 0);
        let mut data: *mut c_uchar = std::ptr::null_mut();
        let rc = unsafe {
            (self.xlib.XGetWindowProperty)(
                self.dpy,
                window,
                self.atom(name),
                0,
                1024,
                xlib::False,
                kind,
                &mut actual_type,
                &mut actual_format,
                &mut count,
                &mut remaining,
                &mut data,
            )
        };
        if rc != xlib::Success as c_int || data.is_null() {
            return None;
        }
        let values = (actual_format == 32).then(|| {
            unsafe { std::slice::from_raw_parts(data as *const c_ulong, count as usize) }.to_vec()
        });
        unsafe { (self.xlib.XFree)(data.cast()) };
        values
    }

    /// An EWMH request to the window manager: a client message on the root
    /// window, which is how a pager asks for a desktop switch.
    pub fn ask_window_manager(&self, window: c_ulong, message: &str, data: [c_long; 5]) {
        let mut event: xlib::XClientMessageEvent = unsafe { std::mem::zeroed() };
        event.type_ = xlib::ClientMessage;
        event.window = window;
        event.message_type = self.atom(message);
        event.format = 32;
        for (i, v) in data.into_iter().enumerate() {
            event.data.set_long(i, v);
        }
        let mut event = xlib::XEvent {
            client_message: event,
        };
        unsafe {
            (self.xlib.XSendEvent)(
                self.dpy,
                self.root,
                xlib::False,
                xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
                &mut event,
            );
            (self.xlib.XFlush)(self.dpy);
        }
    }

    /// Press `keysyms` in order and release them in reverse, through XTest.
    /// Returns false when XTest is missing or a key has no keycode on this
    /// keyboard layout.
    pub fn tap(&self, keysyms: &[c_uint]) -> bool {
        let Some(xtest) = self.xtest.as_ref() else {
            lwarn!("libXtst is not available; keystroke actions cannot run");
            return false;
        };
        let codes: Vec<c_uint> = keysyms
            .iter()
            .map(|&k| unsafe { (self.xlib.XKeysymToKeycode)(self.dpy, k as c_ulong) } as c_uint)
            .collect();
        if codes.contains(&0) {
            lwarn!("a key in {keysyms:x?} has no keycode on this keyboard layout");
            return false;
        }
        unsafe {
            for &c in &codes {
                (xtest.XTestFakeKeyEvent)(self.dpy, c, xlib::True, 0);
            }
            for &c in codes.iter().rev() {
                (xtest.XTestFakeKeyEvent)(self.dpy, c, xlib::False, 0);
            }
            (self.xlib.XFlush)(self.dpy);
        }
        true
    }

    /// Release modifier keys the user is physically holding, so a required
    /// modifier does not turn Super into Ctrl+Super. See the Windows twin,
    /// `release_held_modifiers` in actions.rs.
    pub fn release_held_modifiers(&self, keysyms: &[c_uint]) {
        let Some(xtest) = self.xtest.as_ref() else {
            return;
        };
        let mut keymap = [0 as std::ffi::c_char; 32];
        unsafe { (self.xlib.XQueryKeymap)(self.dpy, keymap.as_mut_ptr()) };
        for &k in keysyms {
            let code = unsafe { (self.xlib.XKeysymToKeycode)(self.dpy, k as c_ulong) } as usize;
            // `c_char` is i8 on x86_64 and u8 on arm64; the cast is needed on one.
            #[allow(clippy::unnecessary_cast)]
            let byte = keymap[code / 8] as u8;
            if code != 0 && byte & (1 << (code % 8)) != 0 {
                unsafe { (xtest.XTestFakeKeyEvent)(self.dpy, code as c_uint, xlib::False, 0) };
            }
        }
        unsafe { (self.xlib.XFlush)(self.dpy) };
    }
}

/// Why corners may not fire in this session, if there is a reason to say so.
///
/// Wayland is the one: see the module docs. Detected from the session rather
/// than from a failed query, because under XWayland the query does not fail;
/// it quietly reports a stale position.
pub fn session_warning() -> Option<&'static str> {
    let wayland = std::env::var("XDG_SESSION_TYPE")
        .is_ok_and(|t| t.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    wayland.then_some(
        "This is a Wayland session, where Cantos cannot see the cursor over most \
         windows. Corners only fire reliably in an X11 session.",
    )
}
