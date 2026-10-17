; Inno Setup 6 script for GitKwaken.
;
; Built by packaging/build-release.ps1:
;   ISCC.exe /DAppVersion=1.0.0 /DSourceDir=<dist\GitKwaken-1.0.0-x64> /DOutputDir=<dist> packaging\gitkwaken.iss
;
; Per-user install by default ({autopf} = %LOCALAPPDATA%\Programs in non-admin mode), no UAC
; prompt. Running the installer again upgrades in place (same AppId). The user may still choose
; an all-users install from the privileges dialog; HKA then maps to HKLM instead of HKCU.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef NumericVersion
  #define NumericVersion AppVersion
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\GitKwaken-" + AppVersion + "-x64"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "GitKwaken"
#define AppExe "gitkwaken.exe"
#ifndef AppUrl
  ; Passed by build-release.ps1 from the repository (/DAppUrl=...).
  #define AppUrl "https://github.com"
#endif

[Setup]
; NEVER change AppId: it is what makes a new installer upgrade the existing install.
AppId={{6C1B7D3E-4F2A-4B8E-9A5D-2E7F1C0B9A64}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=GitKwaken contributors
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
; Numeric only (1.0.1-beta.1 -> 1.0.1); build-release.ps1 passes /DNumericVersion.
VersionInfoVersion={#NumericVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
UsePreviousAppDir=yes
UsePreviousTasks=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=GitKwaken-{#AppVersion}-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Close a running GitKwaken (Restart Manager) so the exe can be replaced during upgrade.
CloseApplications=force
RestartApplications=no
LicenseFile={#SourceDir}\LICENSE-MIT

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "contextmenu"; Description: "Add ""Open in GitKwaken"" to the folder context menu"; GroupDescription: "Shell integration:"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
#if FileExists(SourceDir + "\git\cmd\git.exe")
; Bundled MinGit (build-release.ps1 -BundleGit); gk-cli prefers {app}\git\cmd\git.exe.
Source: "{#SourceDir}\git\*"; DestDir: "{app}\git"; Flags: ignoreversion recursesubdirs createallsubdirs
#endif

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Right-click on a folder.
Root: HKA; Subkey: "Software\Classes\Directory\shell\GitKwaken"; ValueType: string; ValueName: ""; ValueData: "Open in GitKwaken"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\GitKwaken"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\GitKwaken\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: contextmenu
; Right-click on the background of an open folder.
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\GitKwaken"; ValueType: string; ValueName: ""; ValueData: "Open in GitKwaken"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\GitKwaken"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\GitKwaken\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: contextmenu
; "App Paths" so `Win+R gitkwaken` works.
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#AppExe}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe}"; Flags: uninsdeletekey

[InstallDelete]
; Leftovers of the built-in self-updater (gk-update).
Type: files; Name: "{app}\{#AppExe}.old*"
Type: files; Name: "{app}\{#AppExe}.new"
#if FileExists(SourceDir + "\git\cmd\git.exe")
; Replace (not merge) a previously bundled MinGit so removed files do not linger.
Type: filesandordirs; Name: "{app}\git"
#endif

[UninstallDelete]
Type: files; Name: "{app}\{#AppExe}.old*"
Type: files; Name: "{app}\{#AppExe}.new"

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
// When the user unticks "contextmenu" on upgrade, remove the keys a previous install created.
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('contextmenu') then
  begin
    RegDeleteKeyIncludingSubkeys(HKA, 'Software\Classes\Directory\shell\GitKwaken');
    RegDeleteKeyIncludingSubkeys(HKA, 'Software\Classes\Directory\Background\shell\GitKwaken');
  end;
end;
