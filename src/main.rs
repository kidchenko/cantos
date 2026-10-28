// No console window: this is a tray app, and a flashing conhost on login
// would be the first thing every user complained about. No-op off Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

//! Startup and wiring. The work happens elsewhere:
//!
//! | module      | responsibility                                      |
//! |-------------|-----------------------------------------------------|
//! | `watcher`   | polls the cursor on its own thread                  |
//! | `trigger`   | decides when a corner fires: the rules              |
//! | `desktop`   | asks the OS what is happening right now             |
//! | `native`    | macOS: the Dock's own hot corners, read and written |
//! | `geometry`  | monitors, and the hitboxes derived from them        |
//! | `actions`   | what a corner does once it fires                    |
//! | `app`       | the tray icon, its menu, and the settings window    |
//! | `ui`        | the settings window and the protocol it speaks     |
//!
//! Platform code sits behind a `mod imp` per module, one for each of Windows,
//! macOS and Linux, plus platform-only modules: `elevation` and `shell`
//! (Windows), `cg`, `icon` and `native` (macOS), `x11` (Linux). The pieces
//! every kidchenko tray app shares (log, config folder, single instance,
//! theme) come from `traykit`.
//!
//! macOS is the odd one out. It has hot corners of its own, in the Dock, so
//! there Cantos is mostly a front end for those (`native`). Its own detector
//! only handles corners given a Cantos action, one the Dock cannot do.

mod actions;
mod app;
mod autostart;
mod config;
mod geometry;
mod ui;

mod desktop;
mod trigger;
mod watcher;

#[cfg(windows)]
mod elevation;
#[cfg(windows)]
mod shell;

#[cfg(target_os = "macos")]
mod cg;
#[cfg(target_os = "macos")]
mod icon;
#[cfg(target_os = "macos")]
mod native;

#[cfg(target_os = "linux")]
mod x11;

use app::{App, Flow, UserEvent};
use config::Config;
use std::sync::{Arc, Mutex};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use traykit::{lerror, linfo};

fn main() {
    traykit::init(traykit::App {
        name: "Cantos",
        id: "cantos",
        bundle_id: "com.kidchenko.cantos",
        summary: "Hot corners",
    });

    // The native settings window is this same binary, started by the tray
    // as a child (see ui::child). It has no tray, no single-instance lock and
    // no detector: just the window.
    #[cfg(target_os = "macos")]
    if std::env::args().any(|a| a == ui::child::FLAG) {
        let args: Vec<String> = std::env::args().collect();
        if let Some(i) = args.iter().position(|a| a == "--preview") {
            let out = args
                .get(i + 1)
                .map(String::as_str)
                .unwrap_or("settings.png");
            let dark = args.iter().any(|a| a == "dark");
            if let Err(e) = ui::window_mac::preview(out, dark) {
                eprintln!("preview failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        ui::window_mac::run();
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == ui::child::FLAG) {
        ui::window_gtk::run();
        return;
    }
    #[cfg(windows)]
    if std::env::args().any(|a| a == ui::child::FLAG) {
        ui::window_win::run();
        return;
    }

    log_startup();

    // If we are the second launch, the first has been told to show its
    // settings window and there is nothing left for us to do.
    let Some(signal) = traykit::single_instance::acquire() else {
        linfo!("another instance owns the tray; asked it to show settings, exiting");
        return;
    };

    let cfg: config::SharedConfig = Arc::new(Mutex::new(Config::load()));
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Menu-bar agent, no Dock icon. tao resets the activation policy as it
    // starts, overriding LSUIElement, so it has to be asked for here too.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
    }
    let proxy = event_loop.create_proxy();

    // Held for the life of the process; dropping it stops the polling thread.
    // It also owns config-file change detection, so an edit made in an editor
    // reaches the tray and an open settings window rather than sitting there
    // until the next restart, or being overwritten by our next save.
    let _watcher = {
        let proxy = proxy.clone();
        watcher::Watcher::start(cfg.clone(), move || {
            let _ = proxy.send_event(UserEvent::ConfigReloaded);
        })
    };
    // On a Mac the Dock fires its own corners. Its settings are watched too,
    // so a change made in System Settings reaches an open window.
    #[cfg(target_os = "macos")]
    {
        linfo!("hot corners: {:?}", native::read());
        // A corner set before the permission was lost (a new build, say)
        // asks again now, not when it next fires.
        let keystrokes = cfg
            .lock()
            .is_ok_and(|c| c.corners.iter().any(actions::needs_accessibility));
        if keystrokes {
            actions::request_accessibility();
        }
        let proxy = proxy.clone();
        native::watch(move || {
            let _ = proxy.send_event(UserEvent::ConfigReloaded);
        });
    }

    // Launching the app again is how a user asks for the settings window.
    let signal_proxy = proxy.clone();
    signal.watch(move || {
        let _ = signal_proxy.send_event(UserEvent::OpenSettings);
    });

    // `--settings` opens the window straight away. The installer uses it for
    // its "launch now" checkbox, so a first run lands somewhere useful rather
    // than looking like nothing happened. Acted on once the app exists rather
    // than sent as an event now: GTK delivers events queued before the loop
    // starts ahead of `Init`, when there is no app yet to receive them.
    let open_settings = std::env::args().any(|a| a == "--settings");

    // The tray is built once the loop is running: on macOS the status item
    // cannot be created before the application has finished launching.
    let mut app: Option<App> = None;

    event_loop.run(move |event, target, control_flow| {
        // Tray apps idle; nothing here needs a continuous redraw.
        *control_flow = ControlFlow::Wait;

        if let Event::NewEvents(StartCause::Init) = event {
            #[cfg(target_os = "macos")]
            drop_launch_activation();
            match App::new(cfg.clone(), proxy.clone()) {
                Ok(mut a) => {
                    linfo!("tray icon created");
                    if open_settings {
                        a.handle(Event::UserEvent(UserEvent::OpenSettings), target);
                    }
                    app = Some(a);
                }
                Err(e) => {
                    lerror!("{e}");
                    *control_flow = ControlFlow::Exit;
                }
            }
            return;
        }

        if let Some(a) = app.as_mut() {
            if a.handle(event, target) == Flow::Exit {
                app = None;
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

/// tao asks macOS to activate the app as it finishes launching. A menu bar
/// app has nothing to bring forward, and on macOS 26 the request stays
/// pending until the first click on the menu bar icon, then completes while
/// the menu is open and closes it. Withdraw it before anything is shown.
#[cfg(target_os = "macos")]
fn drop_launch_activation() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).deactivate();
    }
}

/// The header every log file opens with. Worth the four lines: "which build,
/// running from where, at what privilege" answers most support questions
/// before the first corner is ever pressed.
fn log_startup() {
    linfo!(
        "--- Cantos {} starting (pid {}) ---",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    linfo!(
        "exe: {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".into())
    );
    linfo!("args: {:?}", std::env::args().skip(1).collect::<Vec<_>>());
    linfo!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    // Each platform's answer to "why would an action silently do nothing?".
    #[cfg(windows)]
    linfo!(
        "integrity: {}",
        elevation::own_level()
            .map(elevation::describe)
            .unwrap_or("unknown")
    );
    #[cfg(target_os = "linux")]
    if let Some(why) = x11::session_warning() {
        traykit::lwarn!("{why}");
    }
}
