//! The settings window: a native window per platform, AppKit on macOS
//! ([`window_mac`]), Win32 on Windows ([`window_win`]), GTK on Linux
//! ([`window_gtk`]), each drawing the same design from the same tokens.
//!
//! Each runs in a child process (see [`child`]), started when the window is
//! opened and gone when it is closed. While the window is up the tray pays for
//! a UI toolkit; the moment it closes, the tray is back to a few megabytes,
//! which is the trade this app exists to make. [`window`] is the protocol the
//! two processes speak.

pub mod child;
pub mod window;
#[cfg(target_os = "linux")]
pub mod window_gtk;
#[cfg(target_os = "macos")]
pub mod window_mac;
#[cfg(windows)]
pub mod window_win;

// The coloured tray icon. macOS draws a template glyph instead (`icon`).
#[cfg(not(target_os = "macos"))]
const TRAY_RGBA: &[u8] = include_bytes!("../../assets/tray-64.rgba");

/// Decoded tray icon pixels, 64 x 64. Kept as raw RGBA rather than a PNG so we
/// do not need an image decoder in the dependency tree.
#[cfg(not(target_os = "macos"))]
pub fn icon_rgba() -> Vec<u8> {
    TRAY_RGBA.to_vec()
}

/// The open settings window: its process, and the line to it.
pub struct Settings(pub child::Child);

impl Settings {
    /// Call `<name>(...args)` in the window. Failures are ignored: the window
    /// may be closing underneath us, and a dropped status message is harmless.
    pub fn push(&self, name: &str, args: &[serde_json::Value]) {
        self.0.push(name, args);
    }

    /// Bring an already open window to the front.
    pub fn raise(&self) {
        self.0.push("raise", &[]);
    }
}

pub use imp::report_open_failure;

/// Tell the user, in a dialog, why the settings window did not appear.
///
/// The settings window is this app's only UI, so a failure recorded nowhere
/// but the log reads as "the app is broken": the user clicks Settings, nothing
/// happens at all, and the log they would need is behind the same tray menu
/// they have just stopped trusting.
fn failure_text(error: &str) -> String {
    let config = crate::config::Config::path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        "Cantos could not open its settings window.\n\n{error}\n\n\
         Hot corners themselves are unaffected and keep working. You can also \
         edit the settings file directly: {config}"
    )
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use crate::util::{pcwstr, wide};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    pub fn report_open_failure(error: &str) {
        let title = wide("Cantos: settings unavailable");
        let text = wide(&super::failure_text(error));
        // No owner window: the tray has none.
        unsafe { MessageBoxW(None, pcwstr(&text), pcwstr(&title), MB_ICONERROR | MB_OK) };
    }
}

// ---------------------------------------------------------------------------
// macOS and Linux
// ---------------------------------------------------------------------------
#[cfg(unix)]
mod imp {
    use crate::log::lwarn;
    use std::process::{Command, Stdio};

    /// AppleScript's alert on a Mac, zenity or kdialog on Linux; just the log
    /// when none of them is there.
    pub fn report_open_failure(error: &str) {
        let text = super::failure_text(error);
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
                        text.clone(),
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
