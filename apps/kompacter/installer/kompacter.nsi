; Kompacter installer: per-user (no admin prompt). Default folder
; %LOCALAPPDATA%\Programs\Kompacter; you can pick another one (remembered for updates).
; Built by .github/workflows/release.yml:  makensis /DVERSION=0.0.3 kompacter.nsi

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "Kompacter"
OutFile "Kompacter-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Kompacter"
InstallDirRegKey HKCU "Software\Kompacter" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "..\icon.ico"
!define MUI_UNICON "..\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\Kompacter.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Kompacter"
!define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kompacter"

!insertmacro MUI_PAGE_DIRECTORY
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

  WriteRegStr HKCU "Software\Kompacter" "InstallDir" "$INSTDIR"
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
  DeleteRegKey HKCU "Software\Kompacter"
  ; Only Kompacter's own files: the folder may be one the user chose (never wipe it).
  Delete "$INSTDIR\Kompacter.exe"
  Delete "$INSTDIR\ffmpeg.exe"
  Delete "$INSTDIR\ffprobe.exe"
  Delete "$INSTDIR\FFMPEG-LICENSE.txt"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  ; Compressed copies (cleaned weekly anyway).
  RMDir /r "$INSTDIR\Kompacted"
  ; Removed only if it's now empty.
  RMDir "$INSTDIR"
SectionEnd
