//! The tray application: the icon, its menu, and the settings window.
//!
//! Everything here runs on the main thread, driven by the event loop in
//! `main`. [`App`] owns the mutable state that outlives a single event (the
//! tray handle and the settings window), so each event handler is a short
//! method rather than another branch of one long closure.

use crate::config::Config;
use crate::config::{SettingsUi, SharedConfig};
use crate::log::{lerror, linfo};
use crate::ui::page::{self, Msg};
use crate::ui::{self, Settings};
use crate::{actions, autostart, log, util};

use tao::event::{Event, WindowEvent};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon as TrayImage, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(not(target_os = "macos"))]
const ICON_EDGE: u32 = 64;

/// Everything that can wake the event loop.
pub enum UserEvent {
    OpenSettings,
    OpenLog,
    ToggleEnabled,
    Quit,
    /// `config.json` was edited outside the app and has been picked up.
    ConfigReloaded,
    /// The native settings window's process ended. Carries the generation it
    /// was opened as, so a window closing late cannot clear a newer one.
    SettingsClosed(u64),
    /// Switch between the web and the native settings window.
    ToggleSettingsUi,
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
    /// `None` whenever the window is closed, and that is the point. Dropping
    /// it tears down the webview's processes, which is what returns the app to
    /// a few megabytes resident.
    settings: Option<Settings>,
    /// Counts windows opened; see `UserEvent::SettingsClosed`.
    settings_gen: u64,
    /// "New settings window": which window `open_settings` builds.
    native_ui_item: CheckMenuItem,
    /// macOS: the application menu. Never visible (Cantos has no menu bar of
    /// its own), but key equivalents are routed through it, so without it
    /// Cmd+C/V/A/Z do nothing in the settings page's text fields.
    #[cfg(target_os = "macos")]
    _app_menu: Menu,
}

impl App {
    pub fn new(cfg: SharedConfig, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let enabled = cfg.lock().map(|c| c.enabled).unwrap_or(true);

        let enabled_item = CheckMenuItem::new("Hot corners enabled", true, enabled, None);
        let settings_item = MenuItem::new("Settings\u{2026}", true, None);
        let log_item = MenuItem::new("Open log", true, None);
        let quit_item = MenuItem::new("Quit Cantos", true, None);
        let native_ui = cfg
            .lock()
            .map(|c| c.settings_ui == SettingsUi::Native)
            .unwrap_or(false);
        let native_ui_item = CheckMenuItem::new("New settings window", true, native_ui, None);

        let menu = Menu::new();
        // No pause on a Mac: the Dock fires the corners, and its settings are
        // what Cantos edits there.
        #[cfg(not(target_os = "macos"))]
        let _ = menu.append(&enabled_item);
        // Only where a native window exists yet; see `SettingsUi`.
        if SettingsUi::NATIVE_AVAILABLE {
            let _ = menu.append(&native_ui_item);
        }
        let _ = menu.append_items(&[
            &settings_item,
            &PredefinedMenuItem::separator(),
            &log_item,
            &quit_item,
        ]);

        #[cfg(target_os = "macos")]
        let (app_menu, app_quit) = app_menu();

        // Captured by id, because the menu items themselves are moved into the
        // menu and the handler runs on another callback entirely.
        forward_menu_events(
            &proxy,
            MenuIds {
                enabled: enabled_item.id().clone(),
                settings: settings_item.id().clone(),
                native_ui: native_ui_item.id().clone(),
                log: log_item.id().clone(),
                quit: vec![
                    quit_item.id().clone(),
                    #[cfg(target_os = "macos")]
                    app_quit,
                ],
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
            native_ui_item,
            #[cfg(target_os = "macos")]
            _app_menu: app_menu,
        })
    }

    pub fn handle(
        &mut self,
        event: Event<UserEvent>,
        target: &EventLoopWindowTarget<UserEvent>,
    ) -> Flow {
        match event {
            Event::UserEvent(UserEvent::Quit) => {
                linfo!("quit requested from the tray");
                self.settings = None;
                return Flow::Exit;
            }
            Event::UserEvent(UserEvent::OpenLog) => log::reveal(),
            Event::UserEvent(UserEvent::ToggleEnabled) => self.toggle_enabled(),
            Event::UserEvent(UserEvent::OpenSettings) => self.open_settings(target),
            Event::UserEvent(UserEvent::SettingsClosed(generation)) => {
                if generation == self.settings_gen {
                    linfo!("native settings window closed");
                    self.settings = None;
                }
            }
            Event::UserEvent(UserEvent::ToggleSettingsUi) => self.toggle_settings_ui(target),
            Event::UserEvent(UserEvent::ConfigReloaded) => self.sync_to_config(),
            Event::UserEvent(UserEvent::Ipc(body)) => self.handle_ipc(&body),
            // macOS: opening Cantos.app again (Finder, Spotlight, `open`)
            // does not start a second process, so the single-instance socket
            // never hears about it. LaunchServices tells the running one
            // instead; treat it like any other relaunch.
            Event::Reopen { .. } => {
                linfo!("reopened by LaunchServices; opening settings");
                self.open_settings(target);
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                linfo!("settings window closed; tearing down the webview");
                self.settings = None;
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

    fn open_settings(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        // Already open: bring it forward rather than opening a second one.
        if let Some(s) = &self.settings {
            linfo!("settings already open; raising it");
            s.raise();
            return;
        }

        self.settings_gen += 1;
        let opened = if self.native_ui_wanted() {
            self.open_native_settings()
        } else {
            let proxy = self.proxy.clone();
            ui::build(target, &page::platform_script(), move |body| {
                let _ = proxy.send_event(UserEvent::Ipc(body));
            })
            .map(|w| Settings::Web(Box::new(w)))
        };
        match opened {
            Ok(s) => {
                linfo!("settings window opened");
                self.settings = Some(s);
            }
            // The likeliest cause by far is a missing webview runtime
            // (WebView2 on Windows, WebKitGTK on Linux). Say so in a dialog as
            // well as the log: this is the app's only window, so a log-only
            // failure just looks like being ignored.
            Err(e) => {
                lerror!("could not open settings window: {e}");
                ui::report_open_failure(&e);
            }
        }
    }

    fn native_ui_wanted(&self) -> bool {
        SettingsUi::NATIVE_AVAILABLE
            && self
                .cfg
                .lock()
                .map(|c| c.settings_ui == SettingsUi::Native)
                .unwrap_or(false)
    }

    fn open_native_settings(&self) -> Result<Settings, String> {
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
        .map(Settings::Native)
    }

    /// Flip between the two windows, reopening the settings in the other one
    /// if they were open.
    fn toggle_settings_ui(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        let now = {
            let Ok(mut c) = self.cfg.lock() else { return };
            c.settings_ui = match c.settings_ui {
                SettingsUi::Web => SettingsUi::Native,
                SettingsUi::Native => SettingsUi::Web,
            };
            let _ = c.save();
            c.settings_ui
        };
        linfo!("settings window is now {now:?}");
        self.native_ui_item.set_checked(now == SettingsUi::Native);
        if self.settings.take().is_some() {
            self.open_settings(target);
        }
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
            Msg::SettingsUi { native } => {
                if native != self.native_ui_wanted() {
                    let _ = self.proxy.send_event(UserEvent::ToggleSettingsUi);
                }
            }
            #[cfg(target_os = "macos")]
            Msg::Native { corners } => self.set_native(*corners),
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
        let Some(s) = &self.settings else { return };
        page::push_state(s, &self.cfg);
        match result {
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

/// macOS: an application menu with Quit and the standard Edit commands, so the
/// settings window behaves like any other Mac window. Returns the menu, which
/// must be kept alive, and the id of its Quit item.
#[cfg(target_os = "macos")]
fn app_menu() -> (Menu, MenuId) {
    use tray_icon::menu::Submenu;

    let quit = MenuItem::new("Quit Cantos", true, "CmdOrCtrl+Q".parse().ok());
    let quit_id = quit.id().clone();
    let app = Submenu::with_items(
        "Cantos",
        true,
        &[&PredefinedMenuItem::close_window(None), &quit],
    );
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    );
    let menu = Menu::new();
    if let (Ok(app), Ok(edit)) = (app, edit) {
        let _ = menu.append_items(&[&app, &edit]);
    }
    menu.init_for_nsapp();
    (menu, quit_id)
}

struct MenuIds {
    enabled: MenuId,
    settings: MenuId,
    native_ui: MenuId,
    log: MenuId,
    /// The tray's Quit, and on macOS the application menu's Cmd+Q as well.
    quit: Vec<MenuId>,
}

fn forward_menu_events(proxy: &EventLoopProxy<UserEvent>, ids: MenuIds) {
    let proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let event = if e.id == ids.enabled {
            UserEvent::ToggleEnabled
        } else if e.id == ids.settings {
            UserEvent::OpenSettings
        } else if e.id == ids.native_ui {
            UserEvent::ToggleSettingsUi
        } else if e.id == ids.log {
            UserEvent::OpenLog
        } else if ids.quit.contains(&e.id) {
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
