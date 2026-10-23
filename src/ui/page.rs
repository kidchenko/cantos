//! The settings page protocol: what the page can ask for, and what we push
//! back into it.
//!
//! Three channels, all defined here so the contract is in one file:
//!
//!   * page -> Rust is [`Msg`], a JSON object tagged with `cmd`.
//!   * Rust -> page is the `push_*`/[`toast`] functions, which call
//!     `window.__hc.<name>(...)` in the document, or send the same call to the
//!     native window (see [`super::Settings::push`]).
//!
//! Every argument crosses as a JSON value, never spliced in as text. Paths and
//! OS error messages end up in these calls, and one stray quote would
//! otherwise turn a status message into a syntax error, or worse.

use super::Settings;
use crate::config::SharedConfig;
use crate::config::{Config, MonitorMode};
use crate::{autostart, geometry, log};
use serde::Deserialize;

/// Commands the settings page can send us.
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Msg {
    /// Page has loaded and wants the current state.
    Ready,
    Set {
        config: Box<Config>,
    },
    Test {
        corner: usize,
    },
    Autostart {
        value: bool,
    },
    /// Restore factory settings. Defaults live in Rust, so the page asks
    /// rather than hard-coding its own copy that could drift.
    Reset,
    OpenLog,
    OpenConfigDir,
    /// macOS: the corners as the page now wants them, written to the Dock.
    /// Sent whole, so a preset that changes all four restarts the Dock once.
    #[cfg(target_os = "macos")]
    Native {
        corners: Box<[crate::native::NativeCorner; 4]>,
    },
}

impl Msg {
    pub fn parse(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }
}

/// Everything the page needs to render itself from scratch.
pub fn push_state(s: &Settings, cfg: &SharedConfig) {
    // Take the geometry first: it locks the config, and std mutexes are not
    // reentrant, so doing it while holding `guard` would deadlock.
    let (displays, hitboxes) = geometry_json(cfg);
    let log_path = log::path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let config_dir = Config::dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let Ok(guard) = cfg.lock() else { return };
    // On a Mac most corners are the Dock's, never config.json's, so the page
    // gets the effective set, with a key each (see `native::effective`).
    #[cfg(target_os = "macos")]
    let (config, native) = {
        let native = crate::native::effective(&guard);
        let mut view = guard.clone();
        view.corners = native.clone().map(|c| c.action);
        (view, Some(native))
    };
    #[cfg(not(target_os = "macos"))]
    let (config, native): (Config, Option<()>) = (guard.clone(), None);
    let state = serde_json::json!({
        "config":    config,
        "native":    native,
        "displays":  displays,
        "hitboxes":  hitboxes,
        "version":   env!("CARGO_PKG_VERSION"),
        "autostart": autostart::is_enabled(),
        "logPath":   log_path,
        "configDir": config_dir,
    });
    s.push("state", &[state]);
}

/// Just the diagram. Corner size and monitor mode change which rectangles are
/// live, so it is refreshed from Rust rather than the page guessing.
pub fn push_geometry(s: &Settings, cfg: &SharedConfig) {
    let (displays, hitboxes) = geometry_json(cfg);
    s.push(
        "geometry",
        &[serde_json::json!({"displays": displays, "hitboxes": hitboxes})],
    );
}

/// A transient message in the page footer. `bad` colours it as an error.
pub fn toast(s: &Settings, msg: &str, bad: bool) {
    s.push("toast", &[msg.into(), bad.into()]);
}

/// Reflect the real state of the run-at-login registry entry, with an optional
/// explanation of why it is not what was asked for.
pub fn push_autostart(s: &Settings, on: bool, err: Option<&str>) {
    match err {
        None => s.push("autostart", &[on.into()]),
        Some(e) => s.push("autostart", &[on.into(), e.into()]),
    }
}

/// The real display arrangement and the rectangles that are actually live.
///
/// Computed here, never in the page: a corner dropped for not sitting on a
/// physical display (L-shaped layouts) then shows as absent in the diagram
/// instead of being a corner that silently never fires.
fn geometry_json(cfg: &SharedConfig) -> (serde_json::Value, serde_json::Value) {
    let (mode, size) = cfg
        .lock()
        .map(|c| (c.monitor_mode, c.corner_size))
        .unwrap_or((MonitorMode::OuterCorners, 8));
    // The Dock's corners are the outer corners of the whole arrangement,
    // whatever Cantos's own setting says.
    #[cfg(target_os = "macos")]
    let mode = {
        let _ = mode;
        MonitorMode::OuterCorners
    };
    let monitors = geometry::monitors();
    let boxes = geometry::hitboxes(&monitors, mode, size);
    (
        serde_json::json!(geometry::display_views(&monitors)),
        serde_json::json!(geometry::hitbox_views(&boxes)),
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::Msg;
    use crate::config::{Action, Modifier};

    /// The exact shape the Mac window sends.
    #[test]
    fn the_pages_native_message_parses() {
        let body = r#"{"cmd":"native","corners":[
            {"action":{"kind":"lockWorkstation"},"modifier":"none"},
            {"action":{"kind":"taskView"},"modifier":"none"},
            {"action":{"kind":"none"},"modifier":"none"},
            {"action":{"kind":"quickNote"},"modifier":"shift"}]}"#;
        let Some(Msg::Native { corners }) = Msg::parse(body) else {
            panic!("did not parse as Msg::Native");
        };
        assert_eq!(corners[1].action, Action::TaskView);
        assert_eq!(corners[3].modifier, Modifier::Shift);
    }
}
