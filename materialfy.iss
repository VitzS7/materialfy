; Inno Setup script for Materialfy (Rust + Qt 6)
; Built from build_windows.ps1:
;   ISCC /DAppVersion=0.1.0 /DDistDir=target\dist\materialfy /Otarget\dist materialfy.iss
; Output: target\dist\Materialfy-<version>-Setup.exe
; Per-user install (no UAC) into %LocalAppData%\Programs\Materialfy

#define AppName "Materialfy"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
; numeric version for the file properties (without -rc1 etc.)
#ifndef AppNumVersion
  #define AppNumVersion AppVersion
#endif
; folder assembled by build_windows.ps1: materialfy.exe, qml\, assets\, Qt, MSVC runtime
#ifndef DistDir
  #define DistDir "target\dist\materialfy"
#endif
#define AppPublisher "Materialfy"
#define AppExeName "materialfy.exe"

[Setup]
LicenseFile=LICENSE
; Do not change AppId: a new version installs over the old one by it. Does not collide with YaMusic (yamusic-md3).
AppId={{A5260B19-41AE-4D9D-9841-204E686F2ADD}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
VersionInfoVersion={#AppNumVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} - Spotify client in Material 3 Expressive style
; Per-user install, no UAC
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes
OutputDir=target\dist
OutputBaseFilename=Materialfy-{#AppVersion}-Setup
SetupIconFile=materialfy.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; x64 only
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; No administrator rights
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Close a running Materialfy before updating
CloseApplications=yes
CloseApplicationsFilter=*{#AppExeName}
; Minimum Windows version: 10 1809
MinVersion=10.0.17763
; Setup language follows the Windows UI language; the dialog appears only when it matches neither
ShowLanguageDialog=auto

[Languages]
; English is the default (first) language, then Russian and Brazilian Portuguese
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[CustomMessages]
english.StartupGroup=Autostart:
english.StartupTask=Start %1 when Windows starts
russian.StartupGroup=Автозапуск:
russian.StartupTask=Запускать %1 при старте Windows
brazilianportuguese.StartupGroup=Inicializar com o Windows:
brazilianportuguese.StartupTask=Iniciar %1 quando o Windows iniciar

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}";            GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce
Name: "startupicon"; Description: "{cm:StartupTask,{#AppName}}";       GroupDescription: "{cm:StartupGroup}";    Flags: unchecked

[InstallDelete]
; QML is rebuilt as a whole: remove files dropped in the new version
Type: filesandordirs; Name: "{app}\qml"

[Files]
; Everything in the distribution folder goes to the install folder
Source: "{#DistDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
; Start menu
Name: "{userprograms}\{#AppName}\{#AppName}";                        Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\{#AppExeName}"; AppUserModelID: "{#AppName}"
Name: "{userprograms}\{#AppName}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"

; Desktop
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\{#AppExeName}"; AppUserModelID: "{#AppName}"; Tasks: desktopicon

; Autostart
Name: "{userstartup}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: startupicon

[Run]
; Offer to launch Materialfy after installation
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Update from inside the app (Setup.exe /SILENT ... /RELAUNCH): start the new version once the files are in place.
; Without /RELAUNCH (a regular or a scripted silent install) nothing changes.
Filename: "{app}\{#AppExeName}"; Flags: nowait; Check: IsAppRelaunch

[UninstallDelete]
; Settings, sign-in (Windows Credential Manager) and cache are kept on uninstall; "Sign out" in the app clears them

[Code]
// /RELAUNCH on the command line: the app started its own update and wants to be back afterwards
function IsAppRelaunch: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/RELAUNCH') = 0 then
      Result := True;
end;
