; The Windows installer, built with Inno Setup:
;
;   iscc /DAppVersion=0.1.0 /DSourceDir=..\..\target\release packaging\windows\scrollback.iss
;
; It installs for the current user only, so it asks for no administrator
; password, and adds a Start menu entry and an uninstaller.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\target\package"
#endif

[Setup]
; This identifies Scrollback to Windows across versions. Never change it.
AppId={{6F0C6D0B-8A0E-4B55-9C55-3E5C2F3B7A41}
AppName=Scrollback
AppVersion={#AppVersion}
AppVerName=Scrollback {#AppVersion}
DefaultDirName={localappdata}\Programs\Scrollback
DefaultGroupName=Scrollback
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=scrollback-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\crates\app\icon\scrollback.ico
UninstallDisplayIcon={app}\scrollback-app.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Close a running copy first, so its program file can be replaced.
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "Add a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#SourceDir}\scrollback-app.exe"; DestDir: "{app}"; Flags: ignoreversion
; The command-line tool, for those who want it on their PATH.
Source: "{#SourceDir}\scrollback.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Scrollback"; Filename: "{app}\scrollback-app.exe"
Name: "{autodesktop}\Scrollback"; Filename: "{app}\scrollback-app.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\scrollback-app.exe"; Description: "Open Scrollback"; Flags: nowait postinstall skipifsilent
; When Scrollback updates itself it runs this silently, then expects to be
; opened again.
Filename: "{app}\scrollback-app.exe"; Flags: nowait; Check: WizardSilent
