//! The native Linux settings window, in GTK 3.
//!
//! Run as its own process (`cantos --settings-window`, see [`super::child`]):
//! it reads the tray's calls from stdin and writes the page's messages to
//! stdout, so the tray cannot tell it from the web page.
//!
//! The web page's design in GTK widgets: GTK styles with CSS, so the page's
//! colour tokens and component rules carry over nearly as written (see
//! [`css`]), and the widgets underneath (combo boxes, scales, switches,
//! entries) are GTK's own, with its keyboard handling and accessibility.
//! GTK 3 is already in the tree for the tray icon, so this costs the binary
//! almost nothing.

use super::child;
use crate::actions::CATALOG;
use crate::config::{Action, Config, Modifier, MonitorMode};
use crate::log::{lerror, linfo};

use gtk::glib;
use gtk::prelude::*;
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::io::BufRead;
use std::time::Duration;

const CORNER_NAMES: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
const CORNER_KEYS: [&str; 4] = ["topLeft", "topRight", "bottomLeft", "bottomRight"];
/// The page's corner glyphs, as the four corner brackets in Unicode.
const CORNER_GLYPHS: [&str; 4] = ["\u{231C}", "\u{231D}", "\u{231E}", "\u{231F}"];

const MODIFIERS: [(&str, &str); 5] = [
    ("none", "Not required"),
    ("ctrl", "Ctrl"),
    ("alt", "Alt"),
    ("shift", "Shift"),
    ("win", "Super"),
];

const PRESETS: [(&str, [&str; 4]); 3] = [
    ("Recommended", ["taskView", "showDesktop", "none", "none"]),
    ("Minimal", ["taskView", "none", "none", "none"]),
    ("Clear all", ["none", "none", "none", "none"]),
];

/// A symbolic icon per action, from the freedesktop icon names every GTK
/// theme carries.
fn icon_for(kind: &str) -> &'static str {
    match kind {
        "taskView" => "view-app-grid-symbolic",
        "showDesktop" => "user-desktop-symbolic",
        "desktopLeft" => "go-previous-symbolic",
        "desktopRight" => "go-next-symbolic",
        "fileExplorer" => "folder-symbolic",
        "screenshot" => "camera-photo-symbolic",
        "settings" => "emblem-system-symbolic",
        "lockWorkstation" => "system-lock-screen-symbolic",
        "screensaver" => "weather-clear-night-symbolic",
        "displayOff" => "video-display-symbolic",
        "custom" => "utilities-terminal-symbolic",
        _ => "list-remove-symbolic",
    }
}

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

// ---------------------------------------------------------------------------
// Styling: the original web page's tokens and rules, in GTK's CSS
// ---------------------------------------------------------------------------

struct Tokens {
    bg: &'static str,
    raised: &'static str,
    sunken: &'static str,
    line: &'static str,
    line_soft: &'static str,
    text: &'static str,
    dim: &'static str,
    faint: &'static str,
    accent: &'static str,
    accent_dim: &'static str,
    danger: &'static str,
    /// `color-mix(accent 14%, raised)` and `color-mix(text 6%, raised)`, the
    /// diagram's primary and secondary screens, worked out once.
    primary_screen: (f64, f64, f64),
    secondary_screen: (f64, f64, f64),
}

const DARK: Tokens = Tokens {
    bg: "#0e1016",
    raised: "#161a24",
    sunken: "#0a0c11",
    line: "#242938",
    line_soft: "#1b1f2b",
    text: "#e9ecf4",
    dim: "#97a0b5",
    faint: "#5d657a",
    accent: "#6ea8fe",
    accent_dim: "#2f4a78",
    danger: "#ef7676",
    primary_screen: (0.144, 0.191, 0.273),
    secondary_screen: (0.137, 0.155, 0.199),
};

const LIGHT: Tokens = Tokens {
    bg: "#f2f4f9",
    raised: "#ffffff",
    sunken: "#eaeef6",
    line: "#d3dae8",
    line_soft: "#e5eaf2",
    text: "#151925",
    dim: "#59627a",
    faint: "#939cb1",
    accent: "#2563c9",
    accent_dim: "#a8c1ea",
    danger: "#c2402f",
    primary_screen: (0.880, 0.914, 0.970),
    secondary_screen: (0.945, 0.946, 0.948),
};

/// Whether the desktop asks for dark: GNOME's colour scheme, or a theme
/// whose name says so.
fn prefers_dark() -> bool {
    let scheme = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("dark"))
        .unwrap_or(false);
    let theme = gtk::Settings::default()
        .and_then(|s| s.gtk_theme_name())
        .is_some_and(|t| t.to_lowercase().contains("dark"));
    scheme || theme || std::env::var("GTK_THEME").is_ok_and(|t| t.to_lowercase().contains("dark"))
}

fn css(t: &Tokens) -> String {
    format!(
        r#"
window, .surface {{ background-color: {bg}; color: {text}; }}
* {{ font-size: 13px; }}
label {{ color: {text}; }}
.h1 {{ font-size: 15px; font-weight: 600; }}
.sub {{ font-size: 11.5px; color: {dim}; }}
.caps {{ font-size: 10.5px; font-weight: 600; letter-spacing: 0.7px; color: {faint}; }}
.caps.on {{ color: {dim}; }}
.dim {{ color: {dim}; }}
.faint {{ color: {faint}; }}

.stage, .card, .panel {{
  background-color: {raised}; border: 1px solid {line_soft};
}}
.stage, .panel {{ border-radius: 12px; }}
.card {{ border-radius: 11px; padding: 10px 12px 12px 12px; }}
.card.hover {{ border-color: {line}; }}
.card.on {{ border-color: {accent_dim}; }}
.cnr {{ color: {faint}; font-weight: 700; }}
.cnr.on, .glyph.on {{ color: {accent}; }}
.glyph {{ color: {faint}; }}
.status {{ font-size: 11.5px; color: {dim}; }}
.dot {{ color: {faint}; font-size: 9px; }}
.dot.on {{ color: {accent}; }}

.row {{ border-top: 1px solid {line_soft}; padding: 11px 14px; }}
.row.first {{ border-top: none; }}
.nm {{ font-size: 12.5px; }}
.hint {{ font-size: 11px; color: {dim}; }}

button.chip {{
  background: {raised}; background-image: none; color: {dim};
  border: 1px solid {line}; border-radius: 999px; padding: 3px 11px;
  box-shadow: none; text-shadow: none; font-size: 11.5px;
}}
button.chip label {{ color: {dim}; font-size: 11.5px; }}
button.chip:hover {{ border-color: {faint}; }}
button.chip:hover label {{ color: {text}; }}

button.mini {{
  background: none; background-image: none; border: 1px solid transparent;
  border-radius: 6px; padding: 1px 7px; box-shadow: none; min-height: 0;
}}
button.mini label {{ color: {faint}; font-size: 10.5px; }}
button.mini:hover {{ border-color: {line}; }}
button.mini:hover label {{ color: {text}; }}
button.mini:disabled {{ opacity: 0.3; }}

button.link {{
  background: none; background-image: none; border: none; box-shadow: none;
  padding: 2px 4px; min-height: 0;
}}
button.link label {{ color: {dim}; font-size: 11px; }}
button.link:hover label {{ color: {text}; }}

combobox button, entry {{
  background: {sunken}; background-image: none; color: {text};
  border: 1px solid {line}; border-radius: 7px; box-shadow: none;
  padding: 4px 9px; min-height: 22px;
}}
combobox button label {{ color: {text}; }}
combobox button:hover, entry:hover {{ border-color: {faint}; }}
combobox button:focus, entry:focus {{
  border-color: {accent}; box-shadow: 0 0 0 3px alpha({accent}, 0.2);
}}
combobox arrow {{ color: {dim}; }}
entry {{ caret-color: {accent}; }}
entry selection {{ background-color: {accent_dim}; }}

.seg {{ background: {sunken}; border: 1px solid {line}; border-radius: 8px; padding: 2px; }}
.seg button {{
  background: none; background-image: none; border: none; box-shadow: none;
  border-radius: 6px; padding: 3px 10px; min-height: 0;
}}
.seg button label {{ color: {dim}; font-size: 11.5px; }}
.seg button:hover label {{ color: {text}; }}
.seg button:checked {{ background: {raised}; }}
.seg button:checked label {{ color: {text}; }}

switch {{
  background: {sunken}; background-image: none; border: 1px solid {line};
  border-radius: 11px; min-width: 36px; min-height: 20px; box-shadow: none;
}}
switch:checked {{ background: {accent}; border-color: transparent; }}
switch slider {{
  background: {faint}; background-image: none; border: none; box-shadow: none;
  border-radius: 50%; min-width: 15px; min-height: 15px; margin: 2px;
}}
switch:checked slider {{ background: #ffffff; }}

scale trough {{ background: {sunken}; border: none; min-height: 3px; border-radius: 2px; }}
scale highlight {{ background: {accent}; border: none; border-radius: 2px; }}
scale slider {{
  background: #ffffff; background-image: none; border: 1px solid rgba(0,0,0,0.3);
  min-width: 13px; min-height: 13px; margin: -6px 0; border-radius: 50%;
  box-shadow: 0 1px 3px rgba(0,0,0,0.35);
}}
.output {{ font-size: 11.5px; color: {dim}; }}

.footer {{ border-top: 1px solid {line_soft}; padding-top: 13px; }}
.version {{ font-size: 11px; color: {faint}; }}
.toast {{ font-size: 11px; color: {dim}; }}
.toast.bad {{ color: {danger}; }}
"#,
        bg = t.bg,
        raised = t.raised,
        sunken = t.sunken,
        line = t.line,
        line_soft = t.line_soft,
        text = t.text,
        dim = t.dim,
        faint = t.faint,
        accent = t.accent,
        accent_dim = t.accent_dim,
        danger = t.danger,
    )
}

fn rgb(hex: &str) -> (f64, f64, f64) {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    (
        ((v >> 16) & 0xff) as f64 / 255.0,
        ((v >> 8) & 0xff) as f64 / 255.0,
        (v & 0xff) as f64 / 255.0,
    )
}

// ---------------------------------------------------------------------------
// State
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
}

struct Ui {
    window: gtk::Window,
    enabled: gtk::Switch,
    diagram: gtk::DrawingArea,
    dot: gtk::Label,
    status: gtk::Label,
    cards: Vec<gtk::Box>,
    names: Vec<gtk::Label>,
    glyphs: Vec<gtk::Label>,
    icons: Vec<gtk::Image>,
    selects: Vec<gtk::ComboBoxText>,
    tests: Vec<gtk::Button>,
    custom: gtk::Box,
    custom_rows: Vec<(gtk::Box, gtk::Entry, gtk::Entry)>,
    dwell: (gtk::Scale, gtk::Label),
    size: (gtk::Scale, gtk::Label),
    cool: (gtk::Scale, gtk::Label),
    modifier: gtk::ComboBoxText,
    mode: Vec<(MonitorMode, gtk::ToggleButton)>,
    mode_hint: gtk::Label,
    fullscreen: gtk::Switch,
    dragging: gtk::Switch,
    autostart: gtk::Switch,
    version: gtk::Label,
    toast: gtk::Label,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// Set while the controls are being brought in line with `STATE`, so
    /// their change handlers do not echo that back as edits.
    static SYNCING: Cell<bool> = const { Cell::new(false) };
    static HOVERED: Cell<Option<usize>> = const { Cell::new(None) };
    static SAVE_GEN: Cell<u64> = const { Cell::new(0) };
    static TOAST_GEN: Cell<u64> = const { Cell::new(0) };
    static DARK_MODE: Cell<bool> = const { Cell::new(true) };
}

fn tokens() -> &'static Tokens {
    if DARK_MODE.with(|d| d.get()) {
        &DARK
    } else {
        &LIGHT
    }
}

fn config() -> Config {
    STATE.with(|s| s.borrow().config.clone().unwrap_or_default())
}

/// Edit the config, show the edit, and save it once edits pause: a dragged
/// slider is one save, not one per step. The page waits 140ms; so does this.
fn edit(f: impl FnOnce(&mut Config)) {
    if SYNCING.with(|s| s.get()) {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mut c = s.config.clone().unwrap_or_default();
        f(&mut c);
        s.config = Some(c);
    });
    refresh();
    let generation = SAVE_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    glib::timeout_add_local_once(Duration::from_millis(140), move || {
        if SAVE_GEN.with(|g| g.get()) == generation {
            child::send(&serde_json::json!({ "cmd": "set", "config": config() }));
        }
    });
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    for c in classes {
        l.style_context().add_class(c);
    }
    l
}

fn classed<W: IsA<gtk::Widget>>(w: &W, classes: &[&str]) {
    for c in classes {
        w.style_context().add_class(c);
    }
}

/// A `.row`: name and hint on the left, a control on the right.
fn row(name: &str, hint: &str, control: &impl IsA<gtk::Widget>, first: bool) -> gtk::Box {
    row_with_hint(name, hint, control, first).0
}

/// [`row`], also handing back the hint, for a row whose hint changes.
fn row_with_hint(
    name: &str,
    hint: &str,
    control: &impl IsA<gtk::Widget>,
    first: bool,
) -> (gtk::Box, gtk::Label) {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    classed(&r, &["row"]);
    if first {
        classed(&r, &["first"]);
    }
    let info = gtk::Box::new(gtk::Orientation::Vertical, 2);
    info.set_hexpand(true);
    info.pack_start(&label(name, &["nm"]), false, false, 0);
    let h = label(hint, &["hint"]);
    h.set_line_wrap(true);
    info.pack_start(&h, false, false, 0);
    r.pack_start(&info, true, true, 0);
    control.as_ref().set_valign(gtk::Align::Center);
    r.pack_end(control, false, false, 0);
    (r, h)
}

fn panel(title: &str) -> gtk::Box {
    let p = gtk::Box::new(gtk::Orientation::Vertical, 0);
    classed(&p, &["panel"]);
    let h2 = label(&title.to_uppercase(), &["caps"]);
    h2.set_margin_start(14);
    h2.set_margin_top(12);
    p.pack_start(&h2, false, false, 0);
    p
}

fn slider(min: f64, max: f64, step: f64) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 11);
    let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    s.set_draw_value(false);
    s.set_size_request(128, -1);
    let out = label("", &["output"]);
    out.set_xalign(1.0);
    out.set_width_chars(7);
    b.pack_start(&s, false, false, 0);
    b.pack_start(&out, false, false, 0);
    (b, s, out)
}

fn build() -> Ui {
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("Cantos");
    window.set_default_size(680, 828);
    window.set_icon(Some(&gtk::gdk_pixbuf::Pixbuf::from_mut_slice(
        super::icon_rgba(),
        gtk::gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        64,
        64,
        64 * 4,
    )));

    let scroller = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    window.add(&scroller);

    let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    classed(&wrap, &["surface"]);
    wrap.set_margin_start(24);
    wrap.set_margin_end(24);
    wrap.set_margin_top(20);
    wrap.set_margin_bottom(28);
    scroller.add(&wrap);

    // Header: the mark, the titles, the master switch.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 11);
    header.set_margin_bottom(18);
    let mark = gtk::DrawingArea::new();
    mark.set_size_request(28, 28);
    mark.set_valign(gtk::Align::Center);
    mark.connect_draw(|_, cr| {
        draw_mark(cr, 28.0);
        glib::Propagation::Proceed
    });
    header.pack_start(&mark, false, false, 0);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
    titles.pack_start(&label("Cantos", &["h1"]), false, false, 0);
    titles.pack_start(
        &label(
            "Throw the cursor into a corner to trigger an action",
            &["sub"],
        ),
        false,
        false,
        0,
    );
    header.pack_start(&titles, true, true, 0);
    let enabled = gtk::Switch::new();
    enabled.set_valign(gtk::Align::Center);
    enabled.set_tooltip_text(Some("Enable hot corners"));
    enabled.connect_state_set(|_, on| {
        edit(|c| c.enabled = on);
        glib::Propagation::Proceed
    });
    header.pack_end(&enabled, false, false, 0);
    wrap.pack_start(&header, false, false, 0);

    // The stage: the diagram and the status line.
    let stage = gtk::Box::new(gtk::Orientation::Vertical, 0);
    classed(&stage, &["stage"]);
    stage.set_margin_bottom(12);
    let diagram = gtk::DrawingArea::new();
    diagram.set_size_request(-1, 190);
    diagram.set_margin_start(14);
    diagram.set_margin_end(14);
    diagram.set_margin_top(14);
    diagram.connect_draw(|w, cr| {
        draw_diagram(cr, w.allocated_width() as f64, w.allocated_height() as f64);
        glib::Propagation::Proceed
    });
    stage.pack_start(&diagram, false, false, 0);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.set_margin_start(16);
    bar.set_margin_top(4);
    bar.set_margin_bottom(9);
    bar.set_size_request(-1, 30);
    let dot = label("\u{25CF}", &["dot"]);
    let status = label("", &["status"]);
    bar.pack_start(&dot, false, false, 0);
    bar.pack_start(&status, false, false, 0);
    stage.pack_start(&bar, false, false, 0);
    wrap.pack_start(&stage, false, false, 0);

    // Presets.
    let presets = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    presets.set_margin_bottom(12);
    let lbl = label("PRESETS", &["caps"]);
    lbl.set_margin_end(2);
    presets.pack_start(&lbl, false, false, 0);
    for (name, kinds) in PRESETS {
        let b = gtk::Button::with_label(name);
        classed(&b, &["chip"]);
        b.connect_clicked(move |_| {
            edit(|c| {
                for (i, k) in kinds.iter().enumerate() {
                    c.corners[i] = action_from(k, &c.corners[i]);
                }
            });
        });
        presets.pack_start(&b, false, false, 0);
    }
    wrap.pack_start(&presets, false, false, 0);

    // The four corner cards.
    let grid = gtk::Grid::new();
    grid.set_column_spacing(9);
    grid.set_row_spacing(9);
    grid.set_column_homogeneous(true);
    grid.set_margin_bottom(14);
    let list = actions();
    let (mut cards, mut names, mut glyphs, mut icons, mut selects, mut tests) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    for i in 0..4 {
        let hover = gtk::EventBox::new();
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        classed(&card, &["card"]);
        hover.add(&card);
        hover.connect_enter_notify_event(move |_, _| {
            set_hovered(Some(i));
            glib::Propagation::Proceed
        });
        hover.connect_leave_notify_event(move |_, e| {
            // Moving onto a child widget also "leaves"; only a real exit counts.
            if e.detail() != gtk::gdk::NotifyType::Inferior {
                set_hovered(None);
            }
            glib::Propagation::Proceed
        });

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let glyph = label(CORNER_GLYPHS[i], &["cnr"]);
        let name = label(&CORNER_NAMES[i].to_uppercase(), &["caps"]);
        let test = gtk::Button::with_label("Test");
        classed(&test, &["mini"]);
        test.set_tooltip_text(Some("Run this action now"));
        test.connect_clicked(move |_| {
            child::send(&serde_json::json!({ "cmd": "test", "corner": i }));
        });
        top.pack_start(&glyph, false, false, 0);
        top.pack_start(&name, true, true, 0);
        top.pack_end(&test, false, false, 0);
        card.pack_start(&top, false, false, 0);

        let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::from_icon_name(Some(icon_for("none")), gtk::IconSize::Button);
        classed(&icon, &["glyph"]);
        let select = gtk::ComboBoxText::new();
        for (kind, text) in &list {
            select.append(Some(kind), text);
        }
        select.set_hexpand(true);
        select.connect_changed(move |s| {
            let Some(kind) = s.active_id() else { return };
            edit(|c| c.corners[i] = action_from(&kind, &c.corners[i]));
        });
        row2.pack_start(&icon, false, false, 0);
        row2.pack_start(&select, true, true, 0);
        card.pack_start(&row2, false, false, 0);

        grid.attach(&hover, (i % 2) as i32, (i / 2) as i32, 1, 1);
        cards.push(card);
        names.push(name);
        glyphs.push(glyph);
        icons.push(icon);
        selects.push(select);
        tests.push(test);
    }
    wrap.pack_start(&grid, false, false, 0);

    // Custom commands, shown while a corner has one.
    let custom = panel("Custom commands");
    custom.set_margin_bottom(12);
    let mut custom_rows = vec![];
    for (i, corner_name) in CORNER_NAMES.iter().enumerate() {
        let r = gtk::Box::new(gtk::Orientation::Vertical, 7);
        classed(&r, &["row"]);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = label(corner_name, &["nm"]);
        head.pack_start(&name, true, true, 0);
        let test = gtk::Button::with_label("Test");
        classed(&test, &["mini"]);
        test.connect_clicked(move |_| {
            child::send(&serde_json::json!({ "cmd": "test", "corner": i }));
        });
        head.pack_end(&test, false, false, 0);
        r.pack_start(&head, false, false, 0);
        let grid = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let command = gtk::Entry::new();
        command.set_placeholder_text(Some("Program, script, document, or URL"));
        command.set_hexpand(true);
        let args = gtk::Entry::new();
        args.set_placeholder_text(Some("Arguments"));
        args.set_width_chars(14);
        let on_edit = move |_: &gtk::Entry| {
            let (command, args) = UI.with(|ui| {
                ui.borrow()
                    .as_ref()
                    .map(|ui| {
                        let (_, c, a) = &ui.custom_rows[i];
                        (c.text().to_string(), a.text().to_string())
                    })
                    .unwrap_or_default()
            });
            edit(|c| c.corners[i] = Action::Custom { command, args });
        };
        command.connect_changed(on_edit);
        args.connect_changed(on_edit);
        grid.pack_start(&command, true, true, 0);
        grid.pack_start(&args, false, false, 0);
        r.pack_start(&grid, false, false, 0);
        custom.pack_start(&r, false, false, 0);
        custom_rows.push((r, command, args));
    }
    wrap.pack_start(&custom, false, false, 0);

    // Trigger.
    let trigger = panel("Trigger");
    trigger.set_margin_bottom(12);
    let (dwell_box, dwell, dwell_out) = slider(0.0, 1200.0, 25.0);
    dwell.connect_value_changed(|s| edit(|c| c.dwell_ms = s.value() as u32));
    trigger.pack_start(
        &row(
            "Dwell",
            "How long the cursor must rest before a corner fires",
            &dwell_box,
            true,
        ),
        false,
        false,
        0,
    );
    let (size_box, size, size_out) = slider(1.0, 40.0, 1.0);
    size.connect_value_changed(|s| edit(|c| c.corner_size = s.value() as i32));
    trigger.pack_start(
        &row(
            "Corner size",
            "Edge length of the sensitive square",
            &size_box,
            false,
        ),
        false,
        false,
        0,
    );
    let (cool_box, cool, cool_out) = slider(200.0, 3000.0, 100.0);
    cool.connect_value_changed(|s| edit(|c| c.cooldown_ms = s.value() as u32));
    trigger.pack_start(
        &row(
            "Cooldown",
            "Minimum quiet time after a corner fires",
            &cool_box,
            false,
        ),
        false,
        false,
        0,
    );
    let modifier = gtk::ComboBoxText::new();
    for (id, text) in MODIFIERS {
        modifier.append(Some(id), text);
    }
    modifier.connect_changed(|m| {
        let Some(id) = m.active_id() else { return };
        if let Ok(v) = serde_json::from_value::<Modifier>(serde_json::json!(id.as_str())) {
            edit(|c| c.modifier = v);
        }
    });
    trigger.pack_start(
        &row(
            "Hold a key",
            "Require a modifier, so corners never fire by accident",
            &modifier,
            false,
        ),
        false,
        false,
        0,
    );
    wrap.pack_start(&trigger, false, false, 0);

    // Scope.
    let scope = panel("Scope");
    scope.set_margin_bottom(12);
    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    classed(&seg, &["seg"]);
    let mut mode = vec![];
    for (m, text) in [
        (MonitorMode::OuterCorners, "Outer"),
        (MonitorMode::EveryMonitor, "Every screen"),
        (MonitorMode::PrimaryOnly, "Primary"),
    ] {
        let b = gtk::ToggleButton::with_label(text);
        b.connect_toggled(move |b| {
            if b.is_active() {
                edit(|c| c.monitor_mode = m);
            }
        });
        seg.pack_start(&b, false, false, 0);
        mode.push((m, b));
    }
    let (mode_row, mode_hint) = row_with_hint("Active corners", "", &seg, true);
    scope.pack_start(&mode_row, false, false, 0);
    let fullscreen = gtk::Switch::new();
    fullscreen.connect_state_set(|_, on| {
        edit(|c| c.suppress_fullscreen = on);
        glib::Propagation::Proceed
    });
    scope.pack_start(
        &row(
            "Pause in fullscreen",
            "Never fire over a fullscreen game or video",
            &fullscreen,
            false,
        ),
        false,
        false,
        0,
    );
    let dragging = gtk::Switch::new();
    dragging.connect_state_set(|_, on| {
        edit(|c| c.suppress_while_dragging = on);
        glib::Propagation::Proceed
    });
    scope.pack_start(
        &row(
            "Pause while dragging",
            "Dragging into a corner never fires it",
            &dragging,
            false,
        ),
        false,
        false,
        0,
    );
    let autostart = gtk::Switch::new();
    // Run-at-login lives with the OS, so it goes to the tray, not the config.
    autostart.connect_state_set(|_, on| {
        if !SYNCING.with(|s| s.get()) {
            child::send(&serde_json::json!({ "cmd": "autostart", "value": on }));
        }
        glib::Propagation::Proceed
    });
    scope.pack_start(
        &row(
            "Start on login",
            "Launch automatically when you sign in",
            &autostart,
            false,
        ),
        false,
        false,
        0,
    );
    wrap.pack_start(&scope, false, false, 0);

    // Footer.
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    classed(&footer, &["footer"]);
    footer.set_margin_top(4);
    let version = label("", &["version"]);
    let toast = label("", &["toast"]);
    footer.pack_start(&version, false, false, 0);
    footer.pack_start(&toast, true, true, 0);
    for (text, cmd) in [
        ("Reset", "reset"),
        ("Config folder", "openConfigDir"),
        ("Log", "openLog"),
    ] {
        let b = gtk::Button::with_label(text);
        classed(&b, &["link"]);
        b.connect_clicked(move |_| child::send(&serde_json::json!({ "cmd": cmd })));
        footer.pack_end(&b, false, false, 0);
    }
    wrap.pack_start(&footer, false, false, 0);

    window.connect_delete_event(|_, _| {
        gtk::main_quit();
        glib::Propagation::Proceed
    });

    Ui {
        window,
        enabled,
        diagram,
        dot,
        status,
        cards,
        names,
        glyphs,
        icons,
        selects,
        tests,
        custom,
        custom_rows,
        dwell: (dwell, dwell_out),
        size: (size, size_out),
        cool: (cool, cool_out),
        modifier,
        mode,
        mode_hint,
        fullscreen,
        dragging,
        autostart,
        version,
        toast,
    }
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

fn set_hovered(corner: Option<usize>) {
    HOVERED.with(|h| h.set(corner));
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            for (i, card) in ui.cards.iter().enumerate() {
                let ctx = card.style_context();
                if corner == Some(i) {
                    ctx.add_class("hover");
                } else {
                    ctx.remove_class("hover");
                }
            }
            ui.diagram.queue_draw();
        }
    });
}

/// Bring every control in line with `STATE`.
fn refresh() {
    let state = STATE.with(|s| s.borrow().clone());
    let Some(c) = state.config.clone() else {
        return;
    };
    SYNCING.with(|s| s.set(true));
    UI.with(|ui| {
        let ui = ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        ui.enabled.set_active(c.enabled);
        let assigned = c.corners.iter().filter(|a| !a.is_none()).count();
        for i in 0..4 {
            let kind = kind_of(&c.corners[i]);
            let on = kind != "none";
            ui.selects[i].set_active_id(Some(&kind));
            ui.icons[i].set_from_icon_name(Some(icon_for(&kind)), gtk::IconSize::Button);
            for (w, cls) in [
                (ui.cards[i].upcast_ref::<gtk::Widget>(), "on"),
                (ui.names[i].upcast_ref(), "on"),
                (ui.glyphs[i].upcast_ref(), "on"),
                (ui.icons[i].upcast_ref(), "on"),
            ] {
                if on {
                    w.style_context().add_class(cls);
                } else {
                    w.style_context().remove_class(cls);
                }
            }
            ui.tests[i].set_sensitive(on);
            let (row, command, args) = &ui.custom_rows[i];
            if let Action::Custom {
                command: cmd,
                args: a,
            } = &c.corners[i]
            {
                row.set_visible(true);
                // Never under the user's caret; the echo of a save is what
                // they just typed.
                if !command.has_focus() && command.text() != cmd.as_str() {
                    command.set_text(cmd);
                }
                if !args.has_focus() && args.text() != a.as_str() {
                    args.set_text(a);
                }
            } else {
                row.set_visible(false);
            }
        }
        let custom: Vec<usize> = (0..4)
            .filter(|&i| matches!(c.corners[i], Action::Custom { .. }))
            .collect();
        ui.custom.set_visible(!custom.is_empty());
        // The first visible row has no top rule, as `h2 + .row` on the page.
        for (n, &i) in custom.iter().enumerate() {
            let ctx = ui.custom_rows[i].0.style_context();
            if n == 0 {
                ctx.add_class("first");
            } else {
                ctx.remove_class("first");
            }
        }

        ui.dwell.0.set_value(c.dwell_ms as f64);
        ui.dwell.1.set_text(&if c.dwell_ms == 0 {
            "instant".to_string()
        } else {
            format!("{} ms", c.dwell_ms)
        });
        ui.size.0.set_value(c.corner_size as f64);
        ui.size.1.set_text(&format!("{} px", c.corner_size));
        ui.cool.0.set_value(c.cooldown_ms as f64);
        ui.cool
            .1
            .set_text(&format!("{:.1} s", c.cooldown_ms as f64 / 1000.0));
        let modifier = serde_json::to_value(c.modifier)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "none".into());
        ui.modifier.set_active_id(Some(&modifier));
        for (m, b) in &ui.mode {
            b.set_active(*m == c.monitor_mode);
        }
        let n = state.displays.len().max(1);
        ui.mode_hint.set_text(&if n == 1 {
            "The four corners of your display".to_string()
        } else {
            match c.monitor_mode {
                MonitorMode::OuterCorners => {
                    "Only the four outer corners of the whole desktop".into()
                }
                MonitorMode::EveryMonitor => {
                    format!("All four corners of each of your {n} displays")
                }
                MonitorMode::PrimaryOnly => {
                    "Only the primary display; the others stay inert".into()
                }
            }
        });
        ui.fullscreen.set_active(c.suppress_fullscreen);
        ui.dragging.set_active(c.suppress_while_dragging);
        ui.autostart.set_active(state.autostart);
        ui.version.set_text(&format!("Cantos {}", state.version));

        let status = if !c.enabled {
            "Hot corners are paused".to_string()
        } else if assigned == 0 {
            "No corners assigned yet. Try a preset".to_string()
        } else {
            let spots = state.hitboxes.len();
            let dwell = if c.dwell_ms == 0 {
                "fires instantly".to_string()
            } else {
                format!("after {} ms", c.dwell_ms)
            };
            format!(
                "{assigned} {} assigned, {spots} hot {} on screen, {dwell}",
                if assigned == 1 { "corner" } else { "corners" },
                if spots == 1 { "spot" } else { "spots" }
            )
        };
        ui.status.set_text(&status);
        if c.enabled && assigned > 0 {
            ui.dot.style_context().add_class("on");
        } else {
            ui.dot.style_context().remove_class("on");
        }
        ui.diagram
            .set_size_request(-1, diagram_height(&state.displays) as i32);
        ui.diagram.queue_draw();
    });
    SYNCING.with(|s| s.set(false));
}

fn show_toast(message: &str, bad: bool) {
    let generation = TOAST_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.toast.set_text(message);
            if bad {
                ui.toast.style_context().add_class("bad");
            } else {
                ui.toast.style_context().remove_class("bad");
            }
        }
    });
    glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        if TOAST_GEN.with(|g| g.get()) == generation {
            UI.with(|ui| {
                if let Some(ui) = ui.borrow().as_ref() {
                    ui.toast.set_text("");
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// The page's inline mark: a dark tile with the lit quarter-disc, 64 units.
fn draw_mark(cr: &gtk::cairo::Context, size: f64) {
    let s = size / 64.0;
    rounded_path(cr, 0.0, 0.0, size, size, 15.0 * s);
    cr.save().ok();
    cr.clip();
    cr.set_source_rgb(
        0x12 as f64 / 255.0,
        0x14 as f64 / 255.0,
        0x1f as f64 / 255.0,
    );
    cr.paint().ok();
    cr.move_to(0.0, 0.0);
    cr.arc(0.0, 0.0, 40.0 * s, 0.0, std::f64::consts::FRAC_PI_2);
    cr.close_path();
    let (r, g, b) = rgb("#6ea8fe");
    cr.set_source_rgb(r, g, b);
    cr.fill().ok();
    cr.restore().ok();
}

fn rounded_path(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn diagram_height(displays: &[DisplayView]) -> f64 {
    let (w, h) = extent(displays);
    if w <= 0.0 {
        return 190.0;
    }
    // The page's rule, at this window's width of the diagram.
    (604.0 * h / w).round().clamp(140.0, 268.0)
}

fn extent(displays: &[DisplayView]) -> (f64, f64) {
    let l = displays.iter().map(|d| d.left).min().unwrap_or(0);
    let t = displays.iter().map(|d| d.top).min().unwrap_or(0);
    let r = displays.iter().map(|d| d.right).max().unwrap_or(0);
    let b = displays.iter().map(|d| d.bottom).max().unwrap_or(0);
    ((r - l) as f64, (b - t) as f64)
}

fn draw_diagram(cr: &gtk::cairo::Context, width: f64, height: f64) {
    let t = tokens();
    let state = STATE.with(|s| s.borrow().clone());
    let displays = &state.displays;
    if displays.is_empty() {
        return;
    }
    let l = displays.iter().map(|d| d.left).min().unwrap_or(0) as f64;
    let top = displays.iter().map(|d| d.top).min().unwrap_or(0) as f64;
    let (w, h) = extent(displays);
    let pad = 24.0;
    let scale = ((width - 2.0 * pad) / w).min((height - 2.0 * pad) / h);
    let ox = (width - w * scale) / 2.0;
    let oy = (height - h * scale) / 2.0;
    let map = |x: f64, y: f64| (ox + (x - l) * scale, oy + (y - top) * scale);

    for d in displays {
        let (x0, y0) = map(d.left as f64, d.top as f64);
        let (x1, y1) = map(d.right as f64, d.bottom as f64);
        rounded_path(cr, x0, y0, x1 - x0, y1 - y0, 8.0);
        let (r, g, b) = if d.primary {
            t.primary_screen
        } else {
            t.secondary_screen
        };
        cr.set_source_rgb(r, g, b);
        cr.fill_preserve().ok();
        let (r, g, b) = rgb(t.line);
        cr.set_source_rgb(r, g, b);
        cr.set_line_width(1.25);
        cr.stroke().ok();
        if d.primary {
            // A hint of the taskbar, so a lone display reads as a screen.
            let bw = (x1 - x0) * 0.22;
            let (r, g, b) = rgb(t.faint);
            cr.set_source_rgba(r, g, b, 0.5);
            rounded_path(cr, (x0 + x1) / 2.0 - bw / 2.0, y1 - 14.0, bw, 4.0, 2.0);
            cr.fill().ok();
        }
    }

    let corners = config().corners;
    let hovered = HOVERED.with(|h| h.get());
    let (r_, b_) = (l + w, top + h);
    for (i, (x, y)) in [(l, top), (r_, top), (l, b_), (r_, b_)]
        .into_iter()
        .enumerate()
    {
        if !state.hitboxes.iter().any(|hb| hb.corner == CORNER_KEYS[i]) {
            continue;
        }
        let (px, py) = map(x, y);
        let (dx, dy) = (
            if i % 2 == 0 { 1.0 } else { -1.0 },
            if i < 2 { 1.0 } else { -1.0 },
        );
        let colour = if hovered == Some(i) {
            t.accent
        } else if !corners[i].is_none() {
            t.accent_dim
        } else {
            t.line
        };
        let (r, g, b) = rgb(colour);
        cr.set_source_rgb(r, g, b);
        cr.set_line_width(2.0);
        cr.set_line_cap(gtk::cairo::LineCap::Square);
        cr.move_to(px, py + dy * 15.0);
        cr.line_to(px, py);
        cr.line_to(px + dx * 15.0, py);
        cr.stroke().ok();
    }
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
        "raise" => UI.with(|ui| {
            if let Some(ui) = ui.borrow().as_ref() {
                ui.window.present();
            }
        }),
        other => lerror!("settings window: unknown push {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

fn start() -> Result<(), String> {
    gtk::init().map_err(|e| format!("could not start GTK: {e}"))?;
    let dark = prefers_dark();
    DARK_MODE.with(|d| d.set(dark));
    let provider = gtk::CssProvider::new();
    provider
        .load_from_data(css(tokens()).as_bytes())
        .map_err(|e| format!("bad stylesheet: {e}"))?;
    let screen = gtk::gdk::Screen::default().ok_or("no screen")?;
    gtk::StyleContext::add_provider_for_screen(
        &screen,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let ui = build();
    UI.with(|u| *u.borrow_mut() = Some(ui));
    Ok(())
}

/// `cantos --settings-window`: run the window until it is closed.
pub fn run() {
    linfo!(
        "settings window process started (pid {})",
        std::process::id()
    );
    if let Err(e) = start() {
        lerror!("settings window: {e}");
        return;
    }
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.window.show_all();
            ui.custom.set_visible(false);
        }
    });

    // The tray's calls arrive on stdin and are handed to GTK's main loop. End
    // of input means the tray has gone, so the window goes too.
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<child::Push>(&line) {
                Ok(push) => {
                    glib::idle_add_once(move || apply(push));
                }
                Err(e) => lerror!("settings window: unreadable line from the tray ({e})"),
            }
        }
        glib::idle_add_once(gtk::main_quit);
    });

    child::send(&serde_json::json!({ "cmd": "ready" }));
    gtk::main();
}
