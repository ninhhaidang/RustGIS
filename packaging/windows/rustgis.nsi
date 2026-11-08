; setup.exe installer (NSIS 3), built by package.sh. A per-user install that needs no admin
; rights, in %LOCALAPPDATA%\Programs\RustGIS: a Start menu shortcut, rustgis-cli on the user's
; PATH, .rgis/.rgisx projects opened by RustGIS and RustGIS offered under Open with for .geojson.
; Defines passed with -D: VERSION, VERSION4 (four numbers), STAGE (the folder holding the exes,
; LICENSE, THIRD-PARTY-LICENSES.txt and path.ps1), ICON, SIDEBAR (a 164x314 BMP) and OUTFILE.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
ManifestDPIAware true

!define APP "RustGIS"
!define PUBLISHER "Open Geospatial Solutions"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"

VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} ${VERSION} setup"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "Copyright the RustGIS contributors. Apache-2.0."

!include "MUI2.nsh"
!include "WinMessages.nsh"
!include "FileFunc.nsh"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${SIDEBAR}"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "${SIDEBAR}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\rustgis.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Asks Explorer to pick up changed file associations.
!macro RefreshShell
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

; Tells running programs (a new terminal) that the user's PATH changed.
!macro RefreshEnvironment
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

; RustGIS's Download and Install (Help › Check for Updates) runs this installer as RustGIS
; closes: `/S /RELAUNCH /D=<its folder>`, and winget runs it silently too. A program can't be
; replaced while it runs, so a silent install waits for each one it replaces (rustgis.exe, and
; rustgis-cli.exe a terminal may be running) to be released, for up to 30 seconds each. Still
; running then, nothing is installed (exit code 2): a silent install would skip the locked files
; and still record the new version. RustGIS's next start says the update didn't arrive. Run
; interactively (without /S), the installer doesn't wait: replacing a running program asks to
; retry, as before.
; `ID` makes the labels of each insertion unique.
!macro WaitForFree FILE ID
  IfSilent 0 free_${ID}
  IfFileExists "$INSTDIR\${FILE}" 0 free_${ID}
  StrCpy $1 0
  wait_${ID}:
    ClearErrors
    FileOpen $0 "$INSTDIR\${FILE}" a
    IfErrors 0 open_${ID}
    IntOp $1 $1 + 1
    ; Equal (60 tries, 30 s) or more: still running; less: 0, the next line, waits again.
    IntCmp $1 60 busy_${ID} 0 busy_${ID}
    Sleep 500
    Goto wait_${ID}
  busy_${ID}:
    SetErrorLevel 2
    Quit
  open_${ID}:
    FileClose $0
  free_${ID}:
!macroend

Section "Install"
  SetOutPath "$INSTDIR"
  ; Both before any file is written: either one still running installs nothing.
  !insertmacro WaitForFree "rustgis.exe" app
  !insertmacro WaitForFree "rustgis-cli.exe" cli
  File "${STAGE}\rustgis.exe"
  File "${STAGE}\rustgis-cli.exe"
  File /oname=LICENSE.txt "${STAGE}\LICENSE"
  File "${STAGE}\THIRD-PARTY-LICENSES.txt"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\rustgis.exe"

  ; rustgis-cli from any new terminal.
  InitPluginsDir
  File /oname=$PLUGINSDIR\path.ps1 "${STAGE}\path.ps1"
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\path.ps1" add "$INSTDIR"'
  Pop $0
  StrCmp $0 0 +2
    MessageBox MB_OK|MB_ICONEXCLAMATION "${APP} is installed, but rustgis-cli could not be added to your PATH ($0). Run it from $INSTDIR instead." /SD IDOK
  !insertmacro RefreshEnvironment

  ; Projects open in RustGIS; GeoJSON lists it under Open with.
  WriteRegStr HKCU "Software\Classes\RustGIS.Project" "" "RustGIS project"
  WriteRegStr HKCU "Software\Classes\RustGIS.Project\DefaultIcon" "" '"$INSTDIR\rustgis.exe",0'
  WriteRegStr HKCU "Software\Classes\RustGIS.Project\shell\open\command" "" '"$INSTDIR\rustgis.exe" "%1"'
  WriteRegStr HKCU "Software\Classes\.rgis" "" "RustGIS.Project"
  WriteRegStr HKCU "Software\Classes\.rgisx" "" "RustGIS.Project"
  WriteRegStr HKCU "Software\Classes\.geojson\OpenWithProgids" "RustGIS.Project" ""
  !insertmacro RefreshShell

  ; Apps & features.
  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" '"$INSTDIR\rustgis.exe",0'
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://rustgis.app"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0

  ; Run by Download and Install: open the updated RustGIS again (the finish page's Start
  ; RustGIS does it when installing by hand).
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/RELAUNCH" $R1
  IfErrors +2
    Exec '"$INSTDIR\rustgis.exe"'
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\${APP}.lnk"

  InitPluginsDir
  File /oname=$PLUGINSDIR\path.ps1 "${STAGE}\path.ps1"
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\path.ps1" remove "$INSTDIR"'
  Pop $0
  StrCmp $0 0 +2
    MessageBox MB_OK|MB_ICONEXCLAMATION "$INSTDIR could not be removed from your PATH ($0). Remove it in Settings › System › About › Advanced system settings › Environment Variables." /SD IDOK
  !insertmacro RefreshEnvironment

  ; Only the associations this installer made: another program may own .geojson.
  DeleteRegValue HKCU "Software\Classes\.geojson\OpenWithProgids" "RustGIS.Project"
  ReadRegStr $0 HKCU "Software\Classes\.rgis" ""
  StrCmp $0 "RustGIS.Project" 0 +2
    DeleteRegKey HKCU "Software\Classes\.rgis"
  ReadRegStr $0 HKCU "Software\Classes\.rgisx" ""
  StrCmp $0 "RustGIS.Project" 0 +2
    DeleteRegKey HKCU "Software\Classes\.rgisx"
  DeleteRegKey HKCU "Software\Classes\RustGIS.Project"
  !insertmacro RefreshShell

  Delete "$INSTDIR\rustgis.exe"
  Delete "$INSTDIR\rustgis-cli.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegValue HKCU "Software\${APP}" "InstallDir"
  DeleteRegKey /ifempty HKCU "Software\${APP}"
SectionEnd
