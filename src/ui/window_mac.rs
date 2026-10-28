//! The native macOS settings window, in AppKit.
//!
//! Run as its own process (`cantos --settings-window`, see [`super::child`]):
//! it reads the tray's calls from stdin and writes its messages to stdout,
//! in the protocol of [`super::window`].
//!
//! It is the original web page's design, redrawn natively: the same colour tokens, type
//! scale, spacing and components (the stage with its display diagram, pill
//! chips, corner cards, the panel, the switch, the footer), measured off
//! the original web page. What native adds: the selects open real macOS menus with SF
//! Symbols, the card glyphs and the diagram's corners show each action's
//! symbol, Tab and Space work everywhere with the web page's focus ring, and
//! every control has a VoiceOver role.
//!
//! The components are a handful of drawn `NSView`s rather than stock AppKit
//! controls, because stock controls cannot take this palette. They pick light
//! or dark from their own effective appearance at every draw, so switching
//! the system appearance repaints them with no code here.
//!
//! On a Mac a corner is either the Dock's, with one of the actions System
//! Settings offers, or Cantos's own, with one the Dock cannot do (see
//! [`crate::native`]). Each corner's menu has the two as sections, and a card
//! with a Cantos action says so, since it only works while Cantos runs. Everything runs on the main thread, as AppKit requires, so the state
//! lives in `thread_local`s.

use super::child;
use crate::actions::{CATALOG, UNTESTABLE};
use crate::config::{Action, Modifier, MonitorMode};
use crate::native::NativeCorner;
use traykit::{lerror, linfo};

use std::cell::{Cell, RefCell};
use std::io::BufRead;

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{
    define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityButtonRole, NSAccessibilityCheckBoxRole,
    NSAccessibilityPopUpButtonRole, NSAppearanceCustomization, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSBitmapImageFileType,
    NSColor, NSControlTextEditingDelegate, NSEvent, NSFocusRingType, NSFont, NSFontAttributeName,
    NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSImage, NSImageSymbolConfiguration, NSKernAttributeName, NSLineBreakMode, NSMenu, NSMenuItem,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextField, NSTextFieldDelegate,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSDictionary, NSMutableDictionary, NSNotification, NSNumber, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Tokens, from the original web page's :root
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

struct Palette {
    bg: Rgb,
    raised: Rgb,
    sunken: Rgb,
    line: Rgb,
    line_soft: Rgb,
    text: Rgb,
    dim: Rgb,
    faint: Rgb,
    accent: Rgb,
    accent_dim: Rgb,
    danger: Rgb,
}

const DARK: Palette = Palette {
    bg: Rgb(0x0e, 0x10, 0x16),
    raised: Rgb(0x16, 0x1a, 0x24),
    sunken: Rgb(0x0a, 0x0c, 0x11),
    line: Rgb(0x24, 0x29, 0x38),
    line_soft: Rgb(0x1b, 0x1f, 0x2b),
    text: Rgb(0xe9, 0xec, 0xf4),
    dim: Rgb(0x97, 0xa0, 0xb5),
    faint: Rgb(0x5d, 0x65, 0x7a),
    accent: Rgb(0x6e, 0xa8, 0xfe),
    accent_dim: Rgb(0x2f, 0x4a, 0x78),
    danger: Rgb(0xef, 0x76, 0x76),
};

const LIGHT: Palette = Palette {
    bg: Rgb(0xf2, 0xf4, 0xf9),
    raised: Rgb(0xff, 0xff, 0xff),
    sunken: Rgb(0xea, 0xee, 0xf6),
    line: Rgb(0xd3, 0xda, 0xe8),
    line_soft: Rgb(0xe5, 0xea, 0xf2),
    text: Rgb(0x15, 0x19, 0x25),
    dim: Rgb(0x59, 0x62, 0x7a),
    faint: Rgb(0x93, 0x9c, 0xb1),
    accent: Rgb(0x25, 0x63, 0xc9),
    accent_dim: Rgb(0xa8, 0xc1, 0xea),
    danger: Rgb(0xc2, 0x40, 0x2f),
};

/// The palette for a view, from its own effective appearance, so a window
/// forced light or dark (and the preview) draws accordingly.
fn palette(view: &NSView) -> &'static Palette {
    if view
        .effectiveAppearance()
        .name()
        .to_string()
        .contains("Dark")
    {
        &DARK
    } else {
        &LIGHT
    }
}

fn color(c: Rgb) -> Retained<NSColor> {
    color_a(c, 1.0)
}

fn color_a(c: Rgb, alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.0 as f64 / 255.0,
        c.1 as f64 / 255.0,
        c.2 as f64 / 255.0,
        alpha,
    )
}

/// CSS `color-mix(in srgb, a p%, b)`.
fn mix(a: Rgb, p: f64, b: Rgb) -> Rgb {
    let m = |x: u8, y: u8| (x as f64 * p + y as f64 * (1.0 - p)).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

// ---------------------------------------------------------------------------
// Layout, in points, top-down; numbers follow the original web page's CSS
// ---------------------------------------------------------------------------

const WIDTH: f64 = 640.0;
const PAD: f64 = 24.0;
const INNER: f64 = WIDTH - 2.0 * PAD;
const TITLEBAR: f64 = 28.0;
const HEADER_Y: f64 = TITLEBAR + 12.0;
const HEADER_H: f64 = 36.0;
const STAGE_PAD: f64 = 14.0;
const STATUS_H: f64 = 39.0;
const CHIP_H: f64 = 26.0;
const CARD_GAP: f64 = 9.0;
const CARD_W: f64 = (INNER - CARD_GAP) / 2.0;
const CARD_H: f64 = 80.0;
const SELECT_H: f64 = 30.0;
const KEY_W: f64 = 104.0;
/// A row of the General panel: a title, a line under it, a control.
const PANEL_ROW_H: f64 = 60.0;
const PANEL_H: f64 = 26.0 + 2.0 * PANEL_ROW_H;
const MODE_W: f64 = 150.0;
/// A row of the custom commands panel: heading, then the two fields.
const COMMAND_ROW_H: f64 = 77.0;
const SWITCH_W: f64 = 38.0;
const SWITCH_H: f64 = 22.0;

const CORNER_NAMES: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
const CORNER_KEYS: [&str; 4] = ["topLeft", "topRight", "bottomLeft", "bottomRight"];

/// The keys System Settings offers, in its order and with its symbols.
const KEYS: [(Modifier, &str); 5] = [
    (Modifier::None, "No key"),
    (Modifier::Shift, "\u{21E7} Shift"),
    (Modifier::Ctrl, "\u{2303} Control"),
    (Modifier::Alt, "\u{2325} Option"),
    (Modifier::Win, "\u{2318} Command"),
];

/// What a closed key select shows: the symbol and a short name, so it fits.
/// The menu keeps the full names.
fn key_short(m: Modifier) -> &'static str {
    match m {
        Modifier::None => "No key",
        Modifier::Shift => "\u{21E7} Shift",
        Modifier::Ctrl => "\u{2303} Ctrl",
        Modifier::Alt => "\u{2325} Opt",
        Modifier::Win => "\u{2318} Cmd",
    }
}

/// The monitor modes, with the names the Windows window gives them.
const MODES: [(MonitorMode, &str); 3] = [
    (MonitorMode::OuterCorners, "Outer corners"),
    (MonitorMode::EveryMonitor, "Every screen"),
    (MonitorMode::PrimaryOnly, "Primary only"),
];

/// Menu tags for the mode menu, above any corner's.
const MODE_TAG: usize = 9000;

fn monitor_mode() -> MonitorMode {
    STATE.with(|s| {
        s.borrow()
            .config
            .as_ref()
            .map_or(MonitorMode::OuterCorners, |c| c.monitor_mode)
    })
}

/// Same presets, and the same names, as the other windows.
const PRESET_LIST: [(&str, [&str; 4]); 3] = [
    ("Recommended", ["taskView", "showDesktop", "none", "none"]),
    ("Minimal", ["taskView", "none", "none", "none"]),
    ("Clear all", ["none", "none", "none", "none"]),
];

/// An SF Symbol per action, with a fallback for macOS releases that predate
/// the first choice.
fn symbol_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "taskView" => &["rectangle.3.group", "rectangle.split.3x1"],
        "appWindows" => &["macwindow.on.rectangle"],
        "showDesktop" => &["menubar.dock.rectangle"],
        "notifications" => &["bell"],
        "launchpad" => &["square.grid.3x3"],
        "quickNote" => &["note.text"],
        "screensaver" => &["sparkles.tv", "tv"],
        "disableScreenSaver" => &["tv.slash", "nosign"],
        "displayOff" => &["display", "moon"],
        "lockWorkstation" => &["lock"],
        "search" => &["magnifyingglass"],
        "fileExplorer" => &["folder"],
        "settings" => &["gearshape"],
        "screenshot" => &["camera.viewfinder"],
        "emoji" => &["face.smiling"],
        "desktopLeft" => &["arrow.left.square"],
        "desktopRight" => &["arrow.right.square"],
        "custom" => &["terminal"],
        _ => &["minus"],
    }
}

/// The catalog flattened to `(kind, label)`, in menu order.
fn actions() -> Vec<(&'static str, &'static str)> {
    CATALOG
        .iter()
        .flat_map(|(_, items)| items.iter().copied())
        .collect()
}

fn kind_of(action: &Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_else(|| "none".into())
}

fn action_of(kind: &str) -> Action {
    if kind == "custom" {
        return Action::Custom {
            command: String::new(),
            args: String::new(),
        };
    }
    serde_json::from_value(serde_json::json!({ "kind": kind })).unwrap_or(Action::None)
}

/// Whether `kind` is one of Cantos's own, which need Cantos running.
fn is_cantos(kind: &str) -> bool {
    CATALOG
        .iter()
        .any(|(group, items)| *group == Some("Cantos") && items.iter().any(|(k, _)| *k == kind))
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The part of the tray's `state` push this window uses.
#[derive(Deserialize, Default, Clone)]
struct State {
    #[serde(default)]
    config: Option<ConfigView>,
    #[serde(default)]
    native: Option<Vec<NativeCorner>>,
    #[serde(default)]
    displays: Vec<DisplayView>,
    #[serde(default)]
    hitboxes: Vec<HitboxView>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    autostart: bool,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
struct DisplayView {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    primary: bool,
}

/// The part of the config this window shows besides the corners.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConfigView {
    monitor_mode: MonitorMode,
}

#[derive(Deserialize, Default, Clone)]
struct HitboxView {
    corner: String,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// What each control is, which decides how it draws and what pressing it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Preset(usize),
    Test(usize),
    ActionSelect(usize),
    KeySelect(usize),
    ModeSelect,
    Autostart,
    Log,
    Folder,
}

struct Ui {
    window: Retained<NSWindow>,
    handler: Retained<Handler>,
    root: Retained<Surface>,
    stage: Retained<Stage>,
    diagram: Retained<Diagram>,
    chips: Vec<Retained<Control>>,
    cards: Vec<Retained<Card>>,
    tests: Vec<Retained<Control>>,
    selects: Vec<Retained<Control>>,
    keys: Vec<Retained<Control>>,
    commands: Retained<Commands>,
    /// Command and arguments, per corner; shown only for custom corners.
    fields: Vec<(Retained<NSTextField>, Retained<NSTextField>)>,
    command_tests: Vec<Retained<Control>>,
    panel: Retained<Panel>,
    switch: Retained<Control>,
    mode: Retained<Control>,
    links: Vec<Retained<Control>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// The corner whose card the pointer is over, lit in the diagram.
    static HOVERED: Cell<Option<usize>> = const { Cell::new(None) };
    /// The footer message and whether it is an error.
    static TOAST: RefCell<(String, bool)> = const { RefCell::new((String::new(), false)) };
    /// Bumped per toast, so an older toast's timer does not clear a newer one.
    static TOAST_GEN: Cell<u64> = const { Cell::new(0) };
    /// Where `layout` put the rows the root view draws itself.
    static ROWS: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };
    /// Bumped per keystroke in a command field, so only the last one in a
    /// burst is sent.
    static TYPING_GEN: Cell<u64> = const { Cell::new(0) };
}

fn corners() -> [NativeCorner; 4] {
    STATE.with(|s| {
        let n = s.borrow().native.clone().unwrap_or_default();
        std::array::from_fn(|i| {
            n.get(i).cloned().unwrap_or(NativeCorner {
                action: Action::None,
                modifier: Modifier::None,
            })
        })
    })
}

fn armed(i: usize) -> bool {
    corners()[i].action != Action::None
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

struct Font {
    size: f64,
    weight: f64,
    /// Letter spacing in em, as CSS has it.
    kern: f64,
    upper: bool,
}

const fn font(size: f64, weight: f64) -> Font {
    Font {
        size,
        weight,
        kern: 0.0,
        upper: false,
    }
}

/// The web page's small-caps label: 10.5px, 600, tracked, upper case.
const CAPS: Font = Font {
    size: 10.5,
    weight: 600.0,
    kern: 0.07,
    upper: true,
};

fn ns_font(f: &Font) -> Retained<NSFont> {
    let weight = unsafe {
        match f.weight as u32 {
            600.. => NSFontWeightSemibold,
            500..=599 => NSFontWeightMedium,
            _ => NSFontWeightRegular,
        }
    };
    NSFont::systemFontOfSize_weight(f.size, weight)
}

fn attrs(f: &Font, c: Rgb, alpha: f64, truncate: bool) -> Retained<NSMutableDictionary> {
    let dict: Retained<NSMutableDictionary> = NSMutableDictionary::new();
    let font = ns_font(f);
    let col = color_a(c, alpha);
    let kern = NSNumber::new_f64(f.kern * f.size);
    unsafe {
        let _: () = msg_send![&dict, setObject: &*font, forKey: NSFontAttributeName];
        let _: () = msg_send![&dict, setObject: &*col, forKey: NSForegroundColorAttributeName];
        let _: () = msg_send![&dict, setObject: &*kern, forKey: NSKernAttributeName];
        if truncate {
            let para = NSMutableParagraphStyle::new();
            para.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            let _: () = msg_send![&dict, setObject: &*para, forKey: NSParagraphStyleAttributeName];
        }
    }
    dict
}

fn shown(text: &str, f: &Font) -> Retained<NSString> {
    NSString::from_str(&if f.upper {
        text.to_uppercase()
    } else {
        text.to_string()
    })
}

fn text_width(text: &str, f: &Font) -> f64 {
    let a = attrs(f, Rgb(0, 0, 0), 1.0, false);
    let size: NSSize = unsafe { msg_send![&shown(text, f), sizeWithAttributes: &*a] };
    size.width.ceil()
}

/// One line of text, vertically centred on `mid`.
fn draw_text(text: &str, f: &Font, c: Rgb, alpha: f64, x: f64, mid: f64, width: f64) {
    let a = attrs(f, c, alpha, true);
    let line = (f.size * 1.3).ceil();
    let r = rect(x, mid - line / 2.0, width.max(0.0), line);
    unsafe {
        let _: () = msg_send![&shown(text, f), drawInRect: r, withAttributes: &*a];
    }
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// A rounded box with a one-point border drawn inside its edge, as CSS's
/// `border` with `box-sizing: border-box` does.
fn rounded(r: NSRect, radius: f64, fill: Option<Rgb>, stroke: Option<Rgb>) {
    let inner = rect(
        r.origin.x + 0.5,
        r.origin.y + 0.5,
        r.size.width - 1.0,
        r.size.height - 1.0,
    );
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inner, radius, radius);
    if let Some(f) = fill {
        color(f).setFill();
        path.fill();
    }
    if let Some(s) = stroke {
        color(s).setStroke();
        path.setLineWidth(1.0);
        path.stroke();
    }
}

/// The web page's focus ring: 2px of accent, offset 2px outside the control.
fn focus_ring(r: NSRect, radius: f64, p: &Palette) {
    let ring = rect(
        r.origin.x - 3.0,
        r.origin.y - 3.0,
        r.size.width + 6.0,
        r.size.height + 6.0,
    );
    let path =
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ring, radius + 3.0, radius + 3.0);
    color(p.accent).setStroke();
    path.setLineWidth(2.0);
    path.stroke();
}

/// The SF Symbol for an action, tinted. `None` on a macOS too old for every
/// candidate, where the slot is simply left empty.
fn symbol(kind: &str, size: f64, tint: Rgb) -> Option<Retained<NSImage>> {
    let image = symbol_for(kind).iter().find_map(|name| {
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
    })?;
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, unsafe {
        NSFontWeightRegular
    })
    .configurationByApplyingConfiguration(
        &NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color(tint)),
    );
    image.imageWithSymbolConfiguration(&config)
}

fn draw_symbol(kind: &str, size: f64, tint: Rgb, centre: NSPoint) {
    if let Some(img) = symbol(kind, size, tint) {
        let s = img.size();
        img.drawInRect(rect(
            centre.x - s.width / 2.0,
            centre.y - s.height / 2.0,
            s.width,
            s.height,
        ));
    }
}

/// The web page's corner glyph: an L bracket in a 12-unit box, `CORNER_GLYPH`.
fn corner_glyph(i: usize, x: f64, y: f64, size: f64, c: Rgb) {
    let s = size / 12.0;
    let p = |px: f64, py: f64| NSPoint::new(x + px * s, y + py * s);
    let pts = match i {
        0 => [p(1.0, 5.0), p(1.0, 1.0), p(5.0, 1.0)],
        1 => [p(7.0, 1.0), p(11.0, 1.0), p(11.0, 5.0)],
        2 => [p(1.0, 7.0), p(1.0, 11.0), p(5.0, 11.0)],
        _ => [p(11.0, 7.0), p(11.0, 11.0), p(7.0, 11.0)],
    };
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(pts[0]);
    path.lineToPoint(pts[1]);
    path.lineToPoint(pts[2]);
    path.setLineWidth(2.0 * s);
    color(c).setStroke();
    path.stroke();
}

/// The web page's inline mark: a dark tile with the lit quarter-disc, 64 units.
fn draw_mark(x: f64, y: f64, size: f64, p: &Palette) {
    let s = size / 64.0;
    let tile = rect(x, y, size, size);
    let clip = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(tile, 15.0 * s, 15.0 * s);
    objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
    clip.addClip();
    color(Rgb(0x12, 0x14, 0x1f)).setFill();
    NSBezierPath::bezierPathWithRect(tile).fill();
    // A quarter circle of radius 40 about the tile's top-left corner. The
    // view is flipped, so "clockwise" here sweeps from right to down.
    let disc = NSBezierPath::bezierPath();
    disc.moveToPoint(NSPoint::new(x, y));
    disc.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        NSPoint::new(x, y),
        40.0 * s,
        0.0,
        90.0,
        false,
    );
    disc.closePath();
    color(Rgb(0x6e, 0xa8, 0xfe)).setFill();
    disc.fill();
    objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    let edge = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        rect(x + 0.4, y + 0.4, size - 0.8, size - 0.8),
        14.25 * s,
        14.25 * s,
    );
    color_a(p.text, 0.18).setStroke();
    edge.setLineWidth(0.8);
    edge.stroke();
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// Repaint every view, as after an appearance change.
fn repaint_all() {
    UI.with(|ui| {
        let ui = ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        let bg = palette(&ui.root).bg;
        ui.window.setBackgroundColor(Some(&color(bg)));
        let mut views: Vec<&NSView> =
            vec![&ui.root, &ui.stage, &ui.diagram, &ui.panel, &ui.commands];
        views.extend(ui.cards.iter().map(|c| &**c as &NSView));
        views.extend(
            ui.chips
                .iter()
                .chain(&ui.tests)
                .chain(&ui.selects)
                .chain(&ui.keys)
                .chain(&ui.links)
                .chain(&ui.command_tests)
                .chain(std::iter::once(&ui.switch))
                .map(|c| &**c as &NSView),
        );
        for v in views {
            v.setNeedsDisplay(true);
        }
    });
}

define_class!(
    /// The window's content: the background, the header, the presets
    /// label and the footer, all of which are text on `--bg`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosSurface"]
    struct Surface;

    impl Surface {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            repaint_all();
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_surface(self);
        }
    }
);

fn draw_surface(view: &NSView) {
    let p = palette(view);
    color(p.bg).setFill();
    NSBezierPath::bezierPathWithRect(view.bounds()).fill();

    // Header: the mark, the name, the one line under it.
    draw_mark(PAD, HEADER_Y + 4.0, 28.0, p);
    let title = Font {
        kern: -0.01,
        ..font(15.0, 600.0)
    };
    draw_text(
        "Cantos",
        &title,
        p.text,
        1.0,
        PAD + 39.0,
        HEADER_Y + 10.0,
        300.0,
    );
    draw_text(
        "Your Mac's hot corners, plus a few only Cantos can do.",
        &font(11.5, 400.0),
        p.dim,
        1.0,
        PAD + 39.0,
        HEADER_Y + 28.0,
        INNER - 39.0,
    );

    let (presets_y, footer_y) = ROWS.with(|r| r.get());
    draw_text(
        "Presets",
        &CAPS,
        p.faint,
        1.0,
        PAD,
        presets_y + CHIP_H / 2.0,
        80.0,
    );

    // Footer: a hairline, the version, and any message.
    color(p.line_soft).setFill();
    NSBezierPath::bezierPathWithRect(rect(PAD, footer_y, INNER, 1.0)).fill();
    let mid = footer_y + 13.0 + 9.0;
    let version = STATE.with(|s| s.borrow().version.clone());
    draw_text(
        &format!("Cantos {version}"),
        &font(11.0, 400.0),
        p.faint,
        1.0,
        PAD,
        mid,
        120.0,
    );
    let (toast, bad) = TOAST.with(|t| t.borrow().clone());
    if !toast.is_empty() {
        draw_text(
            &toast,
            &font(11.0, 400.0),
            if bad { p.danger } else { p.dim },
            1.0,
            PAD + 110.0,
            mid,
            230.0,
        );
    }
}

define_class!(
    /// `.stage`: the raised box holding the diagram and the status line.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosStage"]
    struct Stage;

    impl Stage {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_stage(self);
        }
    }
);

fn draw_stage(view: &NSView) {
    let p = palette(view);
    let b = view.bounds();
    rounded(b, 12.0, Some(p.raised), Some(p.line_soft));

    // `.statusbar`: a dot, accent when any corner is set, and the line.
    let c = corners();
    let set = c.iter().filter(|c| c.action != Action::None).count();
    let own = c.iter().filter(|c| is_cantos(&kind_of(&c.action))).count();
    let mid = b.size.height - 9.0 - 15.0;
    let dot = NSBezierPath::bezierPathWithOvalInRect(rect(STAGE_PAD + 2.0, mid - 3.0, 6.0, 6.0));
    color(if set > 0 { p.accent } else { p.faint }).setFill();
    dot.fill();
    let assigned = match set {
        0 => "No corners assigned yet. Try a preset".to_string(),
        1 => "1 corner assigned".to_string(),
        n => format!("{n} corners assigned"),
    };
    let line = match own {
        0 if set > 0 => format!("{assigned}. All are macOS's own and work without Cantos"),
        0 => assigned,
        1 => format!("{assigned}. 1 is Cantos's own and needs Cantos running"),
        n => format!("{assigned}. {n} are Cantos's own and need Cantos running"),
    };
    draw_text(
        &line,
        &font(11.5, 400.0),
        p.dim,
        1.0,
        STAGE_PAD + 16.0,
        mid,
        b.size.width - 2.0 * STAGE_PAD - 16.0,
    );
}

define_class!(
    /// `#diagram`: the displays as arranged, a bracket at each live corner.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosDiagram"]
    struct Diagram;

    impl Diagram {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_diagram(self.bounds(), palette(self));
        }
    }
);

/// The diagram's height for an arrangement, by the web page's rule: as tall as
/// its shape needs at this width, between 140 and 268.
fn diagram_height(displays: &[DisplayView]) -> f64 {
    let (w, h) = extent(displays);
    let box_w = INNER - 2.0 * STAGE_PAD;
    if w <= 0.0 {
        return 190.0;
    }
    (box_w * h / w).round().clamp(140.0, 268.0)
}

fn extent(displays: &[DisplayView]) -> (f64, f64) {
    let l = displays.iter().map(|d| d.left).min().unwrap_or(0);
    let t = displays.iter().map(|d| d.top).min().unwrap_or(0);
    let r = displays.iter().map(|d| d.right).max().unwrap_or(0);
    let b = displays.iter().map(|d| d.bottom).max().unwrap_or(0);
    ((r - l) as f64, (b - t) as f64)
}

fn draw_diagram(bounds: NSRect, p: &Palette) {
    let state = STATE.with(|s| s.borrow().clone());
    let displays = &state.displays;
    if displays.is_empty() {
        return;
    }
    let l = displays.iter().map(|d| d.left).min().unwrap_or(0) as f64;
    let t = displays.iter().map(|d| d.top).min().unwrap_or(0) as f64;
    let (w, h) = extent(displays);

    // The web page's padding: 24px around the displays, so brackets never clip.
    let pad = 24.0;
    let scale = ((bounds.size.width - 2.0 * pad) / w).min((bounds.size.height - 2.0 * pad) / h);
    let (dw, dh) = (w * scale, h * scale);
    let ox = (bounds.size.width - dw) / 2.0;
    let oy = (bounds.size.height - dh) / 2.0;
    let map = |x: f64, y: f64| NSPoint::new(ox + (x - l) * scale, oy + (y - t) * scale);

    for d in displays {
        let a = map(d.left as f64, d.top as f64);
        let b = map(d.right as f64, d.bottom as f64);
        let r = rect(a.x, a.y, b.x - a.x, b.y - a.y);
        let fill = if d.primary {
            mix(p.accent, 0.14, p.raised)
        } else {
            mix(p.text, 0.06, p.raised)
        };
        let body = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 8.0, 8.0);
        color(fill).setFill();
        body.fill();
        color(p.line).setStroke();
        body.setLineWidth(1.25);
        body.stroke();
        if d.primary {
            // The menu bar, full width along the top of the main display.
            let bar = NSBezierPath::bezierPathWithRect(rect(
                r.origin.x + 8.0,
                r.origin.y + 1.5,
                r.size.width - 16.0,
                4.0,
            ));
            color_a(p.faint, 0.5).setFill();
            bar.fill();
        }
        if displays.len() > 1 {
            let (label, f) = if d.primary {
                (
                    "Primary".to_string(),
                    Font {
                        kern: 0.06,
                        upper: true,
                        ..font(9.5, 600.0)
                    },
                )
            } else {
                (
                    format!("{} \u{00D7} {}", d.right - d.left, d.bottom - d.top),
                    font(9.5, 500.0),
                )
            };
            let tw = text_width(&label, &f);
            draw_text(
                &label,
                &f,
                p.faint,
                1.0,
                r.origin.x + (r.size.width - tw) / 2.0,
                r.origin.y + r.size.height / 2.0,
                tw + 2.0,
            );
        }
    }

    let hovered = HOVERED.with(|c| c.get());
    let corners = corners();
    for hb in &state.hitboxes {
        let Some(i) = CORNER_KEYS.iter().position(|k| *k == hb.corner) else {
            continue;
        };
        // Anchored on the hitbox's outermost edge: the bracket sits exactly
        // where the cursor has to reach, which on an L-shaped arrangement
        // is not always a corner of the whole.
        let x = if i % 2 == 0 { hb.left } else { hb.right };
        let y = if i < 2 { hb.top } else { hb.bottom };
        let at = map(x as f64, y as f64);
        let (dx, dy) = (
            if i % 2 == 0 { 1.0 } else { -1.0 },
            if i < 2 { 1.0 } else { -1.0 },
        );
        let kind = kind_of(&corners[i].action);
        let set = kind != "none";
        let lit = hovered == Some(i);

        // The crop-mark bracket: 15px arms, 2px, square caps.
        let arm = 15.0;
        let path = NSBezierPath::bezierPath();
        path.moveToPoint(NSPoint::new(at.x, at.y + dy * arm));
        path.lineToPoint(at);
        path.lineToPoint(NSPoint::new(at.x + dx * arm, at.y));
        path.setLineWidth(2.0);
        path.setLineCapStyle(objc2_app_kit::NSLineCapStyle::Square);
        color(if lit {
            p.accent
        } else if set {
            p.accent_dim
        } else {
            p.line
        })
        .setStroke();
        path.stroke();

        // What it does, just inside the bracket.
        if set {
            let tint = if lit {
                p.accent
            } else {
                mix(p.accent, 0.7, p.raised)
            };
            draw_symbol(
                &kind,
                12.0,
                tint,
                NSPoint::new(at.x + dx * 17.0, at.y + dy * 17.0),
            );
        }
    }
}

define_class!(
    /// `.card`: one corner. Draws its own heading and glyph; its Test button
    /// and two selects are subviews. Lights its corner in the diagram while
    /// the pointer is over it.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosCard"]
    #[ivars = Cell<usize>]
    struct Card;

    impl Card {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_card(self, self.ivars().get());
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            set_hovered(Some(self.ivars().get()));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            set_hovered(None);
        }
    }
);

impl Card {
    fn new(mtm: MainThreadMarker, corner: usize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(corner));
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

fn draw_card(view: &NSView, i: usize) {
    let p = palette(view);
    let on = armed(i);
    let hovered = HOVERED.with(|h| h.get()) == Some(i);
    let border = if on {
        p.accent_dim
    } else if hovered {
        p.line
    } else {
        p.line_soft
    };
    rounded(view.bounds(), 11.0, Some(p.raised), Some(border));

    corner_glyph(i, 12.0, 12.0, 11.0, if on { p.accent } else { p.faint });
    draw_text(
        CORNER_NAMES[i],
        &CAPS,
        if on { p.dim } else { p.faint },
        1.0,
        29.0,
        17.5,
        160.0,
    );
    // A Cantos action only works while Cantos runs; say so on the card.
    let kind = kind_of(&corners()[i].action);
    if is_cantos(&kind) {
        let tag = Font { size: 9.0, ..CAPS };
        let x = 29.0 + text_width(CORNER_NAMES[i], &CAPS) + 8.0;
        let w = text_width("Cantos", &tag) + 12.0;
        rounded(rect(x, 10.0, w, 15.0), 7.5, None, Some(p.accent_dim));
        draw_text("Cantos", &tag, p.accent, 1.0, x + 6.0, 17.5, w);
    }

    // `.glyph`: the action's symbol, accent when the corner is set.
    draw_symbol(
        &kind,
        13.0,
        if on { p.accent } else { p.faint },
        NSPoint::new(20.0, 36.0 + SELECT_H / 2.0),
    );
}

define_class!(
    /// `.panel` "General", with its one `.row`; the switch is a subview.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosPanel"]
    struct Panel;

    impl Panel {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_panel(self);
        }
    }
);

fn draw_panel(view: &NSView) {
    let p = palette(view);
    let b = view.bounds();
    rounded(b, 12.0, Some(p.raised), Some(p.line_soft));
    let h2 = Font { kern: 0.08, ..CAPS };
    draw_text("General", &h2, p.faint, 1.0, 14.0, 19.0, 200.0);
    draw_text(
        "Open at login",
        &font(12.5, 400.0),
        p.text,
        1.0,
        14.0,
        46.5,
        300.0,
    );
    draw_text(
        "Keeps Cantos in the menu bar after a restart",
        &font(11.0, 400.0),
        p.dim,
        1.0,
        14.0,
        66.0,
        b.size.width - 100.0,
    );
    color(p.line_soft).setFill();
    NSBezierPath::bezierPathWithRect(rect(14.0, 26.0 + PANEL_ROW_H, b.size.width - 28.0, 1.0))
        .fill();
    draw_text(
        "Multiple displays",
        &font(12.5, 400.0),
        p.text,
        1.0,
        14.0,
        46.5 + PANEL_ROW_H,
        300.0,
    );
    draw_text(
        "Where Cantos's own corners are; the Dock's stay its own",
        &font(11.0, 400.0),
        p.dim,
        1.0,
        14.0,
        66.0 + PANEL_ROW_H,
        b.size.width - MODE_W - 40.0,
    );
}

define_class!(
    /// `#custom`: one row per corner with a custom command, each a heading,
    /// a Test button and two real text fields drawn into sunken boxes.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosCommands"]
    struct Commands;

    impl Commands {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_commands(self);
        }
    }
);

fn custom_corners() -> Vec<usize> {
    corners()
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.action, Action::Custom { .. }))
        .map(|(i, _)| i)
        .collect()
}

fn draw_commands(view: &NSView) {
    let p = palette(view);
    let b = view.bounds();
    rounded(b, 12.0, Some(p.raised), Some(p.line_soft));
    let h2 = Font { kern: 0.08, ..CAPS };
    draw_text("Custom commands", &h2, p.faint, 1.0, 14.0, 19.0, 300.0);
    let editing = UI.with(|ui| {
        ui.borrow()
            .as_ref()
            .map(|ui| {
                ui.fields
                    .iter()
                    .map(|(c, a)| (c.currentEditor().is_some(), a.currentEditor().is_some()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    for (row, i) in custom_corners().into_iter().enumerate() {
        let y = 26.0 + row as f64 * COMMAND_ROW_H;
        if row > 0 {
            color(p.line_soft).setFill();
            NSBezierPath::bezierPathWithRect(rect(0.0, y, b.size.width, 1.0)).fill();
        }
        draw_text(
            CORNER_NAMES[i],
            &font(12.0, 600.0),
            p.text,
            1.0,
            14.0,
            y + 11.0 + 9.0,
            200.0,
        );
        let (command_box, args_box) = command_boxes(b.size.width, y);
        let (ec, ea) = editing.get(i).copied().unwrap_or((false, false));
        for (r, active) in [(command_box, ec), (args_box, ea)] {
            if active {
                let glow = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    rect(
                        r.origin.x - 2.5,
                        r.origin.y - 2.5,
                        r.size.width + 5.0,
                        r.size.height + 5.0,
                    ),
                    9.5,
                    9.5,
                );
                color_a(p.accent, 0.2).setStroke();
                glow.setLineWidth(3.0);
                glow.stroke();
            }
            rounded(
                r,
                7.0,
                Some(p.sunken),
                Some(if active { p.accent } else { p.line }),
            );
        }
    }
}

/// The two sunken boxes of a command row whose top is `y`: the command
/// takes the room, the arguments get 128 points, as `.cmdgrid` has it.
fn command_boxes(width: f64, y: f64) -> (NSRect, NSRect) {
    let top = y + 11.0 + 18.0 + 7.0;
    let args_w = 128.0;
    (
        rect(14.0, top, width - 28.0 - 8.0 - args_w, SELECT_H),
        rect(width - 14.0 - args_w, top, args_w, SELECT_H),
    )
}

/// The state of one drawn control.
struct CtlState {
    id: Ctl,
    title: String,
    enabled: bool,
    on: bool,
    hover: bool,
    down: bool,
}

define_class!(
    /// Every interactive element: chips, the Test buttons, the selects, the
    /// switch, the footer links. One class, drawn by `id`, so focus, hover,
    /// keyboard and VoiceOver behave identically across all of them.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosControl"]
    #[ivars = RefCell<CtlState>]
    struct Control;

    impl Control {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_control(self);
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().borrow().enabled
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            self.setNeedsDisplay(true);
            unsafe { msg_send![super(self), becomeFirstResponder] }
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.setNeedsDisplay(true);
            unsafe { msg_send![super(self), resignFirstResponder] }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key = event
                .charactersIgnoringModifiers()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if key == " " || key == "\r" {
                press(self);
            } else {
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            track(self);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().borrow_mut().hover = true;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            {
                let mut s = self.ivars().borrow_mut();
                s.hover = false;
                s.down = false;
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            let (enabled, select) = {
                let s = self.ivars().borrow();
                (s.enabled, matches!(s.id, Ctl::ActionSelect(_) | Ctl::KeySelect(_)))
            };
            if !enabled {
                return;
            }
            // A select opens its menu on press, as NSPopUpButton does.
            if select {
                press(self);
                return;
            }
            self.ivars().borrow_mut().down = true;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let was_down = std::mem::replace(&mut self.ivars().borrow_mut().down, false);
            self.setNeedsDisplay(true);
            let at = self.convertPoint_fromView(event.locationInWindow(), None);
            let b = self.bounds();
            let inside =
                at.x >= 0.0 && at.y >= 0.0 && at.x <= b.size.width && at.y <= b.size.height;
            if was_down && inside {
                press(self);
            }
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            press(self);
            true
        }
    }
);

impl Control {
    fn new(mtm: MainThreadMarker, id: Ctl, title: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(CtlState {
            id,
            title: title.into(),
            enabled: true,
            on: false,
            hover: false,
            down: false,
        }));
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        this.setAccessibilityElement(true);
        let role = unsafe {
            match id {
                Ctl::ActionSelect(_) | Ctl::KeySelect(_) | Ctl::ModeSelect => {
                    NSAccessibilityPopUpButtonRole
                }
                Ctl::Autostart => NSAccessibilityCheckBoxRole,
                _ => NSAccessibilityButtonRole,
            }
        };
        this.setAccessibilityRole(Some(role));
        let label = match id {
            Ctl::Test(i) => format!("Test {}", CORNER_NAMES[i].to_lowercase()),
            Ctl::ActionSelect(i) => format!("{} action", CORNER_NAMES[i]),
            Ctl::KeySelect(i) => format!("{} key to hold", CORNER_NAMES[i]),
            Ctl::ModeSelect => "Multiple displays".into(),
            _ => title.into(),
        };
        this.setAccessibilityLabel(Some(&NSString::from_str(&label)));
        this
    }

    fn set(&self, title: Option<&str>, enabled: bool, on: bool) {
        let id = {
            let mut s = self.ivars().borrow_mut();
            if let Some(t) = title {
                s.title = t.into();
            }
            s.enabled = enabled;
            s.on = on;
            s.id
        };
        self.setAccessibilityEnabled(enabled);
        if let (Some(t), Ctl::ActionSelect(_) | Ctl::KeySelect(_) | Ctl::ModeSelect) = (title, id) {
            unsafe { self.setAccessibilityValue(Some(&NSString::from_str(t))) };
        }
        if id == Ctl::Autostart {
            unsafe { self.setAccessibilityValue(Some(&NSNumber::new_bool(on))) };
        }
        self.setNeedsDisplay(true);
    }

    fn focused(&self) -> bool {
        self.window()
            .and_then(|w| w.firstResponder())
            .is_some_and(|r| {
                std::ptr::eq(
                    Retained::as_ptr(&r).cast::<u8>(),
                    (self as *const Self).cast(),
                )
            })
    }
}

fn track(view: &NSView) {
    for area in view.trackingAreas().iter() {
        view.removeTrackingArea(&area);
    }
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(view),
            None,
        )
    };
    view.addTrackingArea(&area);
}

fn set_hovered(corner: Option<usize>) {
    HOVERED.with(|h| h.set(corner));
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.diagram.setNeedsDisplay(true);
            for c in &ui.cards {
                c.setNeedsDisplay(true);
            }
        }
    });
}

fn draw_control(c: &Control) {
    let p = palette(c);
    let s = c.ivars().borrow();
    let b = c.bounds();
    let focused = c.focused();
    let alpha = if s.enabled { 1.0 } else { 0.3 };
    let mid = b.size.height / 2.0;
    match s.id {
        // `.chip`
        Ctl::Preset(_) => {
            let r = b.size.height / 2.0;
            let border = if s.hover { p.faint } else { p.line };
            rounded(b, r, Some(p.raised), Some(border));
            let text = if s.hover { p.text } else { p.dim };
            draw_text(
                &s.title,
                &font(11.5, 400.0),
                text,
                1.0,
                12.0,
                mid,
                b.size.width - 22.0,
            );
            if focused {
                focus_ring(b, r, p);
            }
        }
        // `.mini`
        Ctl::Test(_) => {
            let lit = s.enabled && (s.hover || s.down);
            rounded(b, 6.0, None, if lit { Some(p.line) } else { None });
            let text = if lit { p.text } else { p.faint };
            draw_text(
                &s.title,
                &font(10.5, 400.0),
                text,
                alpha,
                8.0,
                mid,
                b.size.width - 14.0,
            );
            if focused {
                focus_ring(b, 6.0, p);
            }
        }
        // `select`: sunken, a 7px radius, a chevron on the right.
        Ctl::ActionSelect(_) | Ctl::KeySelect(_) | Ctl::ModeSelect => {
            let border = if focused {
                p.accent
            } else if s.hover && s.enabled {
                p.faint
            } else {
                p.line
            };
            if focused {
                let glow = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    rect(-2.5, -2.5, b.size.width + 5.0, b.size.height + 5.0),
                    9.5,
                    9.5,
                );
                color_a(p.accent, 0.2).setStroke();
                glow.setLineWidth(3.0);
                glow.stroke();
            }
            rounded(b, 7.0, Some(p.sunken), Some(border));
            draw_text(
                &s.title,
                &font(13.0, 400.0),
                p.text,
                alpha,
                10.0,
                mid,
                b.size.width - 34.0,
            );
            let cx = b.size.width - 13.0;
            let chevron = NSBezierPath::bezierPath();
            chevron.moveToPoint(NSPoint::new(cx - 3.5, mid - 1.5));
            chevron.lineToPoint(NSPoint::new(cx, mid + 2.0));
            chevron.lineToPoint(NSPoint::new(cx + 3.5, mid - 1.5));
            chevron.setLineWidth(1.5);
            color_a(p.dim, alpha).setStroke();
            chevron.stroke();
        }
        // `.switch`: 38x22, an 11px-radius track, a 15px thumb.
        Ctl::Autostart => {
            if s.on {
                rounded(b, 11.0, Some(p.accent), None);
            } else {
                rounded(b, 11.0, Some(p.sunken), Some(p.line));
            }
            let x = if s.on { 3.0 + 16.0 } else { 3.0 };
            let thumb = NSBezierPath::bezierPathWithOvalInRect(rect(x, 3.5, 15.0, 15.0));
            color(if s.on { Rgb(0xff, 0xff, 0xff) } else { p.faint }).setFill();
            thumb.fill();
            if focused {
                focus_ring(b, 11.0, p);
            }
        }
        // `.link`
        Ctl::Log | Ctl::Folder => {
            let f = font(11.0, 400.0);
            let col = if s.hover { p.text } else { p.dim };
            draw_text(&s.title, &f, col, 1.0, 4.0, mid, b.size.width);
            if s.hover {
                let w = text_width(&s.title, &f);
                color(col).setFill();
                NSBezierPath::bezierPathWithRect(rect(4.0, mid + 6.5, w, 1.0)).fill();
            }
            if focused {
                focus_ring(b, 4.0, p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

fn press(c: &Control) {
    let (id, enabled, on) = {
        let s = c.ivars().borrow();
        (s.id, s.enabled, s.on)
    };
    if !enabled {
        return;
    }
    match id {
        Ctl::Preset(i) => {
            let kinds = PRESET_LIST[i].1;
            commit(kinds.map(|k| NativeCorner {
                action: action_of(k),
                modifier: Modifier::None,
            }));
        }
        Ctl::Test(i) => child::send(&serde_json::json!({ "cmd": "test", "corner": i })),
        Ctl::ActionSelect(_) | Ctl::KeySelect(_) | Ctl::ModeSelect => open_menu(c, id),
        Ctl::Autostart => {
            // Shown at once; the tray answers with what the OS really did.
            STATE.with(|s| s.borrow_mut().autostart = !on);
            refresh();
            child::send(&serde_json::json!({ "cmd": "autostart", "value": !on }));
        }
        Ctl::Log => child::send(&serde_json::json!({ "cmd": "openLog" })),
        Ctl::Folder => child::send(&serde_json::json!({ "cmd": "openConfigDir" })),
    }
}

/// A real macOS menu under the select, with a checkmark on the current choice
/// and, for actions, each action's symbol.
fn open_menu(c: &Control, id: Ctl) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let handler = UI.with(|ui| ui.borrow().as_ref().map(|u| u.handler.clone()));
    let Some(handler) = handler else { return };
    let corners = corners();
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let item = |title: &str, tag: isize, checked: bool| {
        let it = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(sel!(pick:)),
                &NSString::from_str(""),
            )
        };
        unsafe { it.setTarget(Some(&handler)) };
        it.setTag(tag);
        it.setState(if checked { 1 } else { 0 });
        it
    };
    match id {
        Ctl::ActionSelect(i) => {
            let current = kind_of(&corners[i].action);
            let mut n = 0;
            for (group, items) in CATALOG.iter() {
                if let Some(group) = group {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    let title = match *group {
                        "Cantos" => "Cantos \u{00B7} while Cantos runs",
                        other => other,
                    };
                    menu.addItem(&section_header(title, mtm));
                }
                for (kind, label) in items.iter() {
                    let it = item(label, (i * 1000 + n) as isize, *kind == current);
                    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        &NSString::from_str(symbol_for(kind)[0]),
                        None,
                    ) {
                        it.setImage(Some(&img));
                    }
                    menu.addItem(&it);
                    n += 1;
                }
            }
        }
        Ctl::KeySelect(i) => {
            for (n, (m, label)) in KEYS.iter().enumerate() {
                let tag = (i * 1000 + 100 + n) as isize;
                menu.addItem(&item(label, tag, *m == corners[i].modifier));
            }
        }
        Ctl::ModeSelect => {
            let current = monitor_mode();
            for (n, (m, label)) in MODES.iter().enumerate() {
                menu.addItem(&item(label, (MODE_TAG + n) as isize, *m == current));
            }
        }
        _ => return,
    }
    let b = c.bounds();
    menu.popUpMenuPositioningItem_atLocation_inView(
        None,
        NSPoint::new(0.0, b.size.height + 4.0),
        Some(c),
    );
}

/// A menu section heading: the system's own on macOS 14 and later, a
/// disabled item before that.
fn section_header(title: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let title = NSString::from_str(title);
    if objc2::available!(macos = 14.0) {
        NSMenuItem::sectionHeaderWithTitle(&title, mtm)
    } else {
        let it = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &title,
                None,
                &NSString::from_str(""),
            )
        };
        it.setEnabled(false);
        it
    }
}

define_class!(
    /// Menu picks in, and the window's lifecycle.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CantosSettingsHandler"]
    struct Handler;

    impl Handler {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            let tag = sender.tag().max(0) as usize;
            if let Some((mode, _)) = tag.checked_sub(MODE_TAG).and_then(|n| MODES.get(n)) {
                pick_mode(*mode);
                return;
            }
            let (corner, rest) = (tag / 1000, tag % 1000);
            if corner > 3 {
                return;
            }
            let mut c = corners();
            if rest >= 100 {
                c[corner].modifier = KEYS
                    .get(rest - 100)
                    .map(|(m, _)| *m)
                    .unwrap_or(Modifier::None);
            } else if let Some((kind, _)) = actions().get(rest) {
                // Choosing Custom again keeps what was typed.
                if !(*kind == "custom" && matches!(c[corner].action, Action::Custom { .. })) {
                    c[corner].action = action_of(kind);
                }
            }
            commit(c);
        }
    }

    unsafe impl NSObjectProtocol for Handler {}

    unsafe impl NSControlTextEditingDelegate for Handler {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            typed();
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        fn control_text_did_begin_editing(&self, _notification: &NSNotification) {
            repaint_commands();
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notification: &NSNotification) {
            repaint_commands();
        }
    }

    unsafe impl NSTextFieldDelegate for Handler {}

    unsafe impl NSWindowDelegate for Handler {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            quit();
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// A keystroke in a command field: take the fields' text into `STATE` now,
/// and send the corners once typing pauses, so a burst of keys is one save.
fn typed() {
    let mut c = corners();
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            for (i, (command, args)) in ui.fields.iter().enumerate() {
                if let Action::Custom { .. } = c[i].action {
                    c[i].action = Action::Custom {
                        command: command.stringValue().to_string(),
                        args: args.stringValue().to_string(),
                    };
                }
            }
        }
    });
    STATE.with(|s| s.borrow_mut().native = Some(c.to_vec()));
    let generation = TYPING_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let when =
        DispatchTime::try_from(std::time::Duration::from_millis(400)).unwrap_or(DispatchTime::NOW);
    let _ = DispatchQueue::main().after(when, move || {
        if TYPING_GEN.with(|g| g.get()) == generation {
            child::send(&serde_json::json!({ "cmd": "native", "corners": corners() }));
        }
    });
}

fn repaint_commands() {
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.commands.setNeedsDisplay(true);
        }
    });
}

/// Show the new corners at once, then hand them to the tray to write to the
/// Dock. Its `state` push that follows is what the Dock really has.
fn commit(c: [NativeCorner; 4]) {
    STATE.with(|s| s.borrow_mut().native = Some(c.to_vec()));
    refresh();
    child::send(&serde_json::json!({ "cmd": "native", "corners": c }));
}

/// Show the new mode at once; the tray's `state` push that follows brings
/// the diagram's corners for it.
fn pick_mode(mode: MonitorMode) {
    STATE.with(|s| {
        if let Some(c) = s.borrow_mut().config.as_mut() {
            c.monitor_mode = mode;
        }
    });
    refresh();
    child::send(&serde_json::json!({ "cmd": "monitorMode", "mode": mode }));
}

fn quit() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

// ---------------------------------------------------------------------------
// Building and laying out
// ---------------------------------------------------------------------------

fn build(mtm: MainThreadMarker) -> Ui {
    let handler = Handler::new(mtm);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::FullSizeContentView;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, WIDTH, 700.0),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // Owned by Rust, not by AppKit's close: without this, closing releases
    // the window a second time.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Cantos"));
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    window.setMovableByWindowBackground(true);
    window.setAutorecalculatesKeyViewLoop(true);
    window.setDelegate(Some(ProtocolObject::from_ref(&*handler)));

    let root: Retained<Surface> =
        unsafe { msg_send![Surface::alloc(mtm), initWithFrame: rect(0.0, 0.0, WIDTH, 700.0)] };
    window.setContentView(Some(&root));

    let stage: Retained<Stage> =
        unsafe { msg_send![Stage::alloc(mtm), initWithFrame: NSRect::ZERO] };
    let diagram: Retained<Diagram> =
        unsafe { msg_send![Diagram::alloc(mtm), initWithFrame: NSRect::ZERO] };
    stage.addSubview(&diagram);
    root.addSubview(&stage);

    let chips: Vec<_> = PRESET_LIST
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let c = Control::new(mtm, Ctl::Preset(i), name);
            root.addSubview(&c);
            c
        })
        .collect();

    let (mut cards, mut tests, mut selects, mut keys) = (vec![], vec![], vec![], vec![]);
    for i in 0..4 {
        let card = Card::new(mtm, i);
        let test = Control::new(mtm, Ctl::Test(i), "Test");
        let select = Control::new(mtm, Ctl::ActionSelect(i), "");
        let key = Control::new(mtm, Ctl::KeySelect(i), "");
        card.addSubview(&test);
        card.addSubview(&select);
        card.addSubview(&key);
        root.addSubview(&card);
        cards.push(card);
        tests.push(test);
        selects.push(select);
        keys.push(key);
    }

    let commands: Retained<Commands> =
        unsafe { msg_send![Commands::alloc(mtm), initWithFrame: NSRect::ZERO] };
    let (mut fields, mut command_tests) = (vec![], vec![]);
    for (i, name) in CORNER_NAMES.iter().enumerate() {
        let make = |placeholder: &str, label: String| {
            let f = NSTextField::new(mtm);
            f.setBezeled(false);
            f.setBordered(false);
            f.setDrawsBackground(false);
            f.setFocusRingType(NSFocusRingType::None);
            f.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            f.setAccessibilityLabel(Some(&NSString::from_str(&label)));
            // The delegate is held weakly; `Ui` keeps the handler alive for
            // as long as the fields exist.
            unsafe { f.setDelegate(Some(ProtocolObject::from_ref(&*handler))) };
            commands.addSubview(&f);
            f
        };
        let command = make(
            "App, program, script, file or URL",
            format!("{name} command"),
        );
        let args = make("Arguments", format!("{name} arguments"));
        fields.push((command, args));
        let test = Control::new(mtm, Ctl::Test(i), "Test");
        commands.addSubview(&test);
        command_tests.push(test);
    }
    root.addSubview(&commands);

    let panel: Retained<Panel> =
        unsafe { msg_send![Panel::alloc(mtm), initWithFrame: NSRect::ZERO] };
    let switch = Control::new(mtm, Ctl::Autostart, "Open at login");
    panel.addSubview(&switch);
    let mode = Control::new(mtm, Ctl::ModeSelect, "");
    panel.addSubview(&mode);
    root.addSubview(&panel);

    let links: Vec<_> = [(Ctl::Log, "Log"), (Ctl::Folder, "Config folder")]
        .into_iter()
        .map(|(id, t)| {
            let c = Control::new(mtm, id, t);
            root.addSubview(&c);
            c
        })
        .collect();

    Ui {
        window,
        handler,
        root,
        stage,
        diagram,
        chips,
        cards,
        tests,
        selects,
        keys,
        commands,
        fields,
        command_tests,
        panel,
        switch,
        mode,
        links,
    }
}

/// Place everything for the current display arrangement, which decides the
/// diagram's height, and size the window to fit, keeping its top edge put.
fn layout(ui: &Ui) {
    let displays = STATE.with(|s| s.borrow().displays.clone());
    let dh = diagram_height(&displays);

    let mut y = HEADER_Y + HEADER_H + 18.0;
    let stage_h = STAGE_PAD + dh + STATUS_H;
    ui.stage.setFrame(rect(PAD, y, INNER, stage_h));
    ui.diagram
        .setFrame(rect(STAGE_PAD, STAGE_PAD, INNER - 2.0 * STAGE_PAD, dh));
    y += stage_h + 12.0;

    let presets_y = y;
    let mut x = PAD + text_width("Presets", &CAPS) + 9.0;
    for c in &ui.chips {
        let w = text_width(&c.ivars().borrow().title, &font(11.5, 400.0)) + 24.0;
        c.setFrame(rect(x, y, w, CHIP_H));
        x += w + 7.0;
    }
    y += CHIP_H + 12.0;

    let test_w = text_width("Test", &font(10.5, 400.0)) + 16.0;
    let select_x = 12.0 + 16.0 + 8.0;
    for i in 0..4 {
        let (cx, cy) = (
            PAD + (i % 2) as f64 * (CARD_W + CARD_GAP),
            y + (i / 2) as f64 * (CARD_H + CARD_GAP),
        );
        ui.cards[i].setFrame(rect(cx, cy, CARD_W, CARD_H));
        ui.tests[i].setFrame(rect(CARD_W - 10.0 - test_w, 7.0, test_w, 20.0));
        ui.selects[i].setFrame(rect(
            select_x,
            36.0,
            CARD_W - select_x - 12.0 - 8.0 - KEY_W,
            SELECT_H,
        ));
        ui.keys[i].setFrame(rect(CARD_W - 12.0 - KEY_W, 36.0, KEY_W, SELECT_H));
    }
    y += 2.0 * CARD_H + CARD_GAP + 14.0;

    // Custom commands, only while a corner has one.
    let custom = custom_corners();
    ui.commands.setHidden(custom.is_empty());
    for i in 0..4 {
        let (command, args) = &ui.fields[i];
        let row = custom.iter().position(|&c| c == i);
        command.setHidden(row.is_none());
        args.setHidden(row.is_none());
        ui.command_tests[i].setHidden(row.is_none());
        if let Some(row) = row {
            let ry = 26.0 + row as f64 * COMMAND_ROW_H;
            let (cb, ab) = command_boxes(INNER, ry);
            // The field's own text box, inset into the drawn one and centred.
            let inset = |r: NSRect| {
                rect(
                    r.origin.x + 9.0,
                    r.origin.y + 6.0,
                    r.size.width - 18.0,
                    18.0,
                )
            };
            command.setFrame(inset(cb));
            args.setFrame(inset(ab));
            let tw = text_width("Test", &font(10.5, 400.0)) + 16.0;
            ui.command_tests[i].setFrame(rect(INNER - 10.0 - tw, ry + 9.0, tw, 20.0));
        }
    }
    if !custom.is_empty() {
        let h = 26.0 + custom.len() as f64 * COMMAND_ROW_H;
        ui.commands.setFrame(rect(PAD, y, INNER, h));
        ui.commands.setNeedsDisplay(true);
        y += h + 12.0;
    }

    ui.panel.setFrame(rect(PAD, y, INNER, PANEL_H));
    ui.switch.setFrame(rect(
        INNER - 14.0 - SWITCH_W,
        26.0 + (PANEL_ROW_H - SWITCH_H) / 2.0,
        SWITCH_W,
        SWITCH_H,
    ));
    ui.mode.setFrame(rect(
        INNER - 14.0 - MODE_W,
        26.0 + PANEL_ROW_H + (PANEL_ROW_H - SELECT_H) / 2.0,
        MODE_W,
        SELECT_H,
    ));
    y += PANEL_H + 16.0;

    let footer_y = y;
    let mut x = WIDTH - PAD;
    for c in ui.links.iter().rev() {
        let w = text_width(&c.ivars().borrow().title, &font(11.0, 400.0)) + 8.0;
        x -= w;
        c.setFrame(rect(x, footer_y + 13.0, w, 18.0));
        x -= 6.0;
    }
    ROWS.with(|r| r.set((presets_y, footer_y)));

    let height = footer_y + 13.0 + 18.0 + 22.0;
    let frame = ui.window.frame();
    let content = ui.window.contentRectForFrameRect(frame);
    if (content.size.height - height).abs() > 0.5 {
        let top = frame.origin.y + frame.size.height;
        let new = ui
            .window
            .frameRectForContentRect(rect(frame.origin.x, 0.0, WIDTH, height));
        ui.window.setFrame_display(
            rect(
                frame.origin.x,
                top - new.size.height,
                new.size.width,
                new.size.height,
            ),
            true,
        );
    }
    ui.root.setFrame(rect(0.0, 0.0, WIDTH, height));
    ui.root.setNeedsDisplay(true);
}

/// Bring every control in line with `STATE`.
fn refresh() {
    let state = STATE.with(|s| s.borrow().clone());
    let corners = corners();
    let list = actions();
    UI.with(|ui| {
        let ui = ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        for (i, corner) in corners.iter().enumerate() {
            let kind = kind_of(&corner.action);
            let label = match kind.as_str() {
                // The menu says "Custom command"; the button has less room.
                "custom" => "Custom",
                _ => list
                    .iter()
                    .find(|(k, _)| *k == kind)
                    .map_or("Nothing", |(_, l)| l),
            };
            ui.selects[i].set(Some(label), true, false);
            let key = key_short(corner.modifier);
            ui.keys[i].set(Some(key), kind != "none", false);
            let testable = kind != "none" && !UNTESTABLE.contains(&kind.as_str());
            ui.tests[i].set(None, testable, false);
            ui.cards[i].setNeedsDisplay(true);
        }
        ui.switch.set(None, true, state.autostart);
        let mode = monitor_mode();
        let mode_label = MODES.iter().find(|(m, _)| *m == mode).map_or("", |(_, l)| l);
        ui.mode.set(Some(mode_label), true, false);
        let text = color(palette(&ui.root).text);
        for (i, (command, args)) in ui.fields.iter().enumerate() {
            command.setTextColor(Some(&text));
            args.setTextColor(Some(&text));
            // Never under the user's cursor: the state that comes back after a
            // save is what they just typed, and resetting the field would
            // move the caret.
            if let Action::Custom { command: c, args: a } = &corners[i].action {
                if command.currentEditor().is_none() {
                    command.setStringValue(&NSString::from_str(c));
                }
                if args.currentEditor().is_none() {
                    args.setStringValue(&NSString::from_str(a));
                }
            }
            let testable = matches!(&corners[i].action, Action::Custom { command, .. } if !command.trim().is_empty());
            ui.command_tests[i].set(None, testable, false);
        }
        layout(ui);
        ui.stage.setNeedsDisplay(true);
        ui.diagram.setNeedsDisplay(true);
    });
}

fn show_toast(message: &str, bad: bool) {
    let generation = TOAST_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    TOAST.with(|t| *t.borrow_mut() = (message.into(), bad));
    repaint_root();
    let when =
        DispatchTime::try_from(std::time::Duration::from_millis(2500)).unwrap_or(DispatchTime::NOW);
    let _ = DispatchQueue::main().after(when, move || {
        if TOAST_GEN.with(|g| g.get()) == generation {
            TOAST.with(|t| t.borrow_mut().0.clear());
            repaint_root();
        }
    });
}

fn repaint_root() {
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.root.setNeedsDisplay(true);
        }
    });
}

// ---------------------------------------------------------------------------
// Calls from the tray
// ---------------------------------------------------------------------------

fn apply(push: child::Push) {
    let arg = |i: usize| push.args.get(i).cloned().unwrap_or_default();
    match push.push.as_str() {
        "state" => {
            match serde_json::from_value::<State>(arg(0)) {
                Ok(s) => STATE.with(|state| *state.borrow_mut() = s),
                Err(e) => lerror!("settings window: bad state push ({e})"),
            }
            refresh();
        }
        "geometry" => {
            let g = arg(0);
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if let Ok(d) = serde_json::from_value(g["displays"].clone()) {
                    s.displays = d;
                }
                if let Ok(h) = serde_json::from_value(g["hitboxes"].clone()) {
                    s.hitboxes = h;
                }
            });
            refresh();
        }
        "toast" => show_toast(
            arg(0).as_str().unwrap_or(""),
            arg(1).as_bool().unwrap_or(false),
        ),
        "autostart" => {
            STATE.with(|s| s.borrow_mut().autostart = arg(0).as_bool().unwrap_or(false));
            refresh();
            if let Some(e) = arg(1).as_str() {
                show_toast(e, true);
            }
        }
        "raise" => activate(),
        other => lerror!("settings window: unknown push {other:?}"),
    }
}

fn activate() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    #[allow(deprecated)] // `activate` is macOS 14+; this works on 11 through 26
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.window.makeKeyAndOrderFront(None);
        }
    });
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// `cantos --settings-window`: run the window until it is closed.
pub fn run() {
    let Some(mtm) = MainThreadMarker::new() else {
        lerror!("settings window: not on the main thread");
        return;
    };
    linfo!(
        "settings window process started (pid {})",
        std::process::id()
    );
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon of its own: it belongs to the menu bar app.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let _menu = app_menu();

    let ui = build(mtm);
    UI.with(|u| *u.borrow_mut() = Some(ui));
    refresh();
    repaint_all();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.window.center();
        }
    });

    // The tray's calls arrive on stdin; AppKit is only touched on the main
    // queue. End of input means the tray has gone, so the window goes too.
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<child::Push>(&line) {
                Ok(push) => DispatchQueue::main().exec_async(move || apply(push)),
                Err(e) => lerror!("settings window: unreadable line from the tray ({e})"),
            }
        }
        DispatchQueue::main().exec_async(quit);
    });

    child::send(&serde_json::json!({ "cmd": "ready" }));
    activate();
    app.run();
}

/// `cantos --settings-window --preview out.png [dark]`: draw the window with
/// sample corners into a PNG and exit, without showing anything. For checking
/// the design, including on a machine whose screen is off.
pub fn preview(path: &str, dark: bool) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let ui = build(mtm);
    let name = unsafe {
        if dark {
            objc2_app_kit::NSAppearanceNameDarkAqua
        } else {
            objc2_app_kit::NSAppearanceNameAqua
        }
    };
    if let Some(appearance) = objc2_app_kit::NSAppearance::appearanceNamed(name) {
        app.setAppearance(Some(&appearance));
        ui.window.setAppearance(Some(&appearance));
    }
    let corner = |kind: &str, modifier| NativeCorner {
        action: action_of(kind),
        modifier,
    };
    STATE.with(|s| {
        *s.borrow_mut() = State {
            config: Some(ConfigView {
                monitor_mode: MonitorMode::OuterCorners,
            }),
            native: Some(vec![
                corner("lockWorkstation", Modifier::None),
                corner("taskView", Modifier::None),
                corner("search", Modifier::Win),
                NativeCorner {
                    action: Action::Custom {
                        command: "/Applications/Ghostty.app".into(),
                        args: String::new(),
                    },
                    modifier: Modifier::None,
                },
            ]),
            displays: vec![DisplayView {
                left: 0,
                top: 0,
                right: 1512,
                bottom: 982,
                primary: true,
            }],
            hitboxes: CORNER_KEYS
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let (x, y) = (
                        if i % 2 == 0 { 0 } else { 1504 },
                        if i < 2 { 0 } else { 974 },
                    );
                    HitboxView {
                        corner: (*k).into(),
                        left: x,
                        top: y,
                        right: x + 8,
                        bottom: y + 8,
                    }
                })
                .collect(),
            version: env!("CARGO_PKG_VERSION").into(),
            autostart: true,
        }
    });
    HOVERED.with(|h| h.set(Some(1)));
    UI.with(|u| *u.borrow_mut() = Some(ui));
    refresh();
    repaint_all();

    UI.with(|u| {
        let u = u.borrow();
        let ui = u.as_ref().ok_or("no window")?;
        let view: &NSView = &ui.root;
        let bounds = view.bounds();
        let rep = view
            .bitmapImageRepForCachingDisplayInRect(bounds)
            .ok_or("could not make a bitmap")?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let png = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }
        .ok_or("could not encode PNG")?;
        std::fs::write(path, png.to_vec()).map_err(|e| e.to_string())
    })
}

/// Cmd+W closes and Cmd+Q quits, as in any Mac window. Kept alive by the
/// caller for as long as the app runs.
fn app_menu() -> tray_icon::menu::Menu {
    use tray_icon::menu::{Menu, PredefinedMenuItem, Submenu};
    let menu = Menu::new();
    if let Ok(app) = Submenu::with_items(
        "Cantos",
        true,
        &[
            &PredefinedMenuItem::close_window(None),
            &PredefinedMenuItem::quit(None),
        ],
    ) {
        let _ = menu.append(&app);
    }
    menu.init_for_nsapp();
    menu
}
