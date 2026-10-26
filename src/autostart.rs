//! Run-at-login.
//!
//! * **Windows**: the per-user Run key,
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. It needs no
//!   elevation, it is trivially inspectable in Task Manager's Startup tab, and
//!   uninstalling just leaves one stale value rather than a scheduled task
//!   nobody can find. A Scheduled Task would let us start with a delay, but
//!   costs admin rights and user trust.
//! * **macOS 13+**: `SMAppService`, the API behind System Settings' Login
//!   Items, so Cantos shows up there by name and can be switched off from
//!   either side. macOS 11 and 12 fall back to a LaunchAgent plist.
//! * **Linux**: an XDG autostart entry, `~/.config/autostart/cantos.desktop`,
//!   which every mainstream desktop honours and lists in its startup settings.

pub use imp::{is_enabled, set};

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use crate::util::{exe_path_quoted, pcwstr, wide};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "Cantos";

    /// Closes the key on drop, so early returns cannot leak a handle.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = RegCloseKey(self.0);
                }
            }
        }
    }

    fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<Key> {
        let sub = wide(RUN_KEY);
        let mut hkey = HKEY::default();
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, pcwstr(&sub), None, access, &mut hkey) };
        (rc == ERROR_SUCCESS).then_some(Key(hkey))
    }

    /// Is the Run value present at all? We deliberately do not compare it against
    /// the current executable path: if the user moved the binary we would rather
    /// report "on" and let `set(true)` rewrite the path than silently show "off".
    pub fn is_enabled() -> bool {
        let Some(key) = open(KEY_QUERY_VALUE) else {
            return false;
        };
        let name = wide(VALUE_NAME);
        let rc = unsafe { RegQueryValueExW(key.0, pcwstr(&name), None, None, None, None) };
        rc == ERROR_SUCCESS
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let Some(key) = open(KEY_SET_VALUE) else {
            return Err("could not open the HKCU Run key".into());
        };
        let name = wide(VALUE_NAME);

        if !enabled {
            let rc = unsafe { RegDeleteValueW(key.0, pcwstr(&name)) };
            // Deleting something already absent is the desired end state.
            return if rc == ERROR_SUCCESS || rc.0 == 2 {
                Ok(())
            } else {
                Err(format!(
                    "could not remove the autostart entry (error {})",
                    rc.0
                ))
            };
        }

        let Some(cmd) = exe_path_quoted() else {
            return Err("could not resolve the executable path".into());
        };
        let value = wide(&cmd);
        // REG_SZ length is counted in bytes and must include the terminator.
        let bytes =
            unsafe { std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2) };
        let rc = unsafe { RegSetValueExW(key.0, pcwstr(&name), None, REG_SZ, Some(bytes)) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "could not write the autostart entry (error {})",
                rc.0
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod imp {
    pub fn is_enabled() -> bool {
        if objc2::available!(macos = 13.0) {
            login_item::is_enabled()
        } else {
            launch_agent::is_enabled()
        }
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not resolve the executable path: {e}"))?;
        let exe = exe.to_string_lossy();

        // Run straight from the DMG, or from a quarantined download, the path
        // is a mounted volume or a randomised App Translocation copy that is
        // gone by the next login. Registering it would fail silently later.
        if enabled && (exe.starts_with("/Volumes/") || exe.contains("/AppTranslocation/")) {
            return Err(format!(
                "not enabling open at login from {exe}: move Cantos.app to Applications first"
            ));
        }

        if objc2::available!(macos = 13.0) {
            // A plist left by a macOS 12 install, before an OS upgrade, would
            // launch a second copy at login next to the login item.
            let _ = launch_agent::set(false, &exe);
            login_item::set(enabled)
        } else {
            launch_agent::set(enabled, &exe)
        }
    }

    /// macOS 13+: register the app bundle itself as a login item.
    mod login_item {
        use objc2_service_management::{SMAppService, SMAppServiceStatus};

        /// Only `Enabled` counts. `RequiresApproval` means the user switched
        /// Cantos off in Login Items (or has yet to allow it), and it will not
        /// run at login, so the settings window should not claim otherwise.
        pub fn is_enabled() -> bool {
            unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
        }

        pub fn set(enabled: bool) -> Result<(), String> {
            let service = unsafe { SMAppService::mainAppService() };
            let status = unsafe { service.status() };

            if !enabled {
                if status == SMAppServiceStatus::NotRegistered {
                    return Ok(());
                }
                return unsafe { service.unregisterAndReturnError() }.map_err(|e| {
                    format!(
                        "could not remove the login item: {}",
                        e.localizedDescription()
                    )
                });
            }

            // Registered, but turned off in System Settings. Registering again
            // cannot override the user's choice there, so take them to it.
            if status == SMAppServiceStatus::RequiresApproval {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
                return Ok(());
            }
            unsafe { service.registerAndReturnError() }.map_err(|e| {
                format!("could not add the login item: {}", e.localizedDescription())
            })?;
            if unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
            }
            Ok(())
        }
    }

    /// macOS 11 and 12: a LaunchAgent plist.
    mod launch_agent {
        use std::path::PathBuf;

        const LABEL: &str = "com.kidchenko.cantos";

        fn plist_path() -> Option<PathBuf> {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h)
                    .join("Library/LaunchAgents")
                    .join(format!("{LABEL}.plist"))
            })
        }

        /// Presence of the plist is the switch. As on Windows, we do not
        /// verify the path inside it points at this binary: report "on" and
        /// let `set(true)` rewrite it rather than silently showing "off" after
        /// a move.
        pub fn is_enabled() -> bool {
            plist_path().map(|p| p.exists()).unwrap_or(false)
        }

        pub fn set(enabled: bool, exe: &str) -> Result<(), String> {
            let path = plist_path().ok_or("no HOME in environment")?;

            if !enabled {
                return match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    // Already gone is the desired end state.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(format!("could not remove the LaunchAgent: {e}")),
                };
            }

            // Deliberately not `launchctl load`ed here: RunAtLoad would
            // immediately spawn a second copy on top of the one writing this
            // file. launchd picks the agent up at the next login, which is
            // exactly when it is wanted.
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("could not create LaunchAgents dir: {e}"))?;
            }
            std::fs::write(&path, plist_xml(exe))
                .map_err(|e| format!("could not write the LaunchAgent: {e}"))
        }

        fn plist_xml(exe: &str) -> String {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#,
                exe = xml_escape(exe)
            )
        }

        fn xml_escape(s: &str) -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    fn entry_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("autostart").join("cantos.desktop"))
    }

    /// Presence of the entry is the switch, unless a desktop's startup
    /// settings have switched it off in place with `Hidden=true`, which is how
    /// GNOME Tweaks and KDE record "disabled" without deleting the file.
    pub fn is_enabled() -> bool {
        entry_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|text| !text.lines().any(|l| l.trim() == "Hidden=true"))
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let path = entry_path().ok_or("no HOME in environment")?;

        if !enabled {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                // Already gone is the desired end state.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("could not remove the autostart entry: {e}")),
            };
        }

        let exe = std::env::current_exe()
            .map_err(|e| format!("could not resolve the executable path: {e}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create the autostart dir: {e}"))?;
        }
        std::fs::write(&path, desktop_entry(&exe.to_string_lossy()))
            .map_err(|e| format!("could not write the autostart entry: {e}"))
    }

    fn desktop_entry(exe: &str) -> String {
        // Two layers, per the Desktop Entry spec's "Exec key": inside a quoted
        // argument `"`, `` ` ``, `$` and `\` take a backslash, and then the
        // whole value is a string, whose own escaping doubles every backslash.
        // So `$` is written `\\$`, as the spec's own example has it.
        let quoted: String = exe
            .chars()
            .flat_map(|c| match c {
                '"' | '`' | '$' | '\\' => vec!['\\', c],
                c => vec![c],
            })
            .collect::<String>()
            .replace('\\', "\\\\");
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Cantos\n\
             Comment=Hot corners\n\
             Exec=\"{quoted}\"\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::desktop_entry;

        #[test]
        fn the_exec_line_survives_spaces_and_dollars() {
            let e = desktop_entry("/opt/my apps/$cantos");
            assert!(e.contains(r#"Exec="/opt/my apps/\\$cantos""#), "{e}");
        }
    }
}
