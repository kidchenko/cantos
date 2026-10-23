//! What a corner actually does when it fires.
//!
//! Every action is dispatched on a short-lived detached thread (see [`fire`]).
//! Some of these calls, a broadcast `WM_SYSCOMMAND` on Windows in particular,
//! can block for seconds if any top-level window on the system is wedged,
//! and blocking the watcher thread would stall corner detection for everyone.
//!
//! [`Action`] is one list for every platform, so a config file means the same
//! thing wherever it is read. Not every action exists everywhere: each
//! platform's [`CATALOG`] is what the settings page offers there, and an
//! action from another platform's list, say in a hand-edited config, logs
//! that it is unavailable and does nothing.
//!
//! `TaskView` and `ShowDesktop` are the two that matter most and exist
//! everywhere: Task View, Mission Control and the Activities overview are the
//! same idea under three names.

use crate::config::Action;
use crate::log::lwarn;

pub use imp::CATALOG;
#[cfg(target_os = "macos")]
pub use imp::UNTESTABLE;

/// `(group, [(kind, label)])`, where `kind` is the action's serialised name.
/// A `None` group is listed loose, outside any heading.
pub type Catalog = &'static [(
    Option<&'static str>,
    &'static [(&'static str, &'static str)],
)];

/// Run an action without blocking the caller.
pub fn fire(action: Action) {
    if action.is_none() {
        return;
    }
    std::thread::spawn(move || imp::execute(&action));
}

/// For an action that has no meaning on this platform.
#[allow(dead_code)] // each platform reaches it from a different set of arms
fn unavailable(action: &Action) {
    lwarn!(
        "{action:?} is not available on {}; nothing to do",
        std::env::consts::OS
    );
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::Catalog;
    use crate::config::Action;
    use crate::log::{ldebug, linfo, lwarn};
    use crate::shell;
    use crate::util::{pcwstr, wide};
    use windows::Win32::Foundation::{GetLastError, LPARAM, WPARAM};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Shutdown::LockWorkStation;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_ESCAPE, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
        VK_LWIN, VK_OEM_PERIOD, VK_RCONTROL, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_TAB,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    // SC_SCREENSAVE and SC_MONITORPOWER are siblings in the Windows headers, but
    // windows-rs files them under different namespaces.
    use windows::Win32::Graphics::Gdi::SC_SCREENSAVE;
    use windows::Win32::UI::WindowsAndMessaging::{
        PostMessageW, HWND_BROADCAST, SC_MONITORPOWER, SW_SHOWNORMAL, WM_SYSCOMMAND,
    };

    /// windows-rs has no constants for letter keys; they are just their ASCII
    /// codes in the virtual-key space.
    const fn vk(c: u8) -> VIRTUAL_KEY {
        VIRTUAL_KEY(c as u16)
    }
    const VK_A: VIRTUAL_KEY = vk(b'A');
    const VK_D: VIRTUAL_KEY = vk(b'D');
    const VK_E: VIRTUAL_KEY = vk(b'E');
    const VK_I: VIRTUAL_KEY = vk(b'I');
    const VK_M: VIRTUAL_KEY = vk(b'M');
    const VK_N: VIRTUAL_KEY = vk(b'N');
    const VK_P: VIRTUAL_KEY = vk(b'P');
    const VK_R: VIRTUAL_KEY = vk(b'R');
    const VK_S: VIRTUAL_KEY = vk(b'S');
    const VK_V: VIRTUAL_KEY = vk(b'V');
    const VK_X: VIRTUAL_KEY = vk(b'X');

    /// Modifiers we synthesise a release for before sending a combo. See
    /// `release_held_modifiers`.
    const MODIFIER_KEYS: [VIRTUAL_KEY; 8] = [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
    ];

    fn key_event(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) {
        if inputs.is_empty() {
            return;
        }
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            return;
        }

        // Note this does NOT catch a UIPI discard: when the focused window
        // outranks us Windows accepts the call and reports every event as
        // inserted, then throws the input away. That case is detected up front
        // in `combo` instead.
        let err = unsafe { GetLastError() };
        lwarn!(
            "SendInput inserted {sent} of {} events (error {})",
            inputs.len(),
            err.0
        );
    }

    fn is_down(vk: VIRTUAL_KEY) -> bool {
        // The high bit of GetAsyncKeyState is the "currently down" flag.
        unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
    }

    /// Synthesise a key-up for every modifier the user is physically holding.
    ///
    /// This exists because of the "require a modifier" setting: if the user has to
    /// hold Ctrl to arm a corner, then a naive `Win+Tab` actually arrives as
    /// `Ctrl+Win+Tab`, which is a different shortcut entirely. We release the held
    /// modifiers first so the combo lands clean. We deliberately do not re-press
    /// them; the user's physical release will do that naturally.
    fn release_held_modifiers() {
        let ups: Vec<INPUT> = MODIFIER_KEYS
            .iter()
            .filter(|&&vk| is_down(vk))
            .map(|&vk| key_event(vk, KEYEVENTF_KEYUP))
            .collect();
        send(&ups);
    }

    /// Press keys in order, then release them in reverse, the order a human
    /// would produce, which is what shell hooks expect.
    fn combo(keys: &[VIRTUAL_KEY]) {
        release_held_modifiers();
        let mut inputs = Vec::with_capacity(keys.len() * 2);
        for &k in keys {
            inputs.push(key_event(k, KEYBD_EVENT_FLAGS(0)));
        }
        for &k in keys.iter().rev() {
            inputs.push(key_event(k, KEYEVENTF_KEYUP));
        }
        send(&inputs);
    }

    fn broadcast_syscommand(command: u32, param: i32) {
        // PostMessage rather than SendMessage: this is fire-and-forget and cannot
        // block on an unresponsive window, which SendMessage very much can.
        unsafe {
            let _ = PostMessageW(
                Some(HWND_BROADCAST),
                WM_SYSCOMMAND,
                WPARAM(command as usize),
                LPARAM(param as isize),
            );
        }
    }

    fn run_custom(command: &str, args: &str) {
        if command.trim().is_empty() {
            lwarn!("custom action has no command set; nothing to run");
            return;
        }
        let verb = wide("open");
        let file = wide(command.trim());
        let params = wide(args);
        unsafe {
            // ShellExecuteW handles executables, scripts, documents and URLs
            // uniformly, which is exactly the range we advertise for this action.
            // ShellExecuteW reports failure as a value <= 32, not as an error.
            let rc = ShellExecuteW(
                None,
                pcwstr(&verb),
                pcwstr(&file),
                if args.is_empty() {
                    PCWSTR_NULL
                } else {
                    pcwstr(&params)
                },
                None,
                SW_SHOWNORMAL,
            );
            if rc.0 as isize <= 32 {
                lwarn!(
                    "could not run {command:?}: ShellExecute returned {}",
                    rc.0 as isize
                );
            } else {
                ldebug!("ran {command:?} {args:?}");
            }
        }
    }

    const PCWSTR_NULL: windows::core::PCWSTR = windows::core::PCWSTR::null();

    pub fn execute(action: &Action) {
        // Shell automation is COM, so this thread needs an apartment. It is a
        // fresh thread every time, hence initialise and uninitialise here rather
        // than once at startup.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        run(action);
        unsafe { CoUninitialize() };
    }

    fn run(action: &Action) {
        match action {
            Action::None => {}
            // Shell automation first, keystrokes only as a fallback. See
            // `shell` for why: synthesised input is silently discarded whenever
            // an elevated window holds focus, and these routes are not.
            Action::TaskView => dispatch("Task View", shell::window_switcher, &[VK_LWIN, VK_TAB]),
            Action::ShowDesktop => {
                dispatch("Show desktop", shell::toggle_desktop, &[VK_LWIN, VK_D])
            }
            Action::MinimizeAll => dispatch("Minimize all", shell::minimize_all, &[VK_LWIN, VK_M]),
            Action::StartMenu => {
                dispatch("Start menu", shell::start_menu, &[VK_LCONTROL, VK_ESCAPE])
            }
            Action::RunDialog => dispatch("Run", shell::run_dialog, &[VK_LWIN, VK_R]),
            Action::Settings => dispatch(
                "Settings",
                || shell::launch("ms-settings:"),
                &[VK_LWIN, VK_I],
            ),
            Action::FileExplorer => dispatch(
                "File Explorer",
                || shell::launch("explorer.exe"),
                &[VK_LWIN, VK_E],
            ),
            Action::Screenshot => dispatch(
                "Screenshot",
                || shell::launch("ms-screenclip:"),
                &[VK_LWIN, VK_LSHIFT, VK_S],
            ),
            Action::ProjectDisplay => dispatch(
                "Project",
                || shell::launch("ms-settings-displays-topology-projection:"),
                &[VK_LWIN, VK_P],
            ),
            Action::Notifications => dispatch(
                "Notifications",
                || shell::launch("ms-actioncenter:"),
                &[VK_LWIN, VK_N],
            ),

            // No shell equivalent exists for these, so they stay on synthesised
            // input and inherit its limitation against elevated windows.
            Action::DesktopLeft => combo(&[VK_LCONTROL, VK_LWIN, VK_LEFT]),
            Action::DesktopRight => combo(&[VK_LCONTROL, VK_LWIN, VK_RIGHT]),
            Action::Search => combo(&[VK_LWIN, VK_S]),
            Action::QuickSettings => combo(&[VK_LWIN, VK_A]),
            Action::QuickLink => combo(&[VK_LWIN, VK_X]),
            Action::Clipboard => combo(&[VK_LWIN, VK_V]),
            Action::Emoji => combo(&[VK_LWIN, VK_OEM_PERIOD]),
            Action::LockWorkstation => unsafe {
                let _ = LockWorkStation();
            },
            Action::Screensaver => broadcast_syscommand(SC_SCREENSAVE, 0),
            // 2 == power off. Note this is a request, not a guarantee: systems
            // using Modern Standby may ignore it.
            Action::DisplayOff => broadcast_syscommand(SC_MONITORPOWER, 2),
            Action::Custom { command, args } => run_custom(command, args),
            Action::AppWindows
            | Action::Launchpad
            | Action::QuickNote
            | Action::DisableScreenSaver => super::unavailable(action),
        }
    }

    /// Synthesised keys by default; the shell route only when keys would be lost.
    ///
    /// The ordering matters and was originally the other way round, which was a
    /// mistake: `IShellDispatch` reports success from the COM call itself, not
    /// from the UI actually appearing, so a shell route that quietly does nothing
    /// still looked like it worked and the keystroke fallback never ran. Keys are
    /// the better default because their behaviour is observable and correct
    /// wherever they are permitted at all, which is everywhere except an elevated
    /// foreground window.
    fn dispatch<F: FnOnce() -> bool>(what: &str, shell_route: F, keys: &[VIRTUAL_KEY]) {
        if let Some(reason) = crate::elevation::keystroke_will_be_discarded() {
            lwarn!("{what}: keys would be discarded ({reason}); trying shell automation");
            if shell_route() {
                linfo!("{what}: sent via shell automation");
                return;
            }
            lwarn!("{what}: shell automation unavailable too; sending keys anyway");
        }
        combo(keys);
    }

    /// What the settings page offers here, in the order and groups it shows them.
    pub const CATALOG: Catalog = &[
        (None, &[("none", "Nothing")]),
        (
            Some("Windows & desktops"),
            &[
                ("taskView", "Task View"),
                ("showDesktop", "Show desktop"),
                ("minimizeAll", "Minimize all"),
                ("desktopLeft", "Previous desktop"),
                ("desktopRight", "Next desktop"),
            ],
        ),
        (
            Some("Shell"),
            &[
                ("startMenu", "Start menu"),
                ("search", "Search"),
                ("settings", "Settings"),
                ("quickSettings", "Quick Settings"),
                ("notifications", "Notifications"),
                ("quickLink", "Quick Link menu"),
                ("runDialog", "Run\u{2026}"),
            ],
        ),
        (
            Some("Tools"),
            &[
                ("fileExplorer", "File Explorer"),
                ("screenshot", "Screenshot"),
                ("clipboard", "Clipboard history"),
                ("emoji", "Emoji picker"),
                ("projectDisplay", "Project / second screen"),
            ],
        ),
        (
            Some("Power & screen"),
            &[
                ("lockWorkstation", "Lock"),
                ("screensaver", "Screensaver"),
                ("displayOff", "Turn display off"),
            ],
        ),
        (None, &[("custom", "Custom command")]),
    ];
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
//
// Two kinds of corner. The Dock fires its own (see `native`): those actions
// run here only for the Test button, and are exactly the list System
// Settings offers. Cantos fires the rest itself, through its own detector,
// for as long as it is running: the second group of the catalog.
//
// The Dock owns Mission Control, App Exposé, Show Desktop and Launchpad, and
// listens for a notification to open each one: the same notifications its
// own hot corners send. `CoreDockSendNotification` is private, so it is looked
// up at runtime rather than linked; should a future macOS drop it, Cantos
// falls back to opening the Mission Control app instead of failing to launch.
//
// Keystroke actions need the Accessibility permission to reach other apps, so
// those check for it first and, if it is missing, ask macOS to show the prompt
// rather than typing into the void.
#[cfg(target_os = "macos")]
mod imp {
    use super::Catalog;
    use crate::cg;
    use crate::config::Action;
    use crate::log::{ldebug, linfo, lwarn};
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::process::Command;

    pub fn execute(action: &Action) {
        match action {
            Action::None => {}
            Action::TaskView => dock("com.apple.expose.awake", &[]),
            Action::AppWindows => dock("com.apple.expose.front.awake", &["2"]),
            Action::ShowDesktop => dock("com.apple.showdesktop.awake", &["1"]),
            Action::Launchpad => {
                if !dock_notify(c"com.apple.launchpad.toggle") {
                    // Launchpad until macOS 15, Apps from 26.
                    let _ = open(&["-a", "Launchpad"]) || open(&["-a", "Apps"]);
                }
            }
            Action::LockWorkstation => lock(),
            Action::Screensaver => {
                open(&["-b", "com.apple.ScreenSaver.Engine"]);
            }
            Action::DisplayOff => run("/usr/bin/pmset", &["displaysleepnow"]),
            // Cantos's own: the system's shortcuts for these, or the app.
            Action::DesktopLeft => keys(KEY_LEFT, cg::FLAG_CONTROL),
            Action::DesktopRight => keys(KEY_RIGHT, cg::FLAG_CONTROL),
            Action::Search => keys(KEY_SPACE, cg::FLAG_COMMAND),
            Action::Emoji => keys(KEY_SPACE, cg::FLAG_CONTROL | cg::FLAG_COMMAND),
            Action::Settings => {
                open(&["-b", "com.apple.systempreferences"]);
            }
            Action::FileExplorer => {
                open(&["-b", "com.apple.finder"]);
            }
            Action::Screenshot => {
                open(&["-b", "com.apple.screenshot.launcher"]);
            }
            Action::Custom { command, args } => super::run_custom(command, args),
            // Notification Centre and Quick Note have no way in from outside
            // the Dock, and "disable screen saver" is a state, not an action;
            // the settings window offers no Test for them.
            _ => super::unavailable(action),
        }
    }

    /// The Dock's actions, in System Settings' order, then Cantos's own. The
    /// group names are what the settings window labels the two halves with.
    pub const CATALOG: Catalog = &[
        (None, &[("none", "Nothing")]),
        (
            Some("macOS"),
            &[
                ("taskView", "Mission Control"),
                ("appWindows", "Application Windows"),
                ("showDesktop", "Desktop"),
                ("notifications", "Notification Centre"),
                ("launchpad", "Launchpad"),
                ("quickNote", "Quick Note"),
                ("screensaver", "Start Screen Saver"),
                ("disableScreenSaver", "Disable Screen Saver"),
                ("displayOff", "Put Display to Sleep"),
                ("lockWorkstation", "Lock Screen"),
            ],
        ),
        (
            Some("Cantos"),
            &[
                ("search", "Spotlight"),
                ("fileExplorer", "Finder"),
                ("settings", "System Settings"),
                ("screenshot", "Screenshot"),
                ("emoji", "Emoji & Symbols"),
                ("desktopLeft", "Previous desktop"),
                ("desktopRight", "Next desktop"),
                ("custom", "Custom command"),
            ],
        ),
    ];

    // Virtual key codes from HIToolbox's Events.h. Positional, not
    // characters, so they hold on every keyboard layout.
    const KEY_SPACE: u16 = 0x31;
    const KEY_LEFT: u16 = 0x7B;
    const KEY_RIGHT: u16 = 0x7C;

    /// Post `key` with exactly `flags` held. Setting the flags on the event,
    /// rather than pressing modifier keys, also means a key the user holds to
    /// arm the corner does not leak into the shortcut.
    fn keys(key: u16, flags: u64) {
        if !accessibility_trusted() {
            return;
        }
        unsafe {
            for down in [true, false] {
                let e = cg::CGEventCreateKeyboardEvent(std::ptr::null(), key, down);
                if e.is_null() {
                    lwarn!("could not create a keyboard event");
                    return;
                }
                cg::CGEventSetFlags(e, flags);
                cg::CGEventPost(cg::HID_EVENT_TAP, e);
                cg::CFRelease(e);
            }
        }
    }

    /// Whether macOS will deliver our keystrokes. If not, ask it to show the
    /// "Cantos would like to control this computer" prompt, which links
    /// straight to the switch in System Settings.
    fn accessibility_trusted() -> bool {
        if unsafe { cg::AXIsProcessTrusted() } {
            return true;
        }
        lwarn!(
            "keystroke actions need Accessibility permission: System Settings > \
             Privacy & Security > Accessibility > Cantos"
        );
        // `{AXTrustedCheckOptionPrompt: true}` as a CFDictionary.
        use objc2_foundation::{NSDictionary, NSNumber, NSString};
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");
        let yes = NSNumber::new_bool(true);
        let options = NSDictionary::from_slices(&[&*key], &[&*yes]);
        unsafe { cg::AXIsProcessTrustedWithOptions(&*options as *const _ as *const c_void) }
    }

    /// Kinds the Test button cannot run here; see `execute`.
    pub const UNTESTABLE: &[&str] = &["notifications", "quickNote", "disableScreenSaver"];

    type DockNotify = unsafe extern "C" fn(*const c_void, c_int);

    /// The Dock's notification, or failing that `open -a "Mission Control"`
    /// with the argument the Mission Control app takes for the same view.
    fn dock(notification: &str, fallback_args: &[&str]) {
        let name = std::ffi::CString::new(notification).unwrap_or_default();
        if dock_notify(&name) {
            return;
        }
        let mut argv = vec!["-a", "Mission Control"];
        if !fallback_args.is_empty() {
            argv.push("--args");
            argv.extend_from_slice(fallback_args);
        }
        open(&argv);
    }

    fn dock_notify(notification: &CStr) -> bool {
        let Some(f) = (unsafe {
            symbol::<DockNotify>(
                c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
                c"CoreDockSendNotification",
            )
        }) else {
            lwarn!("CoreDockSendNotification is missing; falling back to Mission Control.app");
            return false;
        };
        let name = cf_string(notification);
        if name.is_null() {
            return false;
        }
        unsafe {
            f(name, 0);
            cg::CFRelease(name);
        }
        ldebug!("sent {notification:?} to the Dock");
        true
    }

    /// Lock now, the way the menu bar's Lock Screen does, by the private
    /// `SACLockScreenImmediate`. Without it, sleeping the display locks too
    /// whenever a password is required on wake, which is the default.
    fn lock() {
        type Lock = unsafe extern "C" fn() -> c_int;
        let f = unsafe {
            symbol::<Lock>(
                c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login",
                c"SACLockScreenImmediate",
            )
        };
        match f {
            Some(f) => {
                unsafe { f() };
            }
            None => {
                lwarn!("SACLockScreenImmediate is missing; sleeping the display instead");
                run("/usr/bin/pmset", &["displaysleepnow"]);
            }
        }
    }

    /// `open` with arguments; true if it ran and succeeded.
    fn open(argv: &[&str]) -> bool {
        match Command::new("/usr/bin/open").args(argv).status() {
            Ok(s) if s.success() => {
                ldebug!("open {argv:?}");
                true
            }
            Ok(s) => {
                lwarn!("open {argv:?} exited with {s}");
                false
            }
            Err(e) => {
                lwarn!("could not run open {argv:?}: {e}");
                false
            }
        }
    }

    fn run(program: &str, args: &[&str]) {
        match Command::new(program).args(args).status() {
            Ok(s) if s.success() => linfo!("ran {program} {args:?}"),
            Ok(s) => lwarn!("{program} {args:?} exited with {s}"),
            Err(e) => lwarn!("could not run {program}: {e}"),
        }
    }

    /// A function from a system framework, looked up at runtime.
    unsafe fn symbol<F: Copy>(library: &CStr, name: &CStr) -> Option<F> {
        // Both frameworks are already mapped into every GUI process, so this
        // finds the loaded image rather than loading anything.
        let handle = libc::dlopen(library.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let sym = libc::dlsym(handle, name.as_ptr());
        if sym.is_null() {
            return None;
        }
        Some(std::mem::transmute_copy::<*mut c_void, F>(&sym))
    }

    fn cf_string(s: &CStr) -> *const c_void {
        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFStringCreateWithCString(
                alloc: *const c_void,
                s: *const c_char,
                encoding: u32,
            ) -> *const c_void;
        }
        const UTF8: u32 = 0x0800_0100;
        unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) }
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
//
// X11, through the window manager's EWMH interface where one exists (desktops,
// show desktop) and XTest keystrokes where the desktop environment only binds
// a key (the overview, screenshots). Everything else is a well-known command,
// tried in order until one is installed.
#[cfg(target_os = "linux")]
mod imp {
    use super::Catalog;
    use crate::config::Action;
    use crate::log::{ldebug, lwarn};
    use crate::x11;
    use std::ffi::{c_long, c_uint};
    use std::process::{Command, Stdio};
    use x11_dl::keysym::{
        XK_Alt_L, XK_Alt_R, XK_Control_L, XK_Control_R, XK_Print, XK_Shift_L, XK_Shift_R,
        XK_Super_L, XK_Super_R,
    };
    use x11_dl::xlib;

    pub fn execute(action: &Action) {
        match action {
            Action::None => {}
            // GNOME opens the Activities overview on a tap of Super, and KDE
            // its launcher; there is no EWMH request for either.
            Action::TaskView => tap(&[XK_Super_L]),
            Action::ShowDesktop => toggle_showing_desktop(),
            Action::DesktopLeft => switch_desktop(-1),
            Action::DesktopRight => switch_desktop(1),
            Action::Screenshot => tap(&[XK_Print]),
            Action::Settings => first_of(&[
                &["gnome-control-center"],
                &["systemsettings"],
                &["systemsettings5"],
                &["xfce4-settings-manager"],
                &["cinnamon-settings"],
                &["mate-control-center"],
            ]),
            Action::FileExplorer => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                first_of(&[&["xdg-open", &home]]);
            }
            Action::LockWorkstation => first_of(&[
                &["loginctl", "lock-session"],
                &["xdg-screensaver", "lock"],
                &["xset", "s", "activate"],
            ]),
            Action::Screensaver => {
                first_of(&[&["xdg-screensaver", "activate"], &["xset", "s", "activate"]])
            }
            Action::DisplayOff => first_of(&[&["xset", "dpms", "force", "off"]]),
            Action::Custom { command, args } => super::run_custom(command, args),
            Action::AppWindows
            | Action::Launchpad
            | Action::QuickNote
            | Action::DisableScreenSaver
            | Action::MinimizeAll
            | Action::StartMenu
            | Action::Search
            | Action::QuickSettings
            | Action::Notifications
            | Action::QuickLink
            | Action::RunDialog
            | Action::Clipboard
            | Action::Emoji
            | Action::ProjectDisplay => super::unavailable(action),
        }
    }

    /// What the settings page offers here, in the order and groups it shows them.
    pub const CATALOG: Catalog = &[
        (None, &[("none", "Nothing")]),
        (
            Some("Windows & desktops"),
            &[
                ("taskView", "Overview (Super)"),
                ("showDesktop", "Show desktop"),
                ("desktopLeft", "Previous desktop"),
                ("desktopRight", "Next desktop"),
            ],
        ),
        (
            Some("Tools"),
            &[
                ("fileExplorer", "Files"),
                ("screenshot", "Screenshot (Print)"),
                ("settings", "Settings"),
            ],
        ),
        (
            Some("Power & screen"),
            &[
                ("lockWorkstation", "Lock"),
                ("screensaver", "Screensaver"),
                ("displayOff", "Turn display off"),
            ],
        ),
        (None, &[("custom", "Custom command")]),
    ];

    /// Released before a tap; see `X::release_held_modifiers`.
    const MODIFIERS: [c_uint; 8] = [
        XK_Control_L,
        XK_Control_R,
        XK_Shift_L,
        XK_Shift_R,
        XK_Alt_L,
        XK_Alt_R,
        XK_Super_L,
        XK_Super_R,
    ];

    fn tap(keysyms: &[c_uint]) {
        let sent = x11::with(|x| {
            x.release_held_modifiers(&MODIFIERS);
            x.tap(keysyms)
        });
        if sent != Some(true) {
            lwarn!("could not send keys {keysyms:x?}");
        }
    }

    /// `_NET_SHOWING_DESKTOP` is a toggle the window manager keeps: read it,
    /// ask for the opposite.
    fn toggle_showing_desktop() {
        x11::with(|x| {
            let showing = x
                .property(x.root, "_NET_SHOWING_DESKTOP", xlib::XA_CARDINAL)
                .and_then(|v| v.first().copied())
                .unwrap_or(0);
            let want = if showing == 0 { 1 } else { 0 };
            x.ask_window_manager(x.root, "_NET_SHOWING_DESKTOP", [want, 0, 0, 0, 0]);
        });
    }

    /// Move one workspace along. Wraps at the ends, which most desktops' own
    /// shortcuts do not: a corner gives no feedback when nothing happens, so
    /// wrapping beats silently staying put.
    fn switch_desktop(step: c_long) {
        x11::with(|x| {
            let get = |name| {
                x.property(x.root, name, xlib::XA_CARDINAL)
                    .and_then(|v| v.first().copied())
            };
            let (Some(current), Some(count)) =
                (get("_NET_CURRENT_DESKTOP"), get("_NET_NUMBER_OF_DESKTOPS"))
            else {
                lwarn!("the window manager does not publish its desktops (EWMH)");
                return;
            };
            let count = count.max(1) as c_long;
            let next = (current as c_long + step).rem_euclid(count);
            x.ask_window_manager(x.root, "_NET_CURRENT_DESKTOP", [next, 0, 0, 0, 0]);
        });
    }

    /// Run the first of `commands` that exists. Spawned, not waited on: a
    /// settings app runs until the user closes it.
    fn first_of(commands: &[&[&str]]) {
        for argv in commands {
            let Some((program, args)) = argv.split_first() else {
                continue;
            };
            match Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    ldebug!("ran {argv:?}");
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    lwarn!("could not run {argv:?}: {e}");
                    return;
                }
            }
        }
        lwarn!("none of {commands:?} is installed");
    }
}

/// A custom command on macOS and Linux.
///
/// Mirrors what `ShellExecute` does on Windows: something executable runs
/// with the arguments, anything else (a URL, a document, an app bundle) is
/// handed to the desktop's opener, `open` or `xdg-open`. Arguments go through
/// `sh` so quoting in them works the way it does in a terminal. On a Mac a
/// bare name like `Safari` is an app, opened with `open -a`.
#[cfg(unix)]
fn run_custom(command: &str, args: &str) {
    use crate::log::ldebug;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    let command = command.trim();
    if command.is_empty() {
        lwarn!("custom action has no command set; nothing to run");
        return;
    }
    let expanded = match command.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => command.to_string(),
    };
    let path = std::path::Path::new(&expanded);
    let is_program = path.is_file()
        && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    let is_url = command.contains("://");
    let bare = !is_url && !command.contains('/');
    let mac = cfg!(target_os = "macos");

    let mut cmd = if is_program || (bare && !mac) {
        // A bare name like `firefox` is a program on PATH; the shell finds it.
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(format!("exec {} {args}", shell_quote(&expanded)));
        c
    } else if mac {
        let mut c = Command::new("/usr/bin/open");
        if bare {
            c.arg("-a");
        }
        c.arg(&expanded);
        // `open` passes arguments to an app only after `--args`.
        if !args.trim().is_empty() {
            c.arg("--args").args(args.split_whitespace());
        }
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(&expanded);
        c
    };
    match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => ldebug!("ran {command:?} {args:?}"),
        Err(e) => lwarn!("could not run {command:?}: {e}"),
    }
}

/// Single-quote for `sh`, so a path with spaces stays one word.
#[cfg(unix)]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::shell_quote;

    #[test]
    fn quoting_keeps_spaces_and_quotes_in_one_word() {
        assert_eq!(
            shell_quote("/Applications/My App"),
            "'/Applications/My App'"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
