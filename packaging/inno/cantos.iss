; Inno Setup script for Cantos.
;   iscc packaging\inno\cantos.iss
; Expects target\release\cantos.exe to already be built.

#define AppName    "Cantos"
; The release workflow passes /DAppVersion=x.y.z; this is the local default.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define AppPublisher "kidchenko"
#define AppURL     "https://github.com/kidchenko/windows-cantos"
#define AppExe     "cantos.exe"

[Setup]
; Never reuse this GUID for another product; it is how Windows identifies
; the app across upgrades and uninstalls.
AppId={{7B1F4C2E-9A3D-4E58-B6C1-2F8A5D0E7341}
AppName={#AppName}
AppVersion={#AppVersion}
; Without this Add/Remove Programs reads "Cantos version 0.1.0".
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=Cantos-Setup-{#AppVersion}
SetupIconFile=..\..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; The app never needs elevation itself; only writing to Program Files does.
PrivilegesRequired=admin
; Restart Manager closes a running instance during upgrade or uninstall
; instead of leaving a locked file behind.
CloseApplications=yes
CloseApplicationsFilter=*.exe
; The [Code] section intentionally touches HKCU; see the note there.
UsedUserAreasWarning=no
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
; --settings so a first run lands on the settings window rather than looking
; like nothing happened. Autostart is a toggle in that window: the app writes
; it to HKCU itself, which keeps it attached to the right user even though
; this installer runs elevated.
Filename: "{app}\{#AppExe}"; Parameters: "--settings"; \
  Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stop the tray instance before files are removed.
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im {#AppExe}"; \
  Flags: runhidden; RunOnceId: "KillCantos"

[Code]
// The autostart entry is per-user and written by the app, so the uninstaller
// has to clear it explicitly or Windows keeps trying to launch a binary that
// is no longer there.
//
// Caveat, and the reason for UsedUserAreasWarning=no above: under UAC an
// admin elevating their own account keeps the same HKCU, so this works in the
// normal case. If a *different* admin supplies credentials, this clears their
// Run key rather than the installing user's, and the original user is left to
// remove a dead startup entry by hand. Writing to HKLM instead would be worse
// -- it would force autostart on every account on the machine.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RegDeleteValue(HKEY_CURRENT_USER,
      'Software\Microsoft\Windows\CurrentVersion\Run', 'Cantos');

    // The WebView2 profile, which the app puts under LOCALAPPDATA because it
    // cannot write beside the executable in Program Files (see ui/mod.rs).
    // The installer never created it, so nothing removes it automatically,
    // and it is several megabytes of Chromium cache.
    //
    // Settings under %APPDATA%\Cantos are left alone on purpose: reinstalling
    // should bring your corners back.
    DelTree(ExpandConstant('{localappdata}\Cantos\WebView2'), True, True, True);
    // Only succeeds if nothing else lives there, which is what we want.
    RemoveDir(ExpandConstant('{localappdata}\Cantos'));

    // Debris from 0.1.x, which tried to create its profile here and failed.
    // A no-op on any machine that never ran one of those builds, and it is
    // what stops the install directory being left behind.
    DelTree(ExpandConstant('{app}\cantos.exe.WebView2'), True, True, True);
    RemoveDir(ExpandConstant('{app}'));
  end;
end;
