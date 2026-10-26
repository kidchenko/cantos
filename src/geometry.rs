//! Monitor enumeration and the corner hitboxes derived from it.
//!
//! The maths is shared; only [`monitors`] asks the OS anything. Every platform
//! reports displays in the same space the cursor is read in, so no scaling is
//! needed anywhere:
//!
//! * **Windows**: physical pixels. The process is manifested PerMonitorV2, so
//!   `GetCursorPos` and the monitor rects agree.
//! * **macOS**: points in the global display space, origin at the top-left of
//!   the main display, y growing downwards: what both `CGDisplayBounds` and
//!   `CGEventGetLocation` use.
//! * **Linux**: X11 root-window pixels, from RandR.

use crate::config::{Corner, MonitorMode};
use serde::Serialize;

pub use imp::monitors;

/// A rectangle with exclusive right/bottom edges, the Win32 `Rect`
/// convention, kept on every platform so the hitbox maths reads the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct MonitorInfo {
    /// Full bounds, including any taskbar or menu bar.
    pub rect: Rect,
    pub is_primary: bool,
}

/// A live corner: which corner it represents and the square the cursor has
/// to be inside for it to count.
#[derive(Clone, Copy, Debug)]
pub struct Hitbox {
    pub corner: Corner,
    pub rect: Rect,
}

/// One physical hot spot, identified by where it actually is.
///
/// `Corner` alone is not an identity: in `EveryMonitor` mode every display
/// contributes its own `TopLeft`, and a state machine that keys on the enum
/// treats all of them as the same place, so one screen's top-left stays
/// disarmed after a different screen's top-left fired. The anchor pixel
/// separates them, and is stable across the watcher's periodic rebuilds
/// because it is derived from the monitor rect rather than from list order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spot {
    pub corner: Corner,
    pub anchor: (i32, i32),
}

impl Hitbox {
    pub fn spot(&self) -> Spot {
        let p = anchor(self.corner, &self.rect);
        Spot {
            corner: self.corner,
            anchor: (p.x, p.y),
        }
    }
}

pub fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

fn union(monitors: &[MonitorInfo]) -> Option<Rect> {
    monitors.iter().map(|m| m.rect).reduce(|a, b| Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    })
}

/// The four `size`-square boxes anchored at the corners of `r`.
fn boxes_for(r: Rect, size: i32) -> [(Corner, Rect); 4] {
    // Clamp so a silly `size` on a small display cannot produce inverted or
    // mutually overlapping rects.
    let w = size.min((r.right - r.left).max(1));
    let h = size.min((r.bottom - r.top).max(1));
    [
        (
            Corner::TopLeft,
            Rect {
                left: r.left,
                top: r.top,
                right: r.left + w,
                bottom: r.top + h,
            },
        ),
        (
            Corner::TopRight,
            Rect {
                left: r.right - w,
                top: r.top,
                right: r.right,
                bottom: r.top + h,
            },
        ),
        (
            Corner::BottomLeft,
            Rect {
                left: r.left,
                top: r.bottom - h,
                right: r.left + w,
                bottom: r.bottom,
            },
        ),
        (
            Corner::BottomRight,
            Rect {
                left: r.right - w,
                top: r.bottom - h,
                right: r.right,
                bottom: r.bottom,
            },
        ),
    ]
}

/// Is this point actually on a physical display?
///
/// Matters for `OuterCorners` on non-rectangular arrangements: with two
/// screens offset vertically, a corner of the bounding box can sit in dead
/// space the cursor can never reach. A hitbox there would be a corner that
/// silently never fires, so [`stand_in`] finds a real one instead.
fn on_a_display(monitors: &[MonitorInfo], p: Point) -> bool {
    monitors.iter().any(|m| contains(&m.rect, p))
}

/// The real corner that takes the place of an outer `corner` sitting in dead
/// space (an L-shaped arrangement): of the displays' own corners of that
/// kind, the one nearest `wanted`, the bounding box's.
///
/// Only a corner the cursor actually stops in counts: one with no display
/// beyond it on either side. With a laptop below a wider monitor, the
/// laptop's bottom corners stand in for the bottom of the arrangement.
fn stand_in(monitors: &[MonitorInfo], corner: Corner, wanted: Point, size: i32) -> Option<Rect> {
    let (dx, dy) = match corner {
        Corner::TopLeft => (-1, -1),
        Corner::TopRight => (1, -1),
        Corner::BottomLeft => (-1, 1),
        Corner::BottomRight => (1, 1),
    };
    monitors
        .iter()
        .filter_map(|m| {
            boxes_for(m.rect, size)
                .into_iter()
                .find(|(c, _)| *c == corner)
        })
        .map(|(_, rect)| (rect, anchor(corner, &rect)))
        .filter(|(_, p)| {
            !on_a_display(
                monitors,
                Point {
                    x: p.x + dx,
                    y: p.y,
                },
            ) && !on_a_display(
                monitors,
                Point {
                    x: p.x,
                    y: p.y + dy,
                },
            )
        })
        .min_by_key(|(_, p)| {
            let (x, y) = ((p.x - wanted.x) as i64, (p.y - wanted.y) as i64);
            x * x + y * y
        })
        .map(|(rect, _)| rect)
}

/// Anchor pixel of a hitbox: the outermost pixel it owns.
fn anchor(corner: Corner, r: &Rect) -> Point {
    match corner {
        Corner::TopLeft => Point {
            x: r.left,
            y: r.top,
        },
        Corner::TopRight => Point {
            x: r.right - 1,
            y: r.top,
        },
        Corner::BottomLeft => Point {
            x: r.left,
            y: r.bottom - 1,
        },
        Corner::BottomRight => Point {
            x: r.right - 1,
            y: r.bottom - 1,
        },
    }
}

/// Build the live hitboxes for the current display layout.
pub fn hitboxes(monitors: &[MonitorInfo], mode: MonitorMode, size: i32) -> Vec<Hitbox> {
    if monitors.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(8);

    match mode {
        MonitorMode::OuterCorners => {
            if let Some(u) = union(monitors) {
                for (corner, rect) in boxes_for(u, size) {
                    let at = anchor(corner, &rect);
                    let rect = if on_a_display(monitors, at) {
                        Some(rect)
                    } else {
                        stand_in(monitors, corner, at, size)
                    };
                    if let Some(rect) = rect {
                        out.push(Hitbox { corner, rect });
                    }
                }
            }
        }
        MonitorMode::EveryMonitor => {
            for m in monitors {
                for (corner, rect) in boxes_for(m.rect, size) {
                    out.push(Hitbox { corner, rect });
                }
            }
        }
        MonitorMode::PrimaryOnly => {
            // Fall back to the first monitor if the OS reports no primary,
            // which can briefly happen while displays are being reconfigured.
            let primary = monitors
                .iter()
                .find(|m| m.is_primary)
                .or_else(|| monitors.first());
            if let Some(m) = primary {
                for (corner, rect) in boxes_for(m.rect, size) {
                    out.push(Hitbox { corner, rect });
                }
            }
        }
    }
    out
}

/// Serialisable views for the settings window.
///
/// The window draws the real display arrangement rather than a generic
/// rectangle, and highlights exactly which corners are live. Both come from
/// here so Rust stays the single source of truth: an outer corner moved off
/// dead space onto a real display is drawn where it actually is.
#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DisplayView {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HitboxView {
    pub corner: Corner,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub fn display_views(monitors: &[MonitorInfo]) -> Vec<DisplayView> {
    monitors
        .iter()
        .map(|m| DisplayView {
            left: m.rect.left,
            top: m.rect.top,
            right: m.rect.right,
            bottom: m.rect.bottom,
            primary: m.is_primary,
        })
        .collect()
}

pub fn hitbox_views(boxes: &[Hitbox]) -> Vec<HitboxView> {
    boxes
        .iter()
        .map(|h| HitboxView {
            corner: h.corner,
            left: h.rect.left,
            top: h.rect.top,
            right: h.rect.right,
            bottom: h.rect.bottom,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::{MonitorInfo, Rect};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    // windows-rs namespaces this under WindowsAndMessaging rather than Gdi,
    // which is where the C headers put it.
    use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<MonitorInfo>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            out.push(MonitorInfo {
                rect: super::Rect::from(mi.rcMonitor),
                is_primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL(1) // keep enumerating
    }

    pub fn monitors() -> Vec<MonitorInfo> {
        let mut out: Vec<MonitorInfo> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(enum_proc),
                LPARAM(&mut out as *mut _ as isize),
            );
        }
        out
    }

    impl From<RECT> for Rect {
        fn from(r: RECT) -> Self {
            Rect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod imp {
    use super::MonitorInfo;
    use crate::cg;

    /// More than anyone plugs in; the list is truncated rather than failing.
    const MAX_DISPLAYS: usize = 16;

    pub fn monitors() -> Vec<MonitorInfo> {
        let mut ids = [0u32; MAX_DISPLAYS];
        let mut count = 0u32;
        let rc = unsafe {
            cg::CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count)
        };
        if rc != 0 {
            return Vec::new();
        }
        let main = unsafe { cg::CGMainDisplayID() };
        ids[..count as usize]
            .iter()
            // A software mirror is listed as a display of its own with the
            // same bounds as the one it copies. Counting it twice would put
            // two hitboxes on one corner in `EveryMonitor` mode.
            .filter(|&&id| unsafe { cg::CGDisplayMirrorsDisplay(id) } == 0)
            .map(|&id| MonitorInfo {
                rect: cg::rect(unsafe { cg::CGDisplayBounds(id) }),
                // The main display is the one with the menu bar, which is what
                // "primary" means to a Mac user.
                is_primary: id == main,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use super::{MonitorInfo, Rect};
    use crate::x11;

    /// RandR 1.5 monitors, which is what desktop environments draw panels on;
    /// the whole root window as one display when RandR is unavailable.
    pub fn monitors() -> Vec<MonitorInfo> {
        x11::with(|x| x.monitors().unwrap_or_else(|| vec![whole_screen(x)])).unwrap_or_default()
    }

    fn whole_screen(x: &x11::X) -> MonitorInfo {
        let (w, h) = x.root_size();
        MonitorInfo {
            rect: Rect {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            },
            is_primary: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(left: i32, top: i32, right: i32, bottom: i32, is_primary: bool) -> MonitorInfo {
        MonitorInfo {
            rect: Rect {
                left,
                top,
                right,
                bottom,
            },
            is_primary,
        }
    }

    fn anchors(boxes: &[Hitbox]) -> Vec<(Corner, (i32, i32))> {
        boxes.iter().map(|h| (h.corner, h.spot().anchor)).collect()
    }

    #[test]
    fn side_by_side_screens_of_one_height_keep_the_outer_corners() {
        let m = [
            screen(0, 0, 1920, 1080, true),
            screen(1920, 0, 3840, 1080, false),
        ];
        let boxes = hitboxes(&m, MonitorMode::OuterCorners, 8);
        assert_eq!(
            anchors(&boxes),
            [
                (Corner::TopLeft, (0, 0)),
                (Corner::TopRight, (3839, 0)),
                (Corner::BottomLeft, (0, 1079)),
                (Corner::BottomRight, (3839, 1079)),
            ]
        );
    }

    #[test]
    fn a_corner_in_dead_space_moves_to_the_nearest_real_one() {
        // A laptop under a wider monitor that hangs off to the left: the
        // bounding box's bottom corners are on neither screen.
        let m = [
            screen(0, 0, 1512, 982, true),
            screen(-223, -1080, 1697, 0, false),
        ];
        let boxes = hitboxes(&m, MonitorMode::OuterCorners, 8);
        assert_eq!(
            anchors(&boxes),
            [
                (Corner::TopLeft, (-223, -1080)),
                (Corner::TopRight, (1696, -1080)),
                (Corner::BottomLeft, (0, 981)),
                (Corner::BottomRight, (1511, 981)),
            ]
        );
    }

    #[test]
    fn a_shorter_screen_beside_a_taller_one_lends_its_own_corner() {
        let m = [
            screen(0, 0, 1920, 1080, true),
            screen(1920, 0, 4480, 1440, false),
        ];
        let boxes = hitboxes(&m, MonitorMode::OuterCorners, 8);
        let bottom_left = boxes.iter().find(|h| h.corner == Corner::BottomLeft);
        assert_eq!(bottom_left.map(|h| h.spot().anchor), Some((0, 1079)));
    }

    #[test]
    fn a_corner_with_a_screen_beyond_it_never_stands_in() {
        // The left screen's top-right is nearest, but the cursor carries on
        // into the right screen there, so it is no corner at all.
        let m = [
            screen(0, 0, 1000, 800, true),
            screen(1000, 0, 2000, 800, false),
        ];
        let rect = stand_in(&m, Corner::TopRight, Point { x: 1000, y: -50 }, 8);
        assert_eq!(
            rect.map(|r| anchor(Corner::TopRight, &r)),
            Some(Point { x: 1999, y: 0 })
        );
    }

    #[test]
    fn every_monitor_has_four_corners_each() {
        let m = [
            screen(0, 0, 1512, 982, true),
            screen(-223, -1080, 1697, 0, false),
        ];
        assert_eq!(hitboxes(&m, MonitorMode::EveryMonitor, 8).len(), 8);
        assert_eq!(hitboxes(&m, MonitorMode::PrimaryOnly, 8).len(), 4);
    }
}
