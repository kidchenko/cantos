//! The tray application: the icon, its menu, and the settings window.
//!
//! Everything here runs on the main thread, driven by the event loop in
//! `main`. [`App`] owns the mutable state that outlives a single event (the
//! tray handle and the settings window), so each event handler is a short
//! method rather than another branch of one long closure.

use crate::config::Config;
use crate::config::SharedConfig;
use crate::log::{lerror, linfo};
use crate::ui::page::{self, Msg};
use crate::ui::{self, Settings};
use crate::{actions, autostart, log, util};

use tao::event::Event;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon as TrayImage, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(not(target_os = "macos"))]
const ICON_EDGE: u32 = 64;

/// Everything that can wake the event loop.
pub enum UserEvent {
    OpenSettings,
    OpenLog,
    CheckForUpdates,
    ToggleEnabled,
    Quit,
    /// `config.json` was edited outside the app and has been picked up.
    ConfigReloaded,
    /// The native settings window's process ended. Carries the generation it
    /// was opened as, so a window closing late cannot clear a newer one.
    SettingsClosed(u64),
    /// A JSON message from the settings page.
    Ipc(String),
}

/// Whether the event loop should keep going.
#[derive(PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

pub struct App {
    cfg: SharedConfig,
    proxy: EventLoopProxy<UserEvent>,
    tray: TrayIcon,
    /// The "Hot corners enabled" checkbox, kept in step with the config.
    enabled_item: CheckMenuItem,
    /// `None` whenever the window is closed, and that is the point: the
    /// window is its own process, and closing it returns the app to a few
    /// megabytes resident.
    settings: Option<Settings>,
    /// Counts windows opened; see `UserEvent::SettingsClosed`.
    settings_gen: u64,
}

impl App {
    pub fn new(cfg: SharedConfig, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let enabled = cfg.lock().map(|c| c.enabled).unwrap_or(true);

        let enabled_item = CheckMenuItem::new("Hot corners enabled", true, enabled, None);
        let settings_item = MenuItem::new("Settings\u{2026}", true, None);
        let log_item = MenuItem::new("Open log", true, None);
        let update_item = MenuItem::new("Check for updates\u{2026}", true, None);
        let quit_item = MenuItem::new("Quit Cantos", true, None);

        let menu = Menu::new();
        // No pause on a Mac: the Dock fires the corners, and its settings are
        // what Cantos edits there.
        #[cfg(not(target_os = "macos"))]
        let _ = menu.append(&enabled_item);
        let _ = menu.append_items(&[
            &settings_item,
            &PredefinedMenuItem::separator(),
            &log_item,
            &update_item,
            &quit_item,
        ]);

        // Captured by id, because the menu items themselves are moved into the
        // menu and the handler runs on another callback entirely.
        forward_menu_events(
            &proxy,
            MenuIds {
                enabled: enabled_item.id().clone(),
                settings: settings_item.id().clone(),
                log: log_item.id().clone(),
                update: update_item.id().clone(),
                quit: quit_item.id().clone(),
            },
        );
        forward_tray_events(&proxy);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip(enabled))
            .with_icon(tray_image(enabled)?)
            // On a Mac the menu bar icon's job is its menu, as with every
            // other one up there, and the glyph is a template that AppKit
            // tints. Elsewhere right-click opens the menu and left-click is
            // the primary action, which for this app means "show me the
            // settings". (Linux's AppIndicator always shows the menu.)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()
            .map_err(|e| format!("could not create the tray icon: {e}"))?;

        Ok(Self {
            cfg,
            proxy,
            tray,
            enabled_item,
            settings: None,
            settings_gen: 0,
        })
    }

    pub fn handle(
        &mut self,
        event: Event<UserEvent>,
        _target: &EventLoopWindowTarget<UserEvent>,
    ) -> Flow {
        match event {
            Event::UserEvent(UserEvent::Quit) => {
                linfo!("quit requested from the tray");
                self.settings = None;
                return Flow::Exit;
            }
            Event::UserEvent(UserEvent::OpenLog) => log::reveal(),
            Event::UserEvent(UserEvent::CheckForUpdates) => check_for_updates(),
            Event::UserEvent(UserEvent::ToggleEnabled) => self.toggle_enabled(),
            Event::UserEvent(UserEvent::OpenSettings) => self.open_settings(),
            Event::UserEvent(UserEvent::SettingsClosed(generation)) => {
                if generation == self.settings_gen {
                    linfo!("native settings window closed");
                    self.settings = None;
                }
            }
            Event::UserEvent(UserEvent::ConfigReloaded) => self.sync_to_config(),
            Event::UserEvent(UserEvent::Ipc(body)) => self.handle_ipc(&body),
            // macOS: opening Cantos.app again (Finder, Spotlight, `open`)
            // does not start a second process, so the single-instance socket
            // never hears about it. LaunchServices tells the running one
            // instead; treat it like any other relaunch.
            Event::Reopen { .. } => {
                linfo!("reopened by LaunchServices; opening settings");
                self.open_settings();
            }
            _ => {}
        }
        Flow::Continue
    }

    fn toggle_enabled(&mut self) {
        let now = {
            let Ok(mut c) = self.cfg.lock() else { return };
            c.enabled = !c.enabled;
            let _ = c.save();
            c.enabled
        };
        self.show_enabled(now);
        self.refresh_page();
    }

    fn open_settings(&mut self) {
        // Already open: bring it forward rather than opening a second one.
        if let Some(s) = &self.settings {
            linfo!("settings already open; raising it");
            s.raise();
            return;
        }

        self.settings_gen += 1;
        match self.spawn_settings() {
            Ok(s) => {
                linfo!("settings window opened");
                self.settings = Some(s);
            }
            // Say so in a dialog as well as the log: this is the app's only
            // window, so a log-only failure just looks like being ignored.
            Err(e) => {
                lerror!("could not open settings window: {e}");
                ui::report_open_failure(&e);
            }
        }
    }

    /// Start the settings window's process; see `ui::child`.
    fn spawn_settings(&self) -> Result<Settings, String> {
        let (messages, closed) = (self.proxy.clone(), self.proxy.clone());
        let generation = self.settings_gen;
        ui::child::Child::spawn(
            move |line| {
                let _ = messages.send_event(UserEvent::Ipc(line));
            },
            move || {
                let _ = closed.send_event(UserEvent::SettingsClosed(generation));
            },
        )
        .map(Settings)
    }

    fn handle_ipc(&mut self, body: &str) {
        let Some(msg) = Msg::parse(body) else {
            lerror!("unparseable IPC message: {body}");
            return;
        };
        // Every message originates from the page, so the window is open --
        // but it can close between the post and the event being drained.
        //
        // Deliberately a presence check and not `let Some(s) = &self.settings`:
        // holding that borrow across the match would collide with the arms
        // that need `&mut self`. Arms that talk to the page take the borrow
        // themselves, right where they use it.
        if self.settings.is_none() {
            return;
        }

        match msg {
            Msg::Ready => {
                self.refresh_page();
                #[cfg(target_os = "linux")]
                if let (Some(s), Some(why)) = (&self.settings, crate::x11::session_warning()) {
                    page::toast(s, why, true);
                }
            }
            Msg::Set { config } => self.save_from_page(*config),
            Msg::Test { corner } => self.test_corner(corner),
            Msg::Reset => self.reset_to_defaults(),
            Msg::OpenLog => log::reveal(),
            Msg::OpenConfigDir => {
                if let Some(d) = Config::dir() {
                    util::shell_open(&d.to_string_lossy());
                }
            }
            Msg::Autostart { value } => self.set_autostart(value),
            #[cfg(target_os = "macos")]
            Msg::Native { corners } => self.set_native(*corners),
            #[cfg(target_os = "macos")]
            Msg::MonitorMode { mode } => self.set_monitor_mode(mode),
        }
    }

    /// The watcher reads the mode every tick, so saving it is all it takes.
    #[cfg(target_os = "macos")]
    fn set_monitor_mode(&mut self, mode: crate::config::MonitorMode) {
        let saved = match self.cfg.lock() {
            Ok(mut c) => {
                c.monitor_mode = mode;
                c.save().map_err(|e| format!("Could not save: {e}"))
            }
            Err(_) => Err("settings are unavailable".into()),
        };
        let Some(s) = &self.settings else { return };
        page::push_state(s, &self.cfg);
        match saved {
            Ok(()) => page::toast(s, "Saved", false),
            Err(e) => page::toast(s, &e, true),
        }
    }

    /// Write the corners to the Dock, then show the page what the Dock now
    /// has, so a failed write cannot leave the page claiming otherwise.
    #[cfg(target_os = "macos")]
    fn set_native(&mut self, corners: [crate::native::NativeCorner; 4]) {
        let result = match self.cfg.lock() {
            Ok(mut c) => {
                let r = crate::native::apply(&mut c, &corners);
                let saved = c.save().map_err(|e| format!("could not save: {e}"));
                r.and(saved)
            }
            Err(_) => Err("settings are unavailable".into()),
        };
        // Asked now, while the user is setting the corner, rather than the
        // first time it fires.
        let untrusted = corners
            .iter()
            .any(|c| actions::needs_accessibility(&c.action))
            && !actions::request_accessibility();
        let Some(s) = &self.settings else { return };
        page::push_state(s, &self.cfg);
        match result {
            Ok(()) if untrusted => page::toast(
                s,
                "Saved. Allow Cantos in Privacy & Security > Accessibility to use it",
                true,
            ),
            Ok(()) => page::toast(s, "Saved", false),
            Err(e) => page::toast(s, &e, true),
        }
    }

    /// The OS is the only record of this; see the note on [`Config`]. On
    /// failure we report back what the OS actually says rather than what was
    /// asked for, so the toggle cannot lie.
    fn set_autostart(&self, value: bool) {
        let Some(s) = &self.settings else { return };
        match autostart::set(value) {
            Ok(()) => page::push_autostart(s, value, None),
            Err(e) => page::push_autostart(s, autostart::is_enabled(), Some(&e)),
        }
    }

    fn save_from_page(&mut self, config: Config) {
        let sanitised = config.sanitised();
        let enabled = sanitised.enabled;
        let saved = {
            let Ok(mut guard) = self.cfg.lock() else {
                return;
            };
            *guard = sanitised;
            guard.save()
        };
        // Tray state can drift from the page, so mirror it on every save.
        self.show_enabled(enabled);

        let Some(s) = &self.settings else { return };
        page::push_geometry(s, &self.cfg);
        match saved {
            Ok(()) => page::toast(s, "Saved", false),
            Err(e) => page::toast(s, &format!("Could not save: {e}"), true),
        }
    }

    fn test_corner(&self, corner: usize) {
        // On a Mac it is the Dock's action, or Cantos's own where the Dock has
        // none.
        #[cfg(target_os = "macos")]
        let action = self.cfg.lock().ok().and_then(|c| {
            crate::native::effective(&c)
                .get(corner)
                .map(|c| c.action.clone())
        });
        #[cfg(not(target_os = "macos"))]
        let action = self
            .cfg
            .lock()
            .ok()
            .and_then(|c| c.corners.get(corner).cloned());
        if let Some(action) = action {
            actions::fire(action);
        }
    }

    fn reset_to_defaults(&mut self) {
        linfo!("settings reset to defaults");
        {
            let Ok(mut guard) = self.cfg.lock() else {
                return;
            };
            *guard = Config::default();
            let _ = guard.save();
        }
        self.show_enabled(Config::default().enabled);
        if let Some(s) = &self.settings {
            page::push_state(s, &self.cfg);
            page::toast(s, "Restored defaults", false);
        }
    }

    /// Bring the tray and any open page back in line with the config, after
    /// something outside this app changed it.
    fn sync_to_config(&mut self) {
        #[cfg(target_os = "macos")]
        if let Ok(mut c) = self.cfg.lock() {
            if crate::native::reconcile(&mut c) {
                let _ = c.save();
            }
        }
        let enabled = self.cfg.lock().map(|c| c.enabled).unwrap_or(true);
        self.show_enabled(enabled);
        self.refresh_page();
    }

    /// The tray's two representations of the on/off state, updated together
    /// so they cannot disagree.
    fn show_enabled(&self, on: bool) {
        self.enabled_item.set_checked(on);
        let _ = self.tray.set_tooltip(Some(tooltip(on)));
    }

    fn refresh_page(&self) {
        if let Some(s) = &self.settings {
            page::push_state(s, &self.cfg);
        }
    }
}

fn tooltip(enabled: bool) -> &'static str {
    if enabled {
        "Cantos: active"
    } else {
        "Cantos: paused"
    }
}

/// The template glyph on macOS, always lit since there is no pause there; the
/// app's coloured mark elsewhere.
fn tray_image(enabled: bool) -> Result<TrayImage, String> {
    #[cfg(target_os = "macos")]
    let (rgba, edge) = {
        let _ = enabled;
        (crate::icon::template(true), crate::icon::EDGE)
    };
    #[cfg(not(target_os = "macos"))]
    let (rgba, edge) = {
        let _ = enabled;
        (ui::icon_rgba(), ICON_EDGE)
    };
    TrayImage::from_rgba(rgba, edge, edge).map_err(|e| format!("tray icon is malformed: {e}"))
}

/// Cantos never touches the network itself. The project page compares the
/// version it is handed with the latest GitHub release and says what to do.
fn check_for_updates() {
    util::shell_open(concat!(
        "https://josebarbosa.dev/cantos/?v=",
        env!("CARGO_PKG_VERSION")
    ));
}

struct MenuIds {
    enabled: MenuId,
    settings: MenuId,
    log: MenuId,
    update: MenuId,
    quit: MenuId,
}

fn forward_menu_events(proxy: &EventLoopProxy<UserEvent>, ids: MenuIds) {
    let proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let event = if e.id == ids.enabled {
            UserEvent::ToggleEnabled
        } else if e.id == ids.settings {
            UserEvent::OpenSettings
        } else if e.id == ids.log {
            UserEvent::OpenLog
        } else if e.id == ids.update {
            UserEvent::CheckForUpdates
        } else if e.id == ids.quit {
            UserEvent::Quit
        } else {
            return;
        };
        let _ = proxy.send_event(event);
    }));
}

fn forward_tray_events(proxy: &EventLoopProxy<UserEvent>) {
    let proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            ..
        } = e
        {
            let _ = proxy.send_event(UserEvent::OpenSettings);
        }
    }));
}
