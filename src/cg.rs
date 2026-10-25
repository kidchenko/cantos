//! The CoreGraphics and Accessibility calls Cantos makes on macOS, declared by
//! hand: the display list, and for corners Cantos fires itself (see
//! `native`), the cursor, modifier and button state, the window list, and
//! keystrokes. A binding crate would add more to the build than these few
//! functions are worth. All of it is safe to call from any thread, which
//! matters because the watcher runs on its own.

#![allow(non_snake_case)]

use crate::geometry::{Point, Rect};
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub type CGDirectDisplayID = u32;
pub type CGEventRef = *mut c_void;

/// `kCGEventSourceStateCombinedSessionState`: what the user's session sees,
/// physical and synthetic input together.
pub const COMBINED_SESSION_STATE: i32 = 0;
/// `kCGHIDEventTap`: post as if from the keyboard itself.
pub const HID_EVENT_TAP: u32 = 0;

pub const FLAG_SHIFT: u64 = 1 << 17;
pub const FLAG_CONTROL: u64 = 1 << 18;
pub const FLAG_OPTION: u64 = 1 << 19;
pub const FLAG_COMMAND: u64 = 1 << 20;
/// `kCGEventFlagMaskNumericPad` and `kCGEventFlagMaskSecondaryFn`: a real
/// arrow key press carries both.
pub const FLAG_NUMERIC_PAD: u64 = 1 << 21;
pub const FLAG_FN: u64 = 1 << 23;

/// `kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements`.
pub const ON_SCREEN_WINDOWS: u32 = 1 | 16;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGGetActiveDisplayList(
        max: u32,
        displays: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> i32;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayMirrorsDisplay(display: CGDirectDisplayID) -> CGDirectDisplayID;

    pub fn CGEventCreate(source: *const c_void) -> CGEventRef;
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventSourceFlagsState(state: i32) -> u64;
    pub fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    pub fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> CGEventRef;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventPost(tap: u32, event: CGEventRef);

    /// Returns a `CFArrayRef` of `CFDictionaryRef`, owned by the caller.
    pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> bool;
    pub fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

/// Display and window bounds arrive as fractional points; hitboxes are whole.
pub fn rect(r: CGRect) -> Rect {
    Rect {
        left: r.origin.x.round() as i32,
        top: r.origin.y.round() as i32,
        right: (r.origin.x + r.size.width).round() as i32,
        bottom: (r.origin.y + r.size.height).round() as i32,
    }
}

/// Where the cursor is, in the global display space.
pub fn cursor() -> Option<Point> {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let p = CGEventGetLocation(event);
        CFRelease(event);
        // Floor, not round: at the right edge of a 1440-point display the
        // cursor reports 1439.6, which belongs to the last column, not past it.
        Some(Point {
            x: p.x.floor() as i32,
            y: p.y.floor() as i32,
        })
    }
}
