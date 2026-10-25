<div align="center">

<img src="assets/icon-256.png" width="112" height="112" alt="Cantos">

# Cantos

Hot corners for Windows, macOS and Linux.<br>
Throw the cursor into a corner of the screen and something happens.

<sub><i>canto</i> is Portuguese for <i>corner</i></sub>

</div>

A single executable of about 1 MB, no runtime to install. Sits in the tray (or
the menu bar) using about 2 MB and no measurable CPU.

---

## Install

### Windows

```powershell
scoop bucket add kidchenko https://github.com/kidchenko/scoop-bucket
scoop install kidchenko/cantos
```

winget and Chocolatey packages are on the way.

Or download from [Releases](https://github.com/kidchenko/cantos/releases):
`Cantos-Setup-<version>.exe` to install, or `cantos-<version>-x64.exe` to run
without installing.

The binary is not code-signed, so SmartScreen warns on first run. Click
**More info**, then **Run anyway**.

### macOS

Download `Cantos-<version>.dmg` from
[Releases](https://github.com/kidchenko/cantos/releases) and drag
Cantos to Applications. One universal build for Apple silicon and Intel,
macOS 11 or later.

The app is ad-hoc signed, not notarised, so the first open is refused.
Right-click Cantos.app, choose **Open**, then **Open** again (on macOS 15 and
later: System Settings > Privacy & Security > **Open Anyway**).

On a Mac, Cantos is mostly a front end for macOS's own hot corners (System
Settings > Desktop & Dock > Hot Corners) rather than a second set of them. It
reads them from the Dock, shows them in its window, and writes your changes
back; the Dock does the firing, so those corners keep working even when
Cantos is not running. Change a corner in System Settings and Cantos shows it
within a second, and the other way round. A corner can also take one of
Cantos's own actions, which the Dock cannot do; see [Actions](#actions).

### Linux

Download `cantos-<version>-linux-x86_64.tar.gz` from
[Releases](https://github.com/kidchenko/cantos/releases), then:

```sh
tar xzf cantos-*-linux-x86_64.tar.gz
cd cantos-*-linux-x86_64 && ./install.sh   # into ~/.local, no root
```

It needs GTK 3 and an AppIndicator library, which most desktops already
have. On Debian and Ubuntu:

```sh
sudo apt install libgtk-3-0 libayatana-appindicator3-1 libxdo3
```

**X11 only.** Under Wayland an app cannot read the cursor position over other
apps' windows, by design, so corners fire unreliably at best. Cantos says so in
its log and settings window. Pick an "Xorg" or "X11" session at the login
screen; GNOME and KDE also have built-in hot corners / screen edges for
Wayland.

## Actions

Each platform lists only what it can actually do, plus anything you can launch
yourself.

**Windows**

| | |
|---|---|
| **Windows & desktops** | Task View `Win+Tab` · Show desktop `Win+D` · Minimize all `Win+M` · Previous desktop `Ctrl+Win+Left` · Next desktop `Ctrl+Win+Right` |
| **Shell** | Start menu · Search `Win+S` · Settings `Win+I` · Quick Settings `Win+A` · Notifications `Win+N` · Quick Link menu `Win+X` · Run `Win+R` |
| **Tools** | File Explorer `Win+E` · Screenshot `Win+Shift+S` · Clipboard history `Win+V` · Emoji picker `Win+.` · Project / second screen `Win+P` |
| **Power & screen** | Lock · Screensaver · Turn display off |
| **Custom** | Any program, script, document, or URL, with arguments |

**macOS**

Each corner's menu has two sections. Each corner can also require a key
(⇧ ⌃ ⌥ ⌘), as in System Settings.

| | |
|---|---|
| **macOS** (always work, even with Cantos quit) | Mission Control · Application Windows · Desktop · Notification Centre · Launchpad · Quick Note · Start Screen Saver · Disable Screen Saver · Put Display to Sleep · Lock Screen |
| **Cantos** (while Cantos runs) | Spotlight `⌘Space` · Finder · System Settings · Screenshot · Emoji & Symbols `⌃⌘Space` · Previous desktop `⌃←` · Next desktop `⌃→` · Custom command |

The first section is the Dock's own: Cantos writes the corner into the Dock's
settings and the Dock fires it. A Cantos action clears that corner in the
Dock and Cantos fires it itself, so nothing ever fires twice; its card in the
settings window is tagged CANTOS. The actions with a shortcut beside them
press it, which macOS only allows with the **Accessibility** permission:
macOS asks when you put one on a corner, or at launch if one is already set
and the permission is missing. macOS fires its own corners at the outer
corners of the whole display arrangement, and Cantos does the same.

Saving a change restarts the Dock, because the Dock only reads these
settings when it starts. It is back within a second, with your windows and
Spaces as they were. System Settings does the same thing without the visible
restart, through a private interface that does not work from outside it on
macOS 26.

**Linux (X11)**

| | |
|---|---|
| **Windows & desktops** | Overview `Super` · Show desktop · Previous desktop · Next desktop |
| **Tools** | Files · Screenshot `Print` · Settings |
| **Power & screen** | Lock · Screensaver · Turn display off |
| **Custom** | Any program, script, document, or URL, with arguments |

Show desktop and switching desktops ask the window manager directly (EWMH), so
they work on any desktop that has them. Overview and Screenshot press the key
your desktop binds: GNOME opens Activities on `Super`, KDE its launcher.

## Settings

Left-click the tray icon (on a Mac, choose **Settings…** from the menu bar
icon). The window shows your actual display layout and marks which corners
are live.

Windows and Linux:

| Setting | Default | Notes |
|---|---|---|
| Dwell | 250 ms | How long the cursor rests before a corner fires. Stops overshooting toward a Close button from triggering it. |
| Cooldown | 700 ms | After firing, the corner needs both this delay **and** the cursor to leave before it can fire again. |
| Corner size | 8 px | Edge length of the hot square. |
| Hold a key | off | Require Ctrl, Alt, Shift, or Win (Super on Linux). |
| Pause in fullscreen | on | No corner fires over a game or video. A maximised window is not fullscreen and still works. |
| Pause while dragging | on | Dragging a window into a corner stays a Windows Snap. |
| Start with Windows | off | Per-user `Run` key. No scheduled task, no admin. On macOS a Login Item; on Linux an XDG autostart entry. |

**Multiple monitors.** Three modes: *Outer* (the four corners of the whole
desktop, default), *Every screen*, or *Primary only*. On L-shaped layouts an
outer corner that isn't on any physical display moves to the nearest real
screen corner, so a laptop under a wider monitor still has its bottom corners.
On a Mac the mode applies to Cantos's own corners; the Dock's work as macOS
has them.

Settings live in `config.json`. Edit it by hand if you like; changes apply
within a second, no restart. On a Mac the corners are not in it: they stay in
the Dock's preferences, where System Settings keeps them.

| | settings and log |
|---|---|
| Windows | `%APPDATA%\Cantos` |
| macOS | `~/Library/Application Support/Cantos` |
| Linux | `~/.config/cantos` (or `$XDG_CONFIG_HOME/cantos`) |

## Troubleshooting

Cantos logs to `cantos.log` beside its settings, reachable from the tray menu
via **Open log** even when the settings window won't open. It records every
decision that can make a corner do nothing.

**A corner does nothing.** Look for the `live corners` line. It lists the pixel
rectangles that are hot right now:

```
INFO  live corners: OuterCorners across 1 monitor(s): TopLeft[0,0..8,8] ...
INFO  TopLeft fired: TaskView
INFO  TopRight suppressed: a mouse button is held (window drag / snap)
```

For per-transition detail, set `CANTOS_LOG=debug` before launching.

**Nothing happens while an admin app is focused.** Windows blocks synthetic
keystrokes aimed at a higher-privilege window. That is UIPI, a security
boundary, not a bug. Lock, Screensaver, Turn display off, and Custom command
still work, since none of them synthesises input. Running Cantos elevated also
fixes it, at the cost of admin rights it otherwise doesn't need.

**A Mac corner does nothing.** Corners on a Mac are macOS's own, so check
System Settings > Desktop & Dock > Hot Corners shows the same thing as Cantos,
and whether the corner needs a key held. The log records every change Cantos
writes and every change it sees made elsewhere.

**Nothing fires on Linux.** Check for a Wayland warning in the log; see
[Linux](#linux) above.

**The settings window won't open.** It is a separate process
(`cantos --settings-window`), so a failure to start it says so in a dialog and
in the log. Hot corners keep working, and you can still edit `config.json` by
hand.

**Launching Cantos again does nothing visible.** A second launch tells the
first to show its settings, then exits. A brief flicker is expected.

## Build

### Windows

```powershell
winget install Rustlang.Rustup
rustup default stable-x86_64-pc-windows-msvc
choco install innosetup -y     # only for the installer
```

Open a new terminal afterwards, or `cargo` won't be on PATH.

```powershell
.\build.ps1                 # binary + installer
.\build.ps1 -Run            # ...then launch it
.\build.ps1 -SkipInstaller  # binary only
.\build.ps1 -Test           # ...then run the full test suite (needs admin)
```

Outputs `target\release\cantos.exe` and `dist\Cantos-Setup-0.0.4.exe`.

If `cargo build` fails on `link.exe`, install the MSVC C++ workload:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

### macOS and Linux

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

./build.sh               # tests + release binary, then Cantos.app or a .tar.gz
./build.sh --run         # ...then launch it
./build.sh --universal --dmg   # macOS: arm64 + x86_64 in one .app, in a .dmg
```

On a Mac, an ad-hoc signed build loses the Accessibility permission every
time the binary changes, so `build.sh` clears the stale entry and macOS asks
again. To keep it across rebuilds, sign with a certificate:
`CANTOS_SIGN_IDENTITY="Apple Development: you@example.com (TEAMID)" ./build.sh`
(`security find-identity -v -p codesigning` lists yours).

On Linux, first install the GTK and AppIndicator headers:

```sh
sudo apt install libgtk-3-dev libxdo-dev libayatana-appindicator3-dev
```

## Tests

```powershell
cargo test                                              # corner rules
powershell -ExecutionPolicy Bypass -File tests\e2e.ps1  # corner detection
```

The integration scripts drive the real cursor, so run them on an idle machine.
See [`tests/README.md`](tests/README.md).

## How it works

The settings window is native on every platform: AppKit on macOS, Win32 on
Windows, GTK on Linux, each drawing the same design from the same colour
tokens. It runs as its own process, started when you open it and gone when
you close it, so the tray app itself stays at a few megabytes. The two talk in
JSON lines over the window's stdin and stdout (`src/ui/child.rs`).

Polls the cursor position at ~33 Hz on every platform. On Windows that is
`GetCursorPos` instead of a `WH_MOUSE_LL` hook. A
low-level hook runs inline on the input thread for every mouse move
system-wide, and Windows silently unhooks it if the callback overruns. Polling
costs a few hundred nanoseconds a tick and can't be disabled behind your back.

On Linux it is `XQueryPointer`, one round trip to the X server.

The process is manifested `PerMonitorV2` on Windows, so cursor and monitor
coordinates share one physical-pixel space; X11 works in root-window pixels.

macOS has no detector at all: the Dock already is one. `src/native.rs` reads
and writes its `wvous-*` preferences in `com.apple.dock`, and the detector
modules are not even compiled there.

Every module that talks to the OS has a `mod imp` per platform behind one
interface, so the rest of the code does not know which it is running on.

Start with `src/trigger.rs`. It holds the corner rules and nothing else: no
Win32 handles, no mutex, no actions. It takes a hot spot plus the current
settings and answers "fire this corner" or "not yet", which is what makes the
rules testable, and the same on all three platforms.

## Licence

MIT
