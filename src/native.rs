//! macOS: the system's own hot corners, read and written in place.
//!
//! On a Mac, Cantos does not detect corners itself. macOS already does, in the
//! Dock, and doing it twice would fire twice. Cantos is a front end for the
//! Dock's settings instead: the same four corners and the same actions as
//! System Settings > Desktop & Dock > Hot Corners, with the menu bar icon and
//! the settings window the other platforms have.
//!
//! The Dock keeps them in its preferences domain, `com.apple.dock`:
//!
//! | key                   | value                                     |
//! |-----------------------|-------------------------------------------|
//! | `wvous-tl-corner` ... | the action, as a number (see [`CODES`])   |
//! | `wvous-tl-modifier`   | the key to hold, as an event-flag mask    |
//!
//! The Dock reads them when it starts, so a change is written and then the
//! Dock is restarted, as `defaults write com.apple.dock ...; killall Dock`
//! would (see [`restart_dock`] for why not `killall` itself). It is back within a second and keeps windows, Spaces and
//! running apps as they were. `CoreDockSetExposeCornerActionWithModifier`
//! would avoid the restart, but on macOS 26 it neither applies nor stores the
//! change, so it is not used.
//!
//! As with run-at-login, the OS is the only record: these corners are never
//! copied into `config.json`, so System Settings and Cantos cannot disagree.
//!
//! # Cantos's own actions
//!
//! A corner can also have an action the Dock cannot do (Spotlight, a custom
//! command...). That one lives in `config.json`, the Dock's corner is left
//! empty, and Cantos's own detector fires it while Cantos runs. A corner is
//! only ever one or the other: [`apply`] keeps it so, and [`reconcile`] lets
//! System Settings win if both end up set.

use crate::config::{Action, Config, Modifier};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSNumber, NSString};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use traykit::{linfo, lwarn};

const DOMAIN: &str = "com.apple.dock";
const KEYS: [&str; 4] = ["tl", "tr", "bl", "br"];

/// The Dock's action numbers. 0 and 1 both mean "nothing" (System Settings
/// writes 1); 7 was Dashboard and no longer exists.
const CODES: [(i64, Action); 11] = [
    (1, Action::None),
    (2, Action::TaskView),
    (3, Action::AppWindows),
    (4, Action::ShowDesktop),
    (5, Action::Screensaver),
    (6, Action::DisableScreenSaver),
    (10, Action::DisplayOff),
    (11, Action::Launchpad),
    (12, Action::Notifications),
    (13, Action::LockWorkstation),
    (14, Action::QuickNote),
];

/// The modifier masks System Settings offers, one key at a time.
const MODIFIERS: [(i64, Modifier); 5] = [
    (0, Modifier::None),
    (1 << 17, Modifier::Shift),
    (1 << 18, Modifier::Ctrl),
    (1 << 19, Modifier::Alt),
    (1 << 20, Modifier::Win),
];

/// One corner as the Dock has it, in Cantos's own terms. Indexed like
/// [`crate::config::Corner::index`].
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NativeCorner {
    pub action: Action,
    pub modifier: Modifier,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFPreferencesCopyAppValue(key: *const c_void, app: *const c_void) -> *mut c_void;
    fn CFPreferencesSetAppValue(key: *const c_void, value: *const c_void, app: *const c_void);
    fn CFPreferencesAppSynchronize(app: *const c_void) -> bool;
}

/// The four corners as the Dock has them right now.
///
/// An action number Cantos does not know reads as nothing. That is safe:
/// [`write`] only touches corners whose value actually changed, so such a
/// corner is left exactly as it was unless the user picks something for it.
pub fn read() -> [NativeCorner; 4] {
    let domain = NSString::from_str(DOMAIN);
    // Drop this process's cached copy, so an edit made in System Settings
    // since the last read is seen.
    unsafe { CFPreferencesAppSynchronize(cf(&*domain)) };
    KEYS.map(|k| {
        let code = number(&domain, &format!("wvous-{k}-corner")).unwrap_or(0);
        let mask = number(&domain, &format!("wvous-{k}-modifier")).unwrap_or(0);
        NativeCorner {
            action: action_of(code),
            modifier: modifier_of(mask),
        }
    })
}

/// Make the Dock's corners `wanted`, restarting it if anything changed.
pub fn write(wanted: &[NativeCorner; 4]) -> Result<(), String> {
    let current = read();
    let domain = NSString::from_str(DOMAIN);
    let mut changed = false;
    for (i, (want, have)) in wanted.iter().zip(&current).enumerate() {
        if want == have {
            continue;
        }
        let code = code_of(&want.action)
            .ok_or_else(|| format!("{:?} is not a macOS hot corner action", want.action))?;
        // A modifier on an empty corner means nothing; store none, as System
        // Settings does.
        let mask = if code == 1 { 0 } else { mask_of(want.modifier) };
        set(&domain, &format!("wvous-{}-corner", KEYS[i]), code);
        set(&domain, &format!("wvous-{}-modifier", KEYS[i]), mask);
        linfo!("{} corner: {:?} {:?}", KEYS[i], want.action, want.modifier);
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    if !unsafe { CFPreferencesAppSynchronize(cf(&*domain)) } {
        return Err("could not save the Dock's preferences".into());
    }
    restart_dock()
}

/// Run `on_change` whenever the corners change outside Cantos, in System
/// Settings or with `defaults`, so an open settings window keeps up. One
/// preferences read a second.
pub fn watch<F: Fn() + Send + 'static>(on_change: F) {
    let _ = std::thread::Builder::new()
        .name("cantos-native".into())
        .spawn(move || {
            let mut last = read();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let now = read();
                if now != last {
                    linfo!("hot corners changed outside Cantos: {now:?}");
                    last = now;
                    on_change();
                }
            }
        });
}

/// Restart the Dock so it reads the new corners.
///
/// `launchctl kickstart -k` stops it and starts it again as one step. Plain
/// `killall Dock` leaves the second half to launchd, which on an idle session
/// (screen asleep, say) can leave the Dock down until something asks for it:
/// measured, it was still not running seconds later.
/// The four corners as they actually behave: the Dock's where it has one,
/// otherwise Cantos's own from `cfg`.
pub fn effective(cfg: &Config) -> [NativeCorner; 4] {
    let dock = read();
    std::array::from_fn(|i| {
        let own = &cfg.corners[i];
        if dock[i].action == Action::None && !own.is_none() {
            NativeCorner {
                action: own.clone(),
                modifier: cfg.corner_keys[i],
            }
        } else {
            dock[i].clone()
        }
    })
}

/// Set the corners to `wanted`, each to whichever side can do its action:
/// the Dock for its own, `cfg` (which the caller saves) for Cantos's. The
/// Dock restarts only if one of its corners actually changed.
pub fn apply(cfg: &mut Config, wanted: &[NativeCorner; 4]) -> Result<(), String> {
    let dock: [NativeCorner; 4] = std::array::from_fn(|i| {
        let w = &wanted[i];
        if is_dock_action(&w.action) {
            cfg.corners[i] = Action::None;
            cfg.corner_keys[i] = Modifier::None;
            w.clone()
        } else {
            cfg.corners[i] = w.action.clone();
            cfg.corner_keys[i] = w.modifier;
            NativeCorner {
                action: Action::None,
                modifier: Modifier::None,
            }
        }
    });
    write(&dock)
}

/// If the Dock has gained a corner Cantos also holds (set in System Settings,
/// say), the Dock wins and Cantos lets go. Returns whether `cfg` changed.
pub fn reconcile(cfg: &mut Config) -> bool {
    let dock = read();
    let mut changed = false;
    for (i, d) in dock.iter().enumerate() {
        if d.action != Action::None && !cfg.corners[i].is_none() {
            linfo!(
                "corner {} now set in System Settings; Cantos's {:?} gives way",
                KEYS[i],
                cfg.corners[i]
            );
            cfg.corners[i] = Action::None;
            cfg.corner_keys[i] = Modifier::None;
            changed = true;
        }
    }
    changed
}

/// Whether the Dock can put `action` on a corner. Nothing counts as no.
pub fn is_dock_action(action: &Action) -> bool {
    *action != Action::None && code_of(action).is_some()
}

fn restart_dock() -> Result<(), String> {
    let service = format!("gui/{}/com.apple.Dock.agent", unsafe { libc::getuid() });
    let kicked = std::process::Command::new("/bin/launchctl")
        .args(["kickstart", "-k", &service])
        .status();
    if matches!(kicked, Ok(s) if s.success()) {
        linfo!("restarted the Dock to apply the change");
        return Ok(());
    }
    lwarn!("launchctl kickstart failed ({kicked:?}); falling back to killall");
    match std::process::Command::new("/usr/bin/killall")
        .arg("Dock")
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("saved, but restarting the Dock failed ({s})")),
        Err(e) => Err(format!("saved, but could not restart the Dock: {e}")),
    }
}

fn action_of(code: i64) -> Action {
    CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| {
            if code > 1 {
                lwarn!("unknown hot corner action {code}; showing it as nothing");
            }
            Action::None
        })
}

fn code_of(action: &Action) -> Option<i64> {
    CODES.iter().find(|(_, a)| a == action).map(|(c, _)| *c)
}

/// A mask holding more than one key (only possible through `defaults`) is
/// shown as its first key.
fn modifier_of(mask: i64) -> Modifier {
    MODIFIERS
        .iter()
        .skip(1)
        .find(|(m, _)| mask & m != 0)
        .map(|(_, k)| *k)
        .unwrap_or(Modifier::None)
}

fn mask_of(modifier: Modifier) -> i64 {
    MODIFIERS
        .iter()
        .find(|(_, k)| *k == modifier)
        .map(|(m, _)| *m)
        .unwrap_or(0)
}

/// NSString and NSNumber are toll-free bridged to CFString and CFNumber.
///
/// Bounded by `Message` so that only an object itself fits: handed a
/// `&Retained<_>` instead, an unbounded version took the address of the
/// smart pointer and crashed CoreFoundation.
fn cf<T: objc2::Message>(object: &T) -> *const c_void {
    object as *const T as *const c_void
}

fn number(domain: &NSString, key: &str) -> Option<i64> {
    let key = NSString::from_str(key);
    let raw = unsafe { CFPreferencesCopyAppValue(cf(&*key), cf(domain)) };
    // "Copy": the reference is ours, and `from_raw` releases it on drop.
    let value: Retained<AnyObject> = unsafe { Retained::from_raw(raw.cast()) }?;
    value.downcast_ref::<NSNumber>().map(|n| n.as_i64())
}

fn set(domain: &NSString, key: &str, value: i64) {
    let key = NSString::from_str(key);
    let value = NSNumber::new_i64(value);
    unsafe { CFPreferencesSetAppValue(cf(&*key), cf(&*value), cf(domain)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_through_its_code() {
        for (code, action) in CODES {
            assert_eq!(action_of(code), action);
            assert_eq!(code_of(&action), Some(code));
        }
    }

    #[test]
    fn zero_and_unknown_codes_read_as_nothing() {
        assert_eq!(action_of(0), Action::None);
        assert_eq!(action_of(7), Action::None);
    }

    #[test]
    fn modifiers_round_trip_and_combinations_read_as_their_first_key() {
        for (mask, m) in MODIFIERS {
            assert_eq!(modifier_of(mask), m);
            assert_eq!(mask_of(m), mask);
        }
        assert_eq!(modifier_of((1 << 17) | (1 << 20)), Modifier::Shift);
    }

    #[test]
    fn actions_the_dock_cannot_do_have_no_code() {
        assert_eq!(code_of(&Action::Search), None);
        assert_eq!(code_of(&Action::DesktopLeft), None);
    }
}
