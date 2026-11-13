!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!ifndef APP_VERSION
  !error "APP_VERSION is required"
!endif
!ifndef SOURCE_EXE
  !error "SOURCE_EXE is required"
!endif
!ifndef SOURCE_CLI
  !error "SOURCE_CLI is required"
!endif
!ifndef SOURCE_SHELL
  !error "SOURCE_SHELL is required"
!endif
!ifndef OUTPUT_DIR
  !error "OUTPUT_DIR is required"
!endif
!ifndef PROJECT_ROOT
  !error "PROJECT_ROOT is required"
!endif
!ifndef BUNDLE_SUFFIX
  !define BUNDLE_SUFFIX ""
!endif

Unicode true
Name "RShell ${APP_VERSION}"
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey "ProductName" "RShell"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "FileDescription" "RShell Windows x64 安装程序"
VIAddVersionKey "LegalCopyright" ""
OutFile "${OUTPUT_DIR}\RShell-${APP_VERSION}-windows-x64${BUNDLE_SUFFIX}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\RShell"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "InstallLocation"
RequestExecutionLevel user
SetCompressor zlib
Icon "${PROJECT_ROOT}\crates\rshell-app\assets\rshell-icon.ico"
UninstallIcon "${PROJECT_ROOT}\crates\rshell-app\assets\rshell-icon.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "RShell 仅支持 Windows x64。"
    Abort
  ${EndIf}
FunctionEnd

Section "RShell" Main
  SectionIn RO
  ; Remove the 1.0.0 Inno installation before switching installer families.
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{C929F176-53EA-4D38-A04E-59EE7DDA4264}_is1" "InstallLocation"
  ${If} $0 != ""
    IfFileExists "$0\unins000.exe" 0 done_inno
    ExecWait '"$0\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART' $1
    ${If} $1 != 0
      MessageBox MB_ICONSTOP "旧版 RShell 卸载失败，请关闭程序后重试。"
      Abort
    ${EndIf}
  ${EndIf}
  done_inno:
  SetOutPath "$INSTDIR"
  File /oname=rshell.exe "${SOURCE_EXE}"
  File /oname=rshell-cli.exe "${SOURCE_CLI}"
  File /oname=rshell-shell.exe "${SOURCE_SHELL}"
!ifdef AGENT_SDK_DIR
  SetOutPath "$INSTDIR\agent-sdk"
  File /r "${AGENT_SDK_DIR}\*"
!endif
  SetOutPath "$INSTDIR"
  File "${PROJECT_ROOT}\README.md"
  File "${PROJECT_ROOT}\LICENSE"
  SetOutPath "$INSTDIR\docs"
  File "${PROJECT_ROOT}\docs\LOCAL_TESTING.md"
  File "${PROJECT_ROOT}\docs\AGENT_COMPONENT.md"
  File "${PROJECT_ROOT}\docs\SHORTCUTS.md"
  File "${PROJECT_ROOT}\docs\TUNNELS.md"
  File "${PROJECT_ROOT}\docs\CLI.md"
  File "${PROJECT_ROOT}\docs\SESSION_TREE.md"
  File "${PROJECT_ROOT}\docs\STATUS.md"
  SetOutPath "$INSTDIR\licenses"
  File /oname=Geist-OFL.txt "${PROJECT_ROOT}\crates\rshell-app\assets\fonts\LICENSE.txt"
  File /oname=Lucide.txt "${PROJECT_ROOT}\crates\rshell-app\assets\lucide\LICENSE"
  File "${PROJECT_ROOT}\crates\rshell-app\assets\licenses\rubash-MIT.txt"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\RShell"
  CreateShortcut "$SMPROGRAMS\RShell\RShell.lnk" "$INSTDIR\rshell.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "DisplayName" "RShell ${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "DisplayIcon" "$INSTDIR\rshell.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "NoRepair" 1
SectionEnd

Section /o "桌面快捷方式" Desktop
  CreateShortcut "$DESKTOP\RShell.lnk" "$INSTDIR\rshell.exe"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "DesktopShortcut" 1
SectionEnd

Section "Uninstall"
  ReadRegDWORD $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell" "DesktopShortcut"
  ${If} $0 == 1
    Delete "$DESKTOP\RShell.lnk"
  ${EndIf}
  Delete "$SMPROGRAMS\RShell\RShell.lnk"
  RMDir "$SMPROGRAMS\RShell"
  Delete "$INSTDIR\rshell.exe"
  Delete "$INSTDIR\rshell-cli.exe"
  Delete "$INSTDIR\rshell-shell.exe"
  RMDir /r "$INSTDIR\agent-sdk"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\docs\STATUS.md"
  Delete "$INSTDIR\docs\LOCAL_TESTING.md"
  Delete "$INSTDIR\docs\AGENT_COMPONENT.md"
  Delete "$INSTDIR\docs\SHORTCUTS.md"
  Delete "$INSTDIR\docs\TUNNELS.md"
  Delete "$INSTDIR\docs\CLI.md"
  Delete "$INSTDIR\docs\SESSION_TREE.md"
  RMDir "$INSTDIR\docs"
  Delete "$INSTDIR\licenses\Geist-OFL.txt"
  Delete "$INSTDIR\licenses\Lucide.txt"
  Delete "$INSTDIR\licenses\rubash-MIT.txt"
  RMDir "$INSTDIR\licenses"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\RShell"
SectionEnd
