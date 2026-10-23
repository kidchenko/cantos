//! Ask Explorer to do things, rather than faking the keystrokes a user would
//! press to do them.
//!
//! This is the difference between an action that works and one that silently
//! does nothing. Synthetic input aimed at a window owned by a higher
//! integrity process is discarded by Windows without an error, so with a
//! terminal or Task Manager running as administrator in the foreground,
//! `SendInput` is simply thrown away.
//!
//! Shell automation is not input. `Shell.Application` is a COM server hosted
//! by Explorer, which runs at medium integrity like we do, and a posted
//! message to the taskbar goes to Explorer's window rather than to whatever
//! happens to have focus. Neither cares what is in the foreground, so neither
//! is affected. Measured, with an elevated window focused:
//!
//! ```text
//! SendInput Win+Tab              discarded
//! IShellDispatch5::WindowSwitcher  worked
//! IShellDispatch4::ToggleDesktop   worked
//! PostMessage(tray, SC_TASKLIST)   worked
//! ms-settings: URI                 worked
//! ```
//!
//! Every one of these returns a bool so the caller can fall back to a
//! keystroke when no shell equivalent exists or the call fails.

use crate::util::{pcwstr, wide};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{IShellDispatch5, Shell, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SC_TASKLIST, SW_SHOWNORMAL, WM_SYSCOMMAND,
};

/// The caller's thread must already be in an initialised apartment; see
/// `actions::fire`, which does that once per dispatch.
fn dispatch() -> Option<IShellDispatch5> {
    unsafe { CoCreateInstance::<_, IShellDispatch5>(&Shell, None, CLSCTX_ALL).ok() }
}

/// Win+Tab's equivalent.
pub fn window_switcher() -> bool {
    dispatch().is_some_and(|d| unsafe { d.WindowSwitcher().is_ok() })
}

/// Win+D's equivalent: a toggle, exactly like the shortcut.
pub fn toggle_desktop() -> bool {
    dispatch().is_some_and(|d| unsafe { d.ToggleDesktop().is_ok() })
}

/// Win+M's equivalent. Unlike `toggle_desktop` this does not restore.
pub fn minimize_all() -> bool {
    dispatch().is_some_and(|d| unsafe { d.MinimizeAll().is_ok() })
}

/// Win+R's equivalent.
pub fn run_dialog() -> bool {
    dispatch().is_some_and(|d| unsafe { d.FileRun().is_ok() })
}

/// Open the Start menu by asking the taskbar, rather than sending Ctrl+Esc.
pub fn start_menu() -> bool {
    unsafe {
        let class = wide("Shell_TrayWnd");
        let Ok(tray) = FindWindowW(pcwstr(&class), PCWSTR::null()) else {
            return false;
        };
        if tray.is_invalid() {
            return false;
        }
        PostMessageW(
            Some(tray),
            WM_SYSCOMMAND,
            WPARAM(SC_TASKLIST as usize),
            LPARAM(0),
        )
        .is_ok()
    }
}

/// Launch a shell URI (`ms-settings:`, `ms-screenclip:`, …) or an executable.
///
/// `ShellExecuteW` signals failure with a return value of 32 or less rather
/// than through an error code.
pub fn launch(target: &str) -> bool {
    let verb = wide("open");
    let file = wide(target);
    let rc = unsafe {
        ShellExecuteW(
            None,
            pcwstr(&verb),
            pcwstr(&file),
            PCWSTR::null(),
            None,
            SW_SHOWNORMAL,
        )
    };
    rc.0 as isize > 32
}
