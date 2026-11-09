; Inno Setup script for leash (folder-guard shelf, gledach.de).
; Not built in this tree: compile later on Windows with Inno Setup 6,
; or under Wine if it is available. The portable zip is the current ship path.
; The app can also add a Start-menu shortcut itself (Add to Start menu).

#define MyAppName "leash"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "gledach.de"
#define MyAppURL "https://gledach.de"
#define MyAppExeName "leash.exe"

[Setup]
AppId={{A3E6C1D0-7B2F-4F19-9C4A-1E8B6D0F2A11}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
DefaultDirName={autopf}\leash
DefaultGroupName=leash
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=leash-setup-win64
Compression=lzma
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "..\target\x86_64-pc-windows-gnu\release\leash.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\leash"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\leash"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Open leash"; Flags: nowait postinstall skipifsilent
