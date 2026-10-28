//! The polling thread: samples the cursor, feeds [`crate::trigger`], and runs
//! whatever it says to run.
//!
//! The decision logic lives in `trigger`; the OS questions live in `desktop`.
//! What is left here is the loop and its three timers.
//!
//! # Why polling rather than a mouse hook
//!
//! The obvious alternative on Windows is a `WH_MOUSE_LL` low-level hook. We
//! deliberately do not use one:
//!
//!   * A low-level hook runs *inline* on the input thread for every mouse
//!     move system-wide. Any latency we add is latency the whole desktop
//!     feels, and if our callback overruns `LowLevelHooksTimeout` Windows
//!     silently unhooks us, and the app appears to just stop working.
//!   * Hooks need a pumped message loop on the installing thread, coupling
//!     detection to UI responsiveness.
//!
//! `GetCursorPos` at ~33 Hz costs a few hundred nanoseconds per tick, is
//! immune to both problems, and a hot corner does not need sub-30 ms
//! resolution when it is gated behind a 250 ms dwell anyway.
//!
//! The same holds elsewhere. On macOS an event tap would need the Input
//! Monitoring permission just to see the pointer move, where reading its
//! position needs none; on X11, `XQueryPointer` is one round trip.

use crate::actions;
use crate::config::{Action, Config, MonitorMode, SharedConfig};
use crate::desktop::{self, System};
use crate::geometry::{self, contains, Hitbox};
use crate::trigger::{Tick, Trigger, Tunables};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use traykit::linfo;

const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// How often to re-enumerate displays. Cheap enough to do on a timer rather
/// than plumbing display-change notifications through to this thread, and it
/// picks up resolution and DPI changes for free.
const LAYOUT_REFRESH: Duration = Duration::from_secs(1);

/// How often to stat `config.json`. Only a `metadata` call unless the
/// timestamp actually moved, so the file is parsed on change, not on a timer.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// Owns the polling thread. Dropping it stops the thread and joins it.
pub struct Watcher {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// `on_external_change` fires when `config.json` was edited by something
    /// other than us and the running config has been replaced from it, so the
    /// tray and the settings window can catch up.
    pub fn start<F>(cfg: SharedConfig, on_external_change: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let handle = std::thread::Builder::new()
            .name("cantos-watch".into())
            .spawn(move || run(cfg, flag, on_external_change))
            .expect("failed to spawn watcher thread");
        Self {
            running,
            handle: Some(handle),
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run<F: Fn()>(cfg: SharedConfig, running: Arc<AtomicBool>, on_external_change: F) {
    // SHQueryUserNotificationState is a shell call; give the thread an STA so
    // it is always invoked from an initialised apartment.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let desktop = System;
    let mut trigger = Trigger::new();
    let mut layout = Layout::default();
    let mut config_file = ConfigFile::new();

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);

        if config_file.due() && config_file.pick_up_external_edit(&cfg) {
            on_external_change();
        }

        let tun = {
            let Ok(guard) = cfg.lock() else { break };
            Tunables::read(&guard)
        };

        // Paused, or nothing to do: no point asking where the cursor is. On a
        // Mac this is the usual case, with every corner left to the Dock.
        if !tun.enabled || !tun.any_assigned {
            trigger.reset();
            continue;
        }

        let boxes = layout.hitboxes(&tun);
        let Some(pos) = desktop::cursor_pos() else {
            continue;
        };
        let at = boxes
            .iter()
            .find(|h| contains(&h.rect, pos))
            .map(|h| h.spot());

        if let Tick::Fire(corner) = trigger.update(at, &tun, &desktop) {
            let action = corner_action(&cfg, corner);
            linfo!("{corner:?} fired: {action:?}");
            actions::fire(action);
        }
    }

    #[cfg(windows)]
    unsafe {
        windows::Win32::System::Com::CoUninitialize()
    };
}

fn corner_action(cfg: &SharedConfig, corner: crate::config::Corner) -> Action {
    cfg.lock()
        .ok()
        .map(|c| c.corners[corner.index()].clone())
        .unwrap_or(Action::None)
}

/// The live hitboxes, rebuilt on a timer or whenever the settings that shape
/// them change.
struct Layout {
    boxes: Vec<Hitbox>,
    built: Instant,
    /// The (size, mode) the current boxes were built for.
    shape: Option<(i32, MonitorMode)>,
    /// Last summary written to the log, so an unchanged layout stays quiet.
    logged: String,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            boxes: Vec::new(),
            // In the past, so the first tick always builds.
            built: Instant::now() - LAYOUT_REFRESH,
            shape: None,
            logged: String::new(),
        }
    }
}

impl Layout {
    fn hitboxes(&mut self, tun: &Tunables) -> &[Hitbox] {
        let wanted = (tun.corner_size, tun.mode);
        let stale = self.built.elapsed() >= LAYOUT_REFRESH || self.shape != Some(wanted);
        if stale {
            let monitors = geometry::monitors();
            self.boxes = geometry::hitboxes(&monitors, tun.mode, tun.corner_size);
            self.built = Instant::now();
            self.shape = Some(wanted);
            self.log_if_changed(tun.mode, monitors.len());
        }
        &self.boxes
    }

    /// `live corners` is the single most useful line in the log: it lists the
    /// exact rectangles that are hot right now, which answers most "why
    /// didn't my corner fire" questions outright. Logged only when it changes,
    /// because this runs once a second.
    fn log_if_changed(&mut self, mode: MonitorMode, monitors: usize) {
        let rects: Vec<String> = self
            .boxes
            .iter()
            .map(|h| {
                format!(
                    "{:?}[{},{}..{},{}]",
                    h.corner, h.rect.left, h.rect.top, h.rect.right, h.rect.bottom
                )
            })
            .collect();
        let summary = format!("{mode:?} across {monitors} monitor(s): {}", rects.join(" "));
        if summary != self.logged {
            linfo!("live corners: {summary}");
            self.logged = summary;
        }
    }
}

/// Watches `config.json` for edits made outside the app.
struct ConfigFile {
    checked: Instant,
    /// Timestamp of the file as we last saw it.
    stamp: Option<std::time::SystemTime>,
}

impl ConfigFile {
    fn new() -> Self {
        Self {
            checked: Instant::now(),
            stamp: Config::modified(),
        }
    }

    fn due(&mut self) -> bool {
        if self.checked.elapsed() < CONFIG_POLL {
            return false;
        }
        self.checked = Instant::now();
        true
    }

    /// Adopt a `config.json` that changed underneath us. Returns whether the
    /// running config actually moved.
    ///
    /// The file is parsed only when its timestamp changes, and the result is
    /// compared against what we already hold, so our own saves, which of
    /// course bump the timestamp, cost one parse and report no change. That is
    /// what keeps this from ping-ponging with the settings window.
    fn pick_up_external_edit(&mut self, cfg: &SharedConfig) -> bool {
        let stamp = Config::modified();
        if stamp == self.stamp {
            return false;
        }

        // Absent or unparseable: leave the running config alone rather than
        // resetting someone's settings because an editor was mid-save. The
        // stamp still moves, so a file that stays broken is not re-parsed
        // every second.
        let Some(loaded) = Config::reload() else {
            self.stamp = stamp;
            return false;
        };

        let Ok(mut guard) = cfg.lock() else {
            return false;
        };

        // Every writer updates the shared config and the file under this same
        // lock, so the file cannot move while we hold it. If the timestamp no
        // longer matches the one we stat'd a moment ago, a save landed between
        // that stat and our read, and `loaded` is already superseded. Drop it
        // and leave the stamp alone so the next poll sees the settled file.
        //
        // Without this the watcher could write a just-overtaken config back
        // over a fresh save: the setting would visibly snap back in the
        // settings window, stay wrong for up to a second, then re-apply
        // itself. The window auto-saves on a 140ms debounce, so dragging a
        // slider aims a burst of writes straight at that window.
        if Config::modified() != stamp {
            return false;
        }
        self.stamp = stamp;

        if *guard == loaded {
            return false;
        }
        *guard = loaded;
        linfo!("config.json changed on disk; reloaded");
        true
    }
}
