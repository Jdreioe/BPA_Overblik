; Per-user Windows installer for BPA Overblik. Built by
; scripts/package-windows-exe.ps1, which passes the release version and the
; built GUI on the command line:
;
;   ISCC /DAppVersion=2026.09.26 /DVersionInfo=2026.9.26.1 /DSourceExe=... bpa-overblik.iss
;
; It installs into %LOCALAPPDATA%\Programs without administrator rights, so the
; in-app updater can run the next setup silently. User data lives in
; %APPDATA%\teamup-shift-sync and is never touched, not even on uninstall.
;
; Parameters the updater passes (see desktop/src/update.rs):
;   /RELAUNCH           start the installed app when a silent install finishes
;   /REPLACES=<path>    the exe the update replaces. Setup waits for it to exit,
;                       and deletes it once the installed copy is in place if
;                       it is a portable teamup-shift-sync-*-x86_64.exe

#ifndef AppVersion
  #error AppVersion must be defined
#endif
#ifndef VersionInfo
  #error VersionInfo must be defined
#endif
#ifndef SourceExe
  #error SourceExe must be defined
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

#define AppName "BPA Overblik"
#define AppExe "BPA Overblik.exe"

[Setup]
; Never change the AppId: it is how an update finds the existing install.
AppId={{155291D4-EF41-47E1-B584-D1954E8A9148}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Jdreioe
AppPublisherURL=https://github.com/Jdreioe/BPA_Overblik
AppSupportURL=https://github.com/Jdreioe/BPA_Overblik/issues
AppUpdatesURL=https://github.com/Jdreioe/BPA_Overblik/releases/latest
VersionInfoVersion={#VersionInfo}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} installation
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DefaultGroupName={#AppName}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=force
RestartApplications=no
SetupIconFile=teamup-shift-sync.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=teamup-shift-sync-{#AppVersion}-x86_64-setup

[Languages]
Name: "danish"; MessagesFile: "compiler:Languages\Danish.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent and HasParam('/RELAUNCH')

[Code]
function HasParam(Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
      Result := True;
end;

{ Only ever delete a file the portable release could have been called. }
function IsPortableRelease(Path: String): Boolean;
var
  Name: String;
begin
  Name := LowerCase(ExtractFileName(Path));
  Result := (Pos('teamup-shift-sync-', Name) = 1)
    and (Copy(Name, Length(Name) - Length('-x86_64.exe') + 1, MaxInt) = '-x86_64.exe');
end;

function IsInUse(Path: String): Boolean;
var
  Stream: TFileStream;
begin
  Result := False;
  if not FileExists(Path) then
    Exit;
  try
    Stream := TFileStream.Create(Path, fmOpenReadWrite or fmShareExclusive);
    Stream.Free;
  except
    Result := True;
  end;
end;

{ The app starts this setup and then exits. Wait for that instead of letting
  Restart Manager time out on a window that does not answer it. }
function InitializeSetup(): Boolean;
var
  Running: String;
  Tries: Integer;
begin
  Result := True;
  Running := ExpandConstant('{param:REPLACES}');
  Tries := 0;
  while (Running <> '') and IsInUse(Running) and (Tries < 50) do
  begin
    Sleep(200);
    Tries := Tries + 1;
  end;
end;

procedure RemovePortableCopy();
var
  Portable: String;
begin
  Portable := ExpandConstant('{param:REPLACES}');
  if (Portable = '') or not IsPortableRelease(Portable) then
    Exit;
  if CompareText(Portable, ExpandConstant('{app}\{#AppExe}')) = 0 then
    Exit;
  DeleteFile(Portable);
  DeleteFile(Portable + '.old');
  DeleteFile(Portable + '.new');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    RemovePortableCopy();
end;
