Unicode true
RequestExecutionLevel user
SetCompressor /SOLID lzma
Name "Ferris Client"
OutFile "${FERRIS_OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Ferris Client"
InstallDirRegKey HKCU "Software\Ferris\Client" "InstallDir"
!include "MUI2.nsh"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\ferris-client.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Ferris Client"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Ferris Client" SEC_MAIN
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    File /oname=ferris-client.exe "${FERRIS_BINARY}"
    File /oname=THIRD_PARTY.md "${FERRIS_NOTICES}"
    File /oname=LICENSE.md "${FERRIS_LICENSE}"
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    CreateShortcut "$SMPROGRAMS\Ferris Client.lnk" "$INSTDIR\ferris-client.exe"
    WriteRegStr HKCU "Software\Ferris\Client" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "DisplayName" "Ferris Client"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "DisplayVersion" "${FERRIS_VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "Publisher" "Ferris"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient" "NoRepair" 1
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    Delete "$SMPROGRAMS\Ferris Client.lnk"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "FerrisClient"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FerrisClient"
    DeleteRegKey HKCU "Software\Ferris\Client"
    Delete "$INSTDIR\ferris-client.exe"
    Delete "$INSTDIR\THIRD_PARTY.md"
    Delete "$INSTDIR\LICENSE.md"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
    ; Settings, downloaded runtimes and vault credentials are retained. Sign out
    ; in the client first to revoke the server credential before uninstalling.
SectionEnd
