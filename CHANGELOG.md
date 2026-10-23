# Changelog

Notable changes to Cantos. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases are tagged `vX.Y.Z`, which is what triggers the release workflow.

## [Unreleased]

Nothing yet.

## [0.1.0] - 2026-10-23

### Added

- **macOS.** Cantos runs in the menu bar on macOS 11 and later, as one
  universal `.dmg` for Apple silicon and Intel. There it is mostly a front
  end for macOS's own hot corners: it reads them from the Dock, edits them
  with the same actions and per-corner keys as System Settings, and keeps up
  with changes made there, so those corners work even with Cantos quit. A
  corner can instead take a Cantos action the Dock cannot do (Spotlight,
  Finder, switching desktops, a custom command), which Cantos fires while it
  runs. Run-at-login is a Login Item.
- **Linux (X11).** Cantos runs in the tray on X11 desktops, from a tarball
  that installs into `~/.local` without root. Corners can open the overview,
  show the desktop, switch desktops, take a screenshot, open Files or
  Settings, lock, start the screensaver, turn the display off, or run
  anything. Wayland sessions are detected and explained, since Wayland does
  not let an app read the cursor over other windows.
- The settings window lists only the actions the platform it runs on can
  perform, under that platform's names.

### Changed

- **The settings window is native** on every platform: AppKit on macOS,
  Win32 on Windows, GTK on Linux, keeping the same design. It runs as its own
  process, so closing it gives all of its memory back: about 12 MB for the
  tray on a Mac afterwards, where the old window left around 50 MB behind.
- **No more WebView2 or WebKit.** The web settings page is gone, so Windows no
  longer needs the WebView2 runtime and Linux no longer needs WebKitGTK.

## [0.0.2] - 2026-09-12

### Fixed

- Uninstalling left the WebView2 profile behind in
  `%LOCALAPPDATA%\Cantos`, several megabytes of Chromium cache that nothing
  ever removed. The installer never created that folder, so Inno did not know
  to clean it up. It now does, along with the install directory itself.

  Settings in `%APPDATA%\Cantos` are still kept on purpose, so reinstalling
  brings your corners back.

## [0.0.1] - 2026-09-12

First release. Everything below is new, so this entry describes the product
rather than listing changes against a version nobody has.

### Corners

- Twenty-one built-in actions across Windows & desktops, shell, tools, and
  power/screen, plus a Custom action that runs any program, script, document,
  or URL with arguments.
- Configurable **dwell** (250 ms default). A corner does not fire the instant
  you touch it, so overshooting toward a window's Close button is harmless.
- **Cooldown and leaving.** After firing, a corner stays disarmed until both
  the cooldown elapses and the cursor leaves it; otherwise parking the mouse
  in a corner would repeat the action.
- Optional **modifier key** requirement (Ctrl, Alt, Shift, or Win). Any
  modifier you are physically holding is released before the app synthesises
  its own shortcut, so `Win`+`Tab` does not arrive as `Ctrl`+`Win`+`Tab`.
- **Suppressed while dragging** (on by default), so dragging a window into a
  corner stays a Windows Snap and nothing else. Suppression disarms the corner
  rather than deferring it, so releasing the button to complete the snap does
  not fire the action at that exact moment.
- **Suppressed over fullscreen** (on by default), via both the shell
  notification state and the foreground window's geometry. An ordinary
  maximised window is explicitly not fullscreen.

### Multiple monitors

- Three modes: **Outer** (the four corners of the whole virtual desktop,
  the default), **Every screen**, and **Primary only**.
- On L-shaped arrangements, an outer corner that does not sit on any physical
  display is dropped rather than published as a corner that can never fire.
- Each display's corners are tracked separately, so one screen's top-left is
  never confused with another's.

### Settings and tray

- Tray icon with an enable/disable toggle, and a settings window that draws
  your real display arrangement and highlights which corners are live.
- The settings window is a WebView2 control built on demand and torn down
  completely on close, which is what keeps the app at ~2 MB private while
  idle. If the WebView2 runtime is missing, the app says so in a dialog and
  offers the download page instead of silently doing nothing.
- Settings live in `%APPDATA%\Cantos\config.json`, written via a temp file
  and rename. The file is watched, so a hand edit takes effect within a second
  rather than being ignored until restart and then overwritten.
- Optional run-at-login via the per-user `Run` key: no scheduled task, no
  elevation, and visible in Task Manager's Startup tab.
- Single instance: launching the app again surfaces the settings window.

### Diagnostics

- A log at `%APPDATA%\Cantos\cantos.log`, reachable from the tray even
  when the settings window is the thing refusing to open. It records every
  decision that can make a corner do nothing, because from the outside they
  all look identical. `CANTOS_LOG=debug` adds per-transition detail.
- Rotates at 1 MB, keeping one previous generation.

### Known limitations

- **The binary is not code-signed.** Windows SmartScreen will warn on first
  run until the download accumulates reputation.
- Windows discards synthesised keystrokes aimed at a window owned by an
  elevated process, so keyboard-driven actions do nothing while an
  administrator window has focus. Actions that do not synthesise input (Lock,
  Screensaver, Turn display off, Custom command) are unaffected, and the log
  names the cause when it happens. See the README for the workarounds.
- x64 only. Windows 10 or later.

[Unreleased]: https://github.com/kidchenko/windows-cantos/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kidchenko/windows-cantos/compare/v0.0.2...v0.1.0
[0.0.2]: https://github.com/kidchenko/windows-cantos/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/kidchenko/windows-cantos/releases/tag/v0.0.1
