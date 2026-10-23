//! The settings window: a tao window hosting the system webview: WebView2 on
//! Windows, WKWebView on macOS, WebKitGTK on Linux.
//!
//! The window is built on demand and dropped when closed. That teardown is
//! the whole reason a webview is affordable here: while the window is open we
//! pay the usual ~80 MB of browser processes, and the moment it closes we are
//! back to a few megabytes of tray app. Keeping the environment warm would
//! make reopening instant but would hold that memory for the entire session,
//! which is exactly the trade this app exists to avoid.

pub mod child;
pub mod page;
#[cfg(target_os = "linux")]
pub mod window_gtk;
#[cfg(target_os = "macos")]
pub mod window_mac;
#[cfg(windows)]
pub mod window_win;

use std::path::PathBuf;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Icon, Window, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

const HTML: &str = include_str!("index.html");
const TRAY_RGBA: &[u8] = include_bytes!("../../assets/tray-64.rgba");
const ICON_EDGE: u32 = 64;
#[cfg(windows)]
const WEBVIEW2_DOWNLOAD: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Decoded window/tray icon pixels. Kept as raw RGBA rather than a PNG so we
/// do not need an image decoder in the dependency tree.
pub fn icon_rgba() -> Vec<u8> {
    TRAY_RGBA.to_vec()
}

pub fn window_icon() -> Option<Icon> {
    Icon::from_rgba(icon_rgba(), ICON_EDGE, ICON_EDGE).ok()
}

/// The installed webview's version, or `None` if there is not one.
///
/// On Windows this asks the WebView2 runtime itself
/// (`GetAvailableCoreWebView2BrowserVersionString` via wry) rather than reading
/// `EdgeUpdate` registry keys, so it stays correct however the runtime was
/// deployed: per-user, per-machine, or fixed-version. macOS always has
/// WebKit; Linux has WebKitGTK if the package that installed Cantos pulled it
/// in, which is the one case where this can be `None` there.
pub fn runtime_version() -> Option<String> {
    wry::webview_version().ok()
}

pub use imp::report_open_failure;

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::{runtime_version, WEBVIEW2_DOWNLOAD};
    use crate::util::{pcwstr, wide};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_STYLE,
    };

    /// Tell the user, in a dialog, why the settings window did not appear.
    ///
    /// The settings window is this app's only UI, so a failure recorded nowhere
    /// but the log reads as "the app is broken": the user clicks Settings, nothing
    /// happens at all, and the log they would need is behind the same tray menu
    /// they have just stopped trusting.
    ///
    /// By far the likeliest cause is a missing WebView2 runtime. Windows 11 ships
    /// it, and on Windows 10 it arrives with Edge, but LTSC, N editions, and
    /// locked-down enterprise images can be without it, and this app supports
    /// Windows 10.
    pub fn report_open_failure(error: &str) {
        match runtime_version() {
            // The runtime is there, so this is something else. Nothing to offer
            // beyond the error itself.
            Some(version) => {
                alert(
                    "Cantos: settings unavailable",
                    &format!(
                        "Cantos could not open its settings window.\n\n\
                         The WebView2 runtime is installed (version {version}), so \
                         that is not the cause:\n\n{error}\n\n\
                         Hot corners themselves are unaffected and keep working.",
                    ),
                    MB_ICONERROR | MB_OK,
                );
            }
            None => {
                let opened = alert(
                    "Cantos: WebView2 required",
                    "Cantos needs the Microsoft WebView2 runtime to show its \
                     settings window, and it is not installed on this PC.\n\n\
                     Your hot corners keep working; only the settings window is \
                     affected. You can also edit the settings file directly; it is \
                     in %APPDATA%\\Cantos\\config.json.\n\n\
                     Open the WebView2 download page now?",
                    MB_ICONWARNING | MB_YESNO,
                );
                if opened {
                    crate::util::shell_open(WEBVIEW2_DOWNLOAD);
                }
            }
        }
    }

    /// Returns whether the user chose Yes. Always false for an OK-only dialog.
    fn alert(title: &str, text: &str, style: MESSAGEBOX_STYLE) -> bool {
        let title = wide(title);
        let text = wide(text);
        // No owner window: the tray has none, and the event loop is blocked for
        // as long as this is up either way.
        unsafe { MessageBoxW(None, pcwstr(&text), pcwstr(&title), style) == IDYES }
    }
}

// ---------------------------------------------------------------------------
// macOS and Linux
// ---------------------------------------------------------------------------
#[cfg(unix)]
mod imp {
    use crate::config::Config;
    use crate::log::lwarn;
    use std::process::{Command, Stdio};

    /// Tell the user, in a dialog, why the settings window did not appear.
    /// See the Windows twin for why a log line alone is not enough.
    pub fn report_open_failure(error: &str) {
        let config = Config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let hint = if cfg!(target_os = "linux") {
            " On Linux it needs WebKitGTK (libwebkit2gtk-4.1)."
        } else {
            ""
        };
        alert(&format!(
            "Cantos could not open its settings window.{hint}\n\n{error}\n\n\
             Hot corners themselves are unaffected and keep working. You can \
             also edit the settings file directly: {config}"
        ));
    }

    /// AppleScript's alert on a Mac, zenity or kdialog on Linux; just the log
    /// when none of them is there.
    fn alert(text: &str) {
        let attempts: Vec<(&str, Vec<String>)> = if cfg!(target_os = "macos") {
            let quoted = text.replace('\\', "\\\\").replace('"', "\\\"");
            vec![(
                "/usr/bin/osascript",
                vec![
                    "-e".into(),
                    format!("display alert \"Cantos\" message \"{quoted}\" as warning"),
                ],
            )]
        } else {
            vec![
                (
                    "zenity",
                    vec![
                        "--warning".into(),
                        "--title=Cantos".into(),
                        format!("--text={text}"),
                    ],
                ),
                (
                    "kdialog",
                    vec![
                        "--title".into(),
                        "Cantos".into(),
                        "--sorry".into(),
                        text.into(),
                    ],
                ),
            ]
        };
        for (program, args) in attempts {
            let spawned = Command::new(program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if spawned.is_ok() {
                return;
            }
        }
        lwarn!("no dialog tool to show the settings failure with");
    }
}

/// The settings window, whichever kind is open.
///
/// `Web` is the original: this page in the system webview, inside the tray
/// process. `Native` is the replacement being built: the OS's own controls,
/// in a child process (see [`child`]). `settingsUi` in config.json picks; both
/// speak the same protocol, so nothing above this type knows which it has.
pub enum Settings {
    Web(Box<WebSettings>),
    Native(child::Child),
}

impl Settings {
    /// Call `window.__hc.<name>(...args)` in the page, or send the same call
    /// to the native window. Failures are ignored: the window may be closing
    /// underneath us, and a dropped status message is harmless.
    pub fn push(&self, name: &str, args: &[serde_json::Value]) {
        match self {
            Settings::Web(w) => {
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                let _ = w
                    .webview
                    .evaluate_script(&format!("window.__hc.{name}({})", args.join(",")));
            }
            Settings::Native(c) => c.push(name, args),
        }
    }

    /// Bring an already open window to the front.
    pub fn raise(&self) {
        match self {
            Settings::Web(w) => {
                w.window.set_visible(true);
                w.window.set_minimized(false);
                w.window.set_focus();
            }
            Settings::Native(c) => c.push("raise", &[]),
        }
    }
}

pub struct WebSettings {
    // Field order is load-bearing: Rust drops fields in declaration order, and
    // `WebView`'s drop closes its WebView2 controller and un-subclasses the
    // parent HWND. With the window first, both of those ran against a window
    // that had already been destroyed, every time the settings window closed.
    pub webview: WebView,
    #[allow(dead_code)] // dropped with the struct; that teardown is the point
    pub window: Window,
}

/// `platform` is a script that runs before the page's own, defining
/// `window.__platform`: see [`page::platform_script`].
pub fn build<T, F>(
    target: &EventLoopWindowTarget<T>,
    platform: &str,
    on_ipc: F,
) -> Result<WebSettings, String>
where
    F: Fn(String) + 'static,
{
    let window = WindowBuilder::new()
        .with_title("Cantos")
        .with_inner_size(LogicalSize::new(680.0, 828.0))
        .with_min_inner_size(LogicalSize::new(560.0, 560.0))
        .with_window_icon(window_icon())
        .with_visible(true)
        .build(target)
        .map_err(|e| format!("could not create the settings window: {e}"))?;

    #[cfg(windows)]
    apply_titlebar_theme(&window);

    // WebView2 needs somewhere writable to keep its user data folder, and left
    // to itself it puts that folder *next to the executable*. That is fine
    // running out of `target\release`, and fails with `E_ACCESSDENIED
    // (0x80070005)` the moment the app is actually installed, because nobody
    // can write to `C:\Program Files\Cantos`. The settings window simply never
    // opened for anyone who used the installer.
    let mut context = WebContext::new(webview_data_dir());

    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(HTML)
        .with_initialization_script(platform)
        // The page is a static local document with no navigation, so there is
        // nothing useful behind a context menu or devtools for an end user.
        .with_devtools(false)
        .with_ipc_handler(move |req| on_ipc(req.body().to_string()));

    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    // WebKitGTK is a GTK widget, packed into the box tao keeps in the window.
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or("the settings window has no GTK container")?;
        builder.build_gtk(vbox)
    };
    let webview = webview.map_err(|e| format!("could not create the webview: {e}"))?;

    // A menu bar app is never the active app, so a new window would open
    // behind whatever the user is looking at.
    #[cfg(target_os = "macos")]
    window.set_focus();

    Ok(WebSettings { webview, window })
}

/// Where the webview keeps its cache and profile.
///
/// `LOCALAPPDATA` rather than `APPDATA`: this is a browser cache measured in
/// megabytes, and roaming it between machines would be nothing but a tax. It
/// deliberately sits beside the app's own data rather than inside it, so that
/// clearing a wedged webview profile cannot take `config.json` with it.
#[cfg(windows)]
fn webview_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        // No LOCALAPPDATA is close to impossible, but a temp folder still
        // beats the install directory, which is the one place guaranteed to
        // fail.
        .unwrap_or_else(std::env::temp_dir);
    Some(base.join("Cantos").join("WebView2"))
}

/// WKWebView keeps its data under `~/Library/WebKit/<bundle id>` whatever it
/// is told, so there is nothing to choose.
#[cfg(target_os = "macos")]
fn webview_data_dir() -> Option<PathBuf> {
    None
}

/// WebKitGTK's cache belongs in the XDG cache directory, beside the config
/// rather than inside it for the same reason as on Windows.
#[cfg(target_os = "linux")]
fn webview_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("cantos").join("webkit"))
}

/// Match the titlebar to the system theme. macOS and GTK do this themselves.
///
/// The page itself follows `prefers-color-scheme`, but the non-client area is
/// Windows' to draw. Without this a dark page gets a bright white titlebar,
/// which looks broken.
#[cfg(windows)]
fn apply_titlebar_theme(window: &Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};

    let hwnd = HWND(window.hwnd() as *mut _);
    let dark: i32 = if crate::theme::prefers_dark() { 1 } else { 0 };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}
