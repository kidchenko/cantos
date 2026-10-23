//! Integrity levels, and whether synthetic input can reach the focused window.
//!
//! Windows silently discards injected input aimed at a window owned by a
//! process at a higher integrity level than the sender. "Silently" is the
//! problem: `SendInput` returns success and reports every event as inserted,
//! so there is no error to check. Corner detection works, the action runs,
//! and nothing happens, which is indistinguishable from a broken app.
//!
//! So we ask the question directly instead: is the foreground window owned by
//! something that outranks us? That turns an invisible failure into a log
//! line that names the cause.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Well-known mandatory-label RIDs. Higher is more privileged.
const LOW: u32 = 0x1000;
const MEDIUM: u32 = 0x2000;
const HIGH: u32 = 0x3000;
const SYSTEM: u32 = 0x4000;

pub fn describe(rid: u32) -> &'static str {
    match rid {
        r if r >= SYSTEM => "System",
        r if r >= HIGH => "High (elevated)",
        r if r >= MEDIUM => "Medium",
        r if r >= LOW => "Low",
        _ => "Untrusted",
    }
}

/// Integrity RID of an open process handle.
fn level_of(process: HANDLE) -> Option<u32> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;

        // Two calls: the first only to learn the buffer size.
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseHandle(token);
            return None;
        }

        // Backed by `u64` rather than `u8`: TOKEN_MANDATORY_LABEL holds a SID
        // pointer, so reading one out of a byte vector (which is only
        // byte-aligned) is undefined behaviour even where it happens to work.
        let mut buf = vec![0u64; needed.div_ceil(8) as usize];
        let ok = GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut needed,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }

        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let count = GetSidSubAuthorityCount(label.Label.Sid);
        if count.is_null() || *count == 0 {
            return None;
        }
        // The integrity RID is the last sub-authority.
        Some(*GetSidSubAuthority(label.Label.Sid, (*count - 1) as u32))
    }
}

pub fn own_level() -> Option<u32> {
    level_of(unsafe { GetCurrentProcess() })
}

fn level_of_pid(pid: u32) -> Option<u32> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let lvl = level_of(h);
        let _ = CloseHandle(h);
        lvl
    }
}

/// `Some(reason)` when a synthesised keystroke is going to be thrown away.
///
/// Deliberately returns the explanation rather than a bool: the only useful
/// thing to do with this is tell the user, and the message needs both levels
/// to make sense.
pub fn keystroke_will_be_discarded() -> Option<String> {
    let ours = own_level()?;
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let theirs = level_of_pid(pid)?;

    (theirs > ours).then(|| {
        format!(
            "the focused window runs at {} and Cantos at {}, so Windows will discard the \
             keystroke. Actions that do not synthesise input (Lock, Screensaver, Turn display \
             off, Custom command) still work; to drive elevated windows, Cantos has to run \
             elevated too.",
            describe(theirs),
            describe(ours)
        )
    })
}

// GetTokenInformation is generated with a slightly awkward signature; wrap it
// once here so the call sites above stay readable.
#[allow(non_snake_case)]
unsafe fn GetTokenInformation(
    token: HANDLE,
    class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
    info: Option<*mut core::ffi::c_void>,
    len: u32,
    ret: *mut u32,
) -> windows::core::Result<()> {
    windows::Win32::Security::GetTokenInformation(token, class, info, len, ret)
}
