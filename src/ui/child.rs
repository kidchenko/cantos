//! The native settings window, run as a child process: `cantos
//! --settings-window`.
//!
//! A separate process for two reasons. Closing the window ends the process,
//! so every byte the window used goes back to the OS, rather than leaving
//! the tray app carrying a UI toolkit's caches for the rest of the session.
//! And the window gets a main thread and an event loop of its own, instead of
//! sharing tao's with the tray.
//!
//! The two talk in JSON lines over the child's stdin and stdout, in the same
//! vocabulary as the web page: the child sends [`super::page::Msg`]s, exactly
//! what the page posts, and receives `{"push": name, "args": [...]}`, exactly
//! the calls the page receives as `window.__hc.<name>(...)`. Its stderr is
//! left alone; the child logs to the same file as the tray.

use crate::log::{linfo, lwarn};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child as Process, ChildStdin, Command, Stdio};
use std::sync::Mutex;

/// The flag that starts the binary as a settings window instead of a tray app.
pub const FLAG: &str = "--settings-window";

pub struct Child {
    process: Process,
    stdin: Mutex<ChildStdin>,
}

impl Child {
    /// Start the window. `on_message` gets each line the window sends, and
    /// `on_exit` runs once when it closes, from a reader thread either way.
    pub fn spawn<M, E>(on_message: M, on_exit: E) -> Result<Child, String>
    where
        M: Fn(String) + Send + 'static,
        E: FnOnce() + Send + 'static,
    {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not find the Cantos executable: {e}"))?;
        let mut process = Command::new(exe)
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the settings window: {e}"))?;
        let stdin = process
            .stdin
            .take()
            .ok_or("no stdin to the settings window")?;
        let stdout = process
            .stdout
            .take()
            .ok_or("no stdout from the settings window")?;

        let _ = std::thread::Builder::new()
            .name("cantos-settings".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(l) if !l.trim().is_empty() => on_message(l),
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                // EOF: the window closed, or crashed. Either way it is gone.
                on_exit();
            });

        linfo!("native settings window started (pid {})", process.id());
        Ok(Child {
            process,
            stdin: Mutex::new(stdin),
        })
    }

    /// Send one call to the window.
    pub fn push(&self, name: &str, args: &[serde_json::Value]) {
        let line = serde_json::json!({ "push": name, "args": args }).to_string();
        let Ok(mut stdin) = self.stdin.lock() else {
            return;
        };
        if let Err(e) = writeln!(stdin, "{line}").and_then(|_| stdin.flush()) {
            lwarn!("could not reach the settings window ({e})");
        }
    }
}

impl Drop for Child {
    /// The tray dropping its handle (quitting, or switching to the web
    /// window) closes the window too.
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

// The window's side, where a native window exists (see `SettingsUi`).

/// A call from the tray, as the child reads it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(serde::Deserialize, Debug)]
pub struct Push {
    pub push: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// The child's side: send one message to the tray.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn send(msg: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
