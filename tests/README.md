# Tests

## `common.ps1`

Not a test. Shared helpers the scripts dot-source, chiefly the config
backup/restore pair.

`Backup-HcConfig` **refuses to run if a backup from a previous run is still
present**. That state means an earlier run died before restoring, so the live
config is already a test config and the stale backup is the only copy of the
user's real settings, and backing up again would overwrite it with a throwaway.
This has happened. The script prints the two commands that recover it.

## `cargo test`

Unit tests for the two modules worth testing in isolation:

- **`src/trigger.rs`**: the corner rules. Dwell, cooldown, re-arm,
  corner-to-corner sliding, and every veto. Pure logic, no Win32, no timing
  flakiness: elapsed time is simulated by constructing `Instant`s in the past,
  and desktop state comes from a `Fake` implementing the `Desktop` trait. That
  fake is the only reason the vetoes are testable at all: during `cargo test`
  no key is ever held and nothing is ever fullscreen, so the real
  implementation could only exercise the "nothing is in the way" path.
- **`src/desktop.rs`**: the rectangle and window-style arithmetic behind
  fullscreen detection, pinned to measurements taken on real hardware.

The one that matters most is `resting_in_a_fired_corner_does_not_refire`;
without that transition the action would repeat 33 times a second while the
cursor sits still.

Two groups are regression tests for bugs that only show up on hardware most
machines do not have, which is exactly why they are pinned here rather than in
the harness:

- `a_maximised_window_with_no_taskbar_looks_like_fullscreen` records the
  measured rectangles behind the maximised-window false positive. See
  `fullscreen.ps1` below.
- `a_second_displays_top_left_is_not_the_one_that_just_fired` covers
  `EveryMonitor` mode, where every display contributes its own `TopLeft`. Keyed
  on the corner name alone, one screen's corner stayed disarmed after a
  different screen's had fired.

## `e2e.ps1`

End-to-end test of the whole corner pipeline: config load, monitor geometry,
cursor polling, dwell, and action dispatch.

```powershell
cargo build --release
powershell -ExecutionPolicy Bypass -File tests\e2e.ps1
```

It backs up your real `config.json`, swaps in a config whose top-left corner
runs a marker-writing command, drives the cursor, and restores everything
afterwards. It uses a marker file rather than Task View so the test is
observable without hijacking the desktop.

Two things this harness gets right that are easy to get wrong:

- **It sets `PER_MONITOR_AWARE_V2` before touching any coordinate.** Without
  that, Windows virtualises the test process's cursor coordinates and
  `SetCursorPos` lands somewhere other than asked on any scaled display. The
  app is manifested per-monitor-v2, so an unaware harness and the app disagree
  about where the screen corners are, and every test fails for a reason that
  has nothing to do with the app.
- **It holds the cursor in place across the dwell window** and reports drift.
  Physical mouse input from whoever is at the machine will otherwise pull the
  cursor out of the corner mid-dwell, and the corner correctly does not fire.

Run it on an idle machine; it moves the cursor and restores it at the end.

## `drag.ps1`

Verifies that a corner does not fire while a mouse button is held, which is
what keeps Windows Snap working: dragging a window into a corner should snap
it and nothing else.

```powershell
powershell -ExecutionPolicy Bypass -File tests\drag.ps1
```

The middle case is the one that matters: after releasing the button while
still parked in the corner, it must *still* not fire. Suppression disarms the
corner rather than merely deferring it, because deferring would fire the
action at the exact moment you release to complete the snap.

It synthesises a **middle**-click rather than a left-click. The suppression
code treats all three buttons identically, so the path under test is the same,
and a synthetic middle-click at a screen corner cannot drag, activate, or
dismiss anything the way a left-click could.

## `fullscreen.ps1`

Covers the suppression path end to end, which nothing used to: every other
script here sets `suppressFullscreen = false`.

```powershell
powershell -ExecutionPolicy Bypass -File tests\fullscreen.ps1
```

Three cases:

| Probe window | Corner must |
|---|---|
| Borderless, sized to the monitor | **not** fire (a game or video player) |
| Ordinary maximised | fire (it is not fullscreen) |
| Borderless **and** maximised | **not** fire (still a fullscreen app) |

**What this does not prove.** Measured on a normal desktop,
`SHQueryUserNotificationState` returns `QUNS_BUSY` for *any* borderless window
covering the monitor: cases 1 and 3 both. `fullscreen_active()` consults that
state before the geometry, so it short-circuits and neither `IsZoomed` nor the
frame check runs for those two. They are real end-to-end assertions, but they
do **not** isolate `has_frame`; the unit test
`a_borderless_window_is_not_framed` does that.

Case 2 is the only one that reaches the geometry path.

The second is the original regression. The check used to be pure geometry:
does the foreground window cover its monitor? But `GetWindowRect` reports the
frame including the invisible resize border, so a maximised window overhangs
its monitor by ~8px on every edge it can grow into. Wherever the work area
equals the monitor rect (a second display with no taskbar, the Windows 11
default, or an auto-hide taskbar), that overhang alone read as fullscreen, and
every corner on every display went quiet while a maximised window had focus.

Note that on a primary display **with** a visible taskbar this case passes
either way, since there the work area is genuinely shorter than the monitor.
The geometry that actually broke it is pinned in the Rust unit tests
(`a_maximised_window_with_no_taskbar_looks_like_fullscreen`) instead. To see
the real thing, maximise a window on a second display that has no taskbar and
check that corners still fire.

## `actions.ps1`

Fires the built-in actions end to end: corner -> dwell -> `SendInput` -> the
shell reacting. Every other script here uses the Custom action because it is
observable via a marker file, which conveniently avoids the key-synthesis path
that Task View and Show desktop actually depend on. This one covers it.

```powershell
powershell -ExecutionPolicy Bypass -File tests\actions.ps1
```

It opens Task View and minimises your windows, then restores both.

**Run it from a non-elevated shell, with no elevated window focused.** It skips
otherwise, and the reason is worth understanding. Windows discards synthesised
input aimed at a higher-integrity window, so with an elevated window in the
foreground every action falls back to shell automation. Task View survives
that (`IShellDispatch5::WindowSwitcher` genuinely works), but `ToggleDesktop`
returns success and does nothing, so Show desktop reads as broken when the app
behaved exactly as designed. That is a real documented limitation, not
something this test can assert on, so it declines to run rather than cry wolf.

The foreground is sampled **throughout** the run, not just checked at the
start, because focus drifts. Launched from an elevated shell with a browser
focused, the Task View case passes and then the foreground falls back to the
elevated terminal before the Show desktop case, producing a `FAIL` for a
build that is entirely correct. Observed, not theorised; that is why the check
is a running sample rather than a precondition.

`Win`+`D` is a **toggle**, so the test forces a known state before asserting.
Without that, firing the corner while the desktop is already showing restores
windows instead of hiding them, and a working action reads as a failure.

Lock, Screensaver and Display off are deliberately not fired: same dispatch
code, but they would leave the machine locked or the screen dark.
