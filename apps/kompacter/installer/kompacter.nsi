; Kompacter installer: per-user (no admin prompt), into %LOCALAPPDATA%\Programs\Kompacter.
; Built by .github/workflows/release.yml:  makensis /DVERSION=0.0.3 kompacter.nsi

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "Kompacter"
OutFile "Kompacter-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Kompacter"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "..\icon.ico"
!define MUI_UNICON "..\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\Kompacter.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Kompacter"
!define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kompacter"

!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ; Close a running copy so its files can be replaced.
  nsExec::Exec 'taskkill /IM Kompacter.exe /F'
  SetOutPath "$INSTDIR"
  File "dist\Kompacter.exe"
  File "dist\ffmpeg.exe"
  File "dist\ffprobe.exe"
  File "dist\FFMPEG-LICENSE.txt"
  File "dist\LICENSE.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Kompacter.lnk" "$INSTDIR\Kompacter.exe"

  WriteRegStr HKCU "${UNINST}" "DisplayName" "Kompacter"
  WriteRegStr HKCU "${UNINST}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST}" "Publisher" "Kompacter"
  WriteRegStr HKCU "${UNINST}" "DisplayIcon" "$INSTDIR\Kompacter.exe"
  WriteRegStr HKCU "${UNINST}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNINST}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM Kompacter.exe /F'
  Delete "$SMPROGRAMS\Kompacter.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Kompacter"
  DeleteRegKey HKCU "${UNINST}"
  ; Includes the Kompacted folder (compressed copies, cleaned weekly anyway).
  RMDir /r "$INSTDIR"
SectionEnd
