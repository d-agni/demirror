; demirror's Windows installer (Inno Setup 6). The release workflow builds it with
;   iscc /DVersion=0.2.0 /DSource=<folder with the programs and driver\> /O<output> demirror.iss
; It installs the programs, adds the display driver (so devices extend the desktop) and lets
; devices on the local network through the firewall. Uninstalling removes all three.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\dist\windows"
#endif

[Setup]
AppId={{903EC75C-0577-4791-B3A9-99E124919366}
AppName=demirror
AppVersion={#Version}
AppVerName=demirror {#Version}
AppPublisher=demirror
AppPublisherURL=https://github.com/d-agni/demirror
AppSupportURL=https://github.com/d-agni/demirror/issues
DefaultDirName={autopf}\demirror
DefaultGroupName=demirror
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 10 1709: the first with the display driver framework demirror uses.
MinVersion=10.0.16299
OutputBaseFilename=demirror-{#Version}-windows-x86_64-setup
SetupIconFile=..\icons\demirror.ico
UninstallDisplayIcon={app}\demirror.exe
LicenseFile={#Source}\LICENSE-MIT
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
#ifdef Sign
; iscc /DSign /Sdemirror="<command> $f": the installer and uninstaller get signed.
SignTool=demirror
SignedUninstaller=yes
#endif

[Tasks]
Name: "driver"; Description: "Install the display driver: devices extend the desktop like real monitors"
Name: "firewall"; Description: "Let devices on this network connect (Windows Firewall)"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#Source}\demirror.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\vmon-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\vmon-client.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\driver\*"; DestDir: "{app}\driver"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\demirror"; Filename: "{app}\demirror.exe"
Name: "{autodesktop}\demirror"; Filename: "{app}\demirror.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\demirror.exe"; Parameters: "--install-driver --log ""{tmp}\driver.log"""; StatusMsg: "Installing the display driver..."; Tasks: driver; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""demirror"" dir=in action=allow program=""{app}\demirror.exe"" enable=yes profile=private,domain"; StatusMsg: "Opening the firewall for demirror..."; Tasks: firewall; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""demirror"" dir=in action=allow program=""{app}\vmon-server.exe"" enable=yes profile=private,domain"; Tasks: firewall; Flags: runhidden waituntilterminated
Filename: "{app}\demirror.exe"; Description: "Open demirror"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\demirror.exe"; Parameters: "--uninstall-driver"; RunOnceId: "driver"; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""demirror"""; RunOnceId: "firewall"; Flags: runhidden waituntilterminated

[Code]
// Say so when the driver didn't install; demirror still mirrors screens and can retry it.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Reason: AnsiString;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('driver')
    and LoadStringFromFile(ExpandConstant('{tmp}\driver.log'), Reason) then
    SuppressibleMsgBox('The display driver could not be installed: ' + String(Reason) + #13#10#13#10 +
      'demirror can still mirror screens, and can try again from its Share page.',
      mbError, MB_OK, IDOK);
end;
