//! Single-instance enforcement, plus a way for the second launch to be useful.
//!
//! Exiting silently when the app is already in the tray is correct but
//! baffling: double-clicking the shortcut appears to do nothing at all. So the
//! second instance signals the first to surface its settings window, then
//! steps aside.
//!
//! Windows carries the signal on a named auto-reset event rather than a
//! broadcast window message. `HWND_BROADCAST` only reaches top-level windows,
//! and this app spends nearly all its life with no window at all, so it would
//! have to hold a hidden top-level window open purely to receive the
//! broadcast. An event needs no window and no message pump.
//!
//! macOS and Linux carry it on a Unix domain socket in the config directory.
//! On macOS that only matters for a second launch from a terminal: opening
//! Cantos.app again from Finder reaches the running copy as an
//! `Event::Reopen` instead (see `main`).

pub use imp::acquire;

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use crate::log::{linfo, lwarn};
    use crate::util::{pcwstr, wide};
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
        INFINITE,
    };

    // "Local\" scopes both objects to the logon session, so two users signed in at
    // once each get their own instance rather than fighting over one.
    const MUTEX_NAME: &str = r"Local\Cantos.SingleInstance";
    const EVENT_NAME: &str = r"Local\Cantos.ShowSettings";

    /// Held by the sole running instance. Owns the event other launches signal.
    pub struct Signal(isize);

    /// Returns `None` if another instance is already running, in which case it has
    /// been asked to show its settings window and this process should exit.
    pub fn acquire() -> Option<Signal> {
        let mutex_name = wide(MUTEX_NAME);

        let existing = unsafe {
            match CreateMutexW(None, true, pcwstr(&mutex_name)) {
                Ok(handle) => {
                    if GetLastError() == ERROR_ALREADY_EXISTS {
                        let _ = CloseHandle(handle);
                        true
                    } else {
                        // Deliberately never closed: the mutex must outlive
                        // `main`, and Windows releases it on process exit anyway.
                        // `HANDLE` is a plain Copy wrapper with no Drop, so simply
                        // dropping the binding keeps the handle open.
                        let _ = handle;
                        false
                    }
                }
                // If the mutex cannot be created at all, assume we are alone
                // rather than refusing to start.
                Err(_) => false,
            }
        };

        if existing {
            linfo!("mutex already held: another instance is running");
            raise();
            return None;
        }

        // Auto-reset, initially unsignalled. Created here rather than in `watch`
        // so the window in which a second launch could find no event to open is as
        // small as possible.
        let event_name = wide(EVENT_NAME);
        match unsafe { CreateEventW(None, false, false, pcwstr(&event_name)) } {
            Ok(handle) => Some(Signal(handle.0 as isize)),
            Err(e) => {
                // Emphatically NOT None. Returning None here would make the app
                // silently refuse to start, which from the outside is
                // indistinguishable from a crash -- the window "blinks and
                // closes". Losing relaunch-to-surface is the lesser failure.
                lwarn!("could not create the show-settings event ({e}); relaunching will not surface the window");
                Some(Signal(0))
            }
        }
    }

    /// Ask the instance that already owns the event to show its settings.
    fn raise() {
        let event_name = wide(EVENT_NAME);
        unsafe {
            match OpenEventW(EVENT_MODIFY_STATE, false, pcwstr(&event_name)) {
                Ok(handle) => {
                    match SetEvent(handle) {
                        Ok(()) => linfo!("signalled the running instance to show settings"),
                        Err(e) => lwarn!("SetEvent failed: {e}"),
                    }
                    let _ = CloseHandle(handle);
                }
                Err(e) => lwarn!("could not open the show-settings event: {e}"),
            }
        }
    }

    impl Signal {
        /// Run `on_signal` every time another launch asks us to surface.
        ///
        /// The thread blocks forever. That is fine: the event loop never returns,
        /// so the process tears this thread down on exit.
        pub fn watch<F>(self, on_signal: F)
        where
            F: Fn() + Send + 'static,
        {
            let raw = self.0;
            if raw == 0 {
                lwarn!("no show-settings event; relaunch-to-surface is disabled");
                return;
            }
            let _ = std::thread::Builder::new()
                .name("cantos-instance".into())
                .spawn(move || {
                    let handle = HANDLE(raw as *mut std::ffi::c_void);
                    linfo!("listening for relaunch signals");
                    loop {
                        // WAIT_OBJECT_0 is the only value we want to act on; a
                        // failure here means the handle is gone and looping would
                        // spin at full tilt.
                        let rc = unsafe { WaitForSingleObject(handle, INFINITE) };
                        if rc.0 != 0 {
                            lwarn!(
                                "relaunch listener stopping (WaitForSingleObject returned {})",
                                rc.0
                            );
                            break;
                        }
                        linfo!("relaunch signal received; opening settings");
                        on_signal();
                    }
                });
        }
    }
}

// ---------------------------------------------------------------------------
// macOS and Linux
// ---------------------------------------------------------------------------
#[cfg(unix)]
mod imp {
    use crate::log::{linfo, lwarn};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    fn socket_path() -> Option<PathBuf> {
        // Alongside the config, well within the ~104-char sun_path limit.
        crate::config::Config::dir().map(|d| d.join("cantos.sock"))
    }

    /// Held by the sole running instance. Owns the listening socket; `None`
    /// means single-instance could not be set up but the app should run anyway.
    pub struct Signal(Option<UnixListener>);

    /// Returns `None` if another instance is already running, in which case it
    /// has been asked to show its settings window and this process should exit.
    pub fn acquire() -> Option<Signal> {
        let Some(path) = socket_path() else {
            // No config dir: assume we are alone rather than refusing to start.
            lwarn!(
                "no socket path; single-instance disabled, relaunching will not surface settings"
            );
            return Some(Signal(None));
        };

        // Someone already listening? Then we are the second launch: poke them
        // and step aside.
        if raise(&path) {
            linfo!("show-settings socket is live; signalled the running instance");
            return None;
        }

        // Nothing answered: a stale socket file, or none at all. Clear it and
        // take the socket ourselves.
        let _ = std::fs::remove_file(&path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match UnixListener::bind(&path) {
            Ok(listener) => Some(Signal(Some(listener))),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Lost a startup race between our raise() and bind(): the
                // winner is up now, so behave like the second launch after all.
                let _ = raise(&path);
                linfo!("lost the startup race; asked the winner to show settings, exiting");
                None
            }
            Err(e) => {
                // Emphatically NOT None; see the Windows twin.
                lwarn!("could not bind the show-settings socket ({e}); relaunching will not surface the window");
                Some(Signal(None))
            }
        }
    }

    /// Connect to an existing instance's socket and nudge it. Returns true if
    /// an instance was there to nudge.
    fn raise(path: &Path) -> bool {
        match UnixStream::connect(path) {
            Ok(mut s) => {
                // The connection is the message; a byte just makes the read on
                // the other side return promptly.
                let _ = s.write_all(b"1");
                true
            }
            Err(_) => false,
        }
    }

    impl Signal {
        /// Run `on_signal` every time another launch asks us to surface.
        ///
        /// The thread blocks on `accept` forever. That is fine: the event loop
        /// never returns, so the process tears this thread down on exit.
        pub fn watch<F>(self, on_signal: F)
        where
            F: Fn() + Send + 'static,
        {
            let Some(listener) = self.0 else {
                lwarn!("no show-settings socket; relaunch-to-surface is disabled");
                return;
            };
            let _ = std::thread::Builder::new()
                .name("cantos-instance".into())
                .spawn(move || {
                    linfo!("listening for relaunch signals");
                    for stream in listener.incoming() {
                        match stream {
                            Ok(mut s) => {
                                // Drain the poke byte; its arrival is the whole
                                // message.
                                let mut buf = [0u8; 8];
                                let _ = s.read(&mut buf);
                                linfo!("relaunch signal received; opening settings");
                                on_signal();
                            }
                            Err(e) => {
                                lwarn!("relaunch accept failed ({e}); listener stopping");
                                break;
                            }
                        }
                    }
                });
        }
    }
}
