# End-to-end test of the corner pipeline: config load -> monitor geometry ->
# cursor polling -> dwell -> action dispatch.
#
# Uses a Custom action that touches a marker file rather than Task View, so
# the test is observable and does not hijack the desktop.

$ErrorActionPreference = 'Continue'
. "$PSScriptRoot\common.ps1"

$proj    = Split-Path $PSScriptRoot -Parent
$scratch = $PSScriptRoot
$marker  = Join-Path $scratch 'fired.txt'
$cfg     = Get-HcConfigPath
$backup  = Join-Path $scratch 'config.backup.json'

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class C {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out P p);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool EnumDisplayMonitors(IntPtr h, IntPtr c, MonEnum f, IntPtr d);
  [DllImport("user32.dll")] public static extern bool GetMonitorInfo(IntPtr m, ref MONITORINFO mi);

  [StructLayout(LayoutKind.Sequential)] public struct P { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct MONITORINFO {
    public int cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags;
  }
  public delegate bool MonEnum(IntPtr m, IntPtr hdc, IntPtr r, IntPtr d);

  public static List<string> Monitors() {
    var outp = new List<string>();
    EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (m, hdc, r, d) => {
      var mi = new MONITORINFO(); mi.cbSize = Marshal.SizeOf(typeof(MONITORINFO));
      if (GetMonitorInfo(m, ref mi))
        outp.Add(string.Format("{0},{1} .. {2},{3}{4}",
          mi.rcMonitor.L, mi.rcMonitor.T, mi.rcMonitor.R, mi.rcMonitor.B,
          (mi.dwFlags & 1) != 0 ? "  [primary]" : ""));
      return true;
    }, IntPtr.Zero);
    return outp;
  }
}
"@

# Must happen before any coordinate call. Without it Windows virtualises this
# process's cursor coordinates, so SetCursorPos lands somewhere other than
# asked for on any scaled display, which is exactly what the app would see.
$aware = [C]::SetProcessDpiAwarenessContext([IntPtr](-4))  # PER_MONITOR_AWARE_V2
Write-Host ("DPI awareness set: {0}" -f $aware)

Stop-Process -Name cantos -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 600

if (-not (Backup-HcConfig $backup)) { Write-Host 'RESULT: SKIP - stale backup present'; exit 1 }
Remove-Item $marker -ErrorAction SilentlyContinue

Write-Host "physical monitors:"
foreach ($m in [C]::Monitors()) { Write-Host ("  {0}" -f $m) }

# Union of all monitor rects is what OuterCorners anchors to.
$L = [int]::MaxValue; $T = [int]::MaxValue; $R = [int]::MinValue; $B = [int]::MinValue
foreach ($m in [C]::Monitors()) {
  if ($m -match '^(-?\d+),(-?\d+) \.\. (-?\d+),(-?\d+)') {
    $L = [Math]::Min($L, [int]$Matches[1]); $T = [Math]::Min($T, [int]$Matches[2])
    $R = [Math]::Max($R, [int]$Matches[3]); $B = [Math]::Max($B, [int]$Matches[4])
  }
}
if ($R -le $L -or $B -le $T) {
  Restore-HcConfig $backup
  Write-Host 'RESULT: FAIL - bogus union rect'; exit 1
}
Write-Host ("union rect: {0},{1} .. {2},{3}" -f $L, $T, $R, $B)

Write-HcConfig @{
  enabled               = $true
  corners               = @(
    @{ kind = 'custom'; command = 'cmd.exe'; args = ('/c echo fired > "{0}"' -f $marker) },
    @{ kind = 'none' }, @{ kind = 'none' }, @{ kind = 'none' }
  )
  dwellMs               = 150
  cooldownMs            = 700
  cornerSize            = 12
  monitorMode           = 'outerCorners'
  suppressFullscreen    = $false
  suppressWhileDragging = $true
  modifier              = 'none'
}
Write-Host "wrote test config (no BOM)"

# Sanity-check the action itself, independent of corner detection.
Remove-Item $marker -ErrorAction SilentlyContinue
Start-Process -FilePath 'cmd.exe' -ArgumentList ('/c echo fired > "{0}"' -f $marker) -WindowStyle Hidden -Wait
Write-Host ("action sanity check (marker written directly): {0}" -f (Test-Path $marker))
Remove-Item $marker -ErrorAction SilentlyContinue

$orig = New-Object 'C+P'
[void][C]::GetCursorPos([ref]$orig)
Write-Host ("cursor was at {0},{1}" -f $orig.X, $orig.Y)

# Hold the cursor in place for the whole dwell window. Physical mouse input
# from whoever is at the machine will otherwise drag it out mid-dwell and the
# corner legitimately will not fire.
function Park($x, $y, $label) {
  $drift = 0
  for ($i = 0; $i -lt 18; $i++) {
    $p = New-Object 'C+P'; [void][C]::GetCursorPos([ref]$p)
    if ($i -gt 0 -and ([Math]::Abs($p.X - $x) -gt 2 -or [Math]::Abs($p.Y - $y) -gt 2)) { $drift++ }
    [void][C]::SetCursorPos($x, $y)
    Start-Sleep -Milliseconds 100
  }
  $p = New-Object 'C+P'; [void][C]::GetCursorPos([ref]$p)
  $note = if ($drift -gt 2) { "  [!! {0} samples drifted - physical mouse input detected]" -f $drift } else { "" }
  Write-Host ("  {0}: asked {1},{2} -> held {3},{4}{5}" -f $label, $x, $y, $p.X, $p.Y, $note)
}

# Everything past this point can leave the machine in a state the user did not
# ask for -- a test config in place, the app running, the cursor parked in a
# corner -- so it all runs under a finally.
$started = $false
$aborted = $false
try {
  $exe = Join-Path $proj 'target\release\cantos.exe'
  Start-Process -FilePath $exe
  Start-Sleep -Seconds 2
  $started = $null -ne (Get-Process cantos -ErrorAction SilentlyContinue)
  if (-not $started) { throw 'process did not start' }

  Write-Host "--- test 1: park in the top-left corner ---"
  Park ($L + 1) ($T + 1) "corner"
  $fired1 = Test-Path $marker
  Write-Host ("  marker present: {0} (want True)" -f $fired1)

  Write-Host "--- test 2: centre of screen should NOT fire ---"
  Remove-Item $marker -ErrorAction SilentlyContinue
  Park ([int](($L + $R) / 2)) ([int](($T + $B) / 2)) "centre"
  $fired2 = Test-Path $marker
  Write-Host ("  marker present: {0} (want False)" -f $fired2)

  Write-Host "--- test 3: re-entering the corner fires again ---"
  Park ($L + 2) ($T + 2) "corner"
  $fired3 = Test-Path $marker
  Write-Host ("  marker present: {0} (want True)" -f $fired3)
}
catch {
  $aborted = $true
  Write-Host ("  aborted: {0}" -f $_.Exception.Message) -ForegroundColor Red
}
finally {
  [void][C]::SetCursorPos($orig.X, $orig.Y)
  Stop-Process -Name cantos -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 400
  Restore-HcConfig $backup
  Remove-Item $marker -ErrorAction SilentlyContinue
}

if (-not $started) { Write-Host 'RESULT: FAIL - process did not start'; exit 1 }
# Without this, an abort mid-run leaves $fired2/$fired3 unassigned and the line
# below reads "RESULT: FAIL (corner=True centre= rearm=)" -- indistinguishable
# from a corner that genuinely did not fire.
if ($aborted)      { Write-Host 'RESULT: FAIL - the run aborted before every case completed'; exit 1 }

Write-Host ""
if ($fired1 -and -not $fired2 -and $fired3) { Write-Host "RESULT: PASS"; exit 0 }
Write-Host ("RESULT: FAIL  (corner={0} centre={1} rearm={2})" -f $fired1, $fired2, $fired3)
exit 1
