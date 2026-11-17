; The Windows installer of Partitur.
;
; Built by the release workflow from the repository root:
;   makensis -NOCD -DVERSION=<version> packaging\windows\installer.nsi
; -NOCD keeps the working directory at the root, so the File, Icon and OutFile paths below are
; relative to it: the program and its two documents come from the staging folder `stage\`.
;
; It installs partitur.exe, LICENSE and README.md into Program Files\Partitur, adds one Start
; menu shortcut for every user and an entry in Settings > Apps, and writes the uninstaller that
; takes all of that back. Nothing else is installed: the C runtime is linked into partitur.exe
; (.cargo/config.toml), and the program is started from its shortcut, so it stays off PATH.
; Neither the installer nor the uninstaller touches the user's configuration and data under
; %APPDATA%\mhert\partitur: an uninstall leaves them for a later install to find.

Unicode true

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\mhert.partitur"

Name "Partitur"
Caption "Partitur ${VERSION} Setup"
OutFile "partitur-${VERSION}-x86_64-windows-setup.exe"
InstallDir "$PROGRAMFILES64\Partitur"
RequestExecutionLevel admin
Icon "packaging\windows\partitur.ico"
UninstallIcon "packaging\windows\partitur.ico"

Page directory "" "" VerifyInstDirIsOurs
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles
ShowInstDetails show
ShowUninstDetails show

; The directory chosen here becomes this program's own, and the only question the uninstaller
; asks about it is whether partitur.exe is in it. So a directory belongs to this install if it
; does not exist yet, if it is empty, or if it already holds partitur.exe, which is what lets an
; existing install be reinstalled or upgraded in place. Anything else is somebody else's
; directory: C:\Program Files itself, or a folder of documents. Installing into one of those
; would leave partitur.exe and an uninstaller among files that are not ours, so the refusal has
; to come before the first file is written rather than at uninstall time.
;
; A leave function on the directory page, not .onVerifyInstDir: NSIS runs .onVerifyInstDir on
; every keystroke and its documentation says such a function must not put up a MessageBox, so the
; most it could do is grey the Next button out and leave the user to guess why. A leave function
; runs once, when Next is pressed, where the reason can be said out loud and Abort keeps the user
; on the page with the directory still theirs to change. Neither mechanism runs in a silent
; install, which shows no pages at all: an installer run with /S takes the directory it is given.
Function VerifyInstDirIsOurs
  IfFileExists "$INSTDIR\partitur.exe" install_here
  IfFileExists "$INSTDIR\*.*" 0 install_here

  ; Every directory but a drive root lists "." and ".." of its own, so an empty one is the search
  ; that turns up nothing besides those two.
  FindFirst $0 $1 "$INSTDIR\*.*"
  next_entry:
    StrCmp $1 "" dir_is_empty
    StrCmp $1 "." skip_entry
    StrCmp $1 ".." skip_entry
      FindClose $0
      MessageBox MB_OK|MB_ICONSTOP "Partitur will not install into $INSTDIR, because that folder already holds files that are not part of a Partitur installation. Please choose an empty folder or a new one."
      Abort
    skip_entry:
    FindNext $0 $1
    Goto next_entry
  dir_is_empty:
  FindClose $0
  install_here:
FunctionEnd

Section "Install"
  ; NSIS builds this installer against a 32-bit stub, and a 32-bit program on 64-bit Windows reads
  ; and writes the 32-bit view of the registry: the key below would land under WOW6432Node
  ; instead, where Settings > Apps and every other 64-bit reader of the uninstall list will not
  ; look. This installs an x86_64 program into $PROGRAMFILES64, so it only runs on 64-bit Windows,
  ; where the 64-bit view exists.
  SetRegView 64
  ; The install is for the machine, so the shortcut goes into the Start menu every user sees
  ; (%ProgramData%\Microsoft\Windows\Start Menu\Programs), not only the installing account's.
  SetShellVarContext all

  SetOutPath "$INSTDIR"
  File "stage\partitur.exe"
  File "stage\LICENSE"
  File "stage\README.md"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; So the install shows up in Settings > Apps and can be removed from there, silently too.
  ; NoModify and NoRepair: there are no parts to pick and nothing to repair, so Settings offers
  ; only Uninstall.
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "Partitur"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "Mathias Hertlein"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\partitur.exe,0"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1

  ; The shortcut is the way in, so it is what the install ends with: one entry in the Start menu
  ; rather than a folder of its own, because there is a single program to start. It shows the
  ; icon built into partitur.exe.
  CreateShortcut "$SMPROGRAMS\Partitur.lnk" "$INSTDIR\partitur.exe" "" "$INSTDIR\partitur.exe" 0
SectionEnd

Section "Uninstall"
  ; The view and the Start menu the install wrote to. Left at their defaults, the DeleteRegKey
  ; below would look under WOW6432Node and the Delete in the installing account's own Start
  ; menu, find nothing, and leave both behind.
  SetRegView 64
  SetShellVarContext all

  ; In an uninstaller $INSTDIR is not what the directory page was set to: it is the directory the
  ; uninstaller is run from, and the documented _?= switch overrides even that. A copy of
  ; uninstall.exe carried into a downloads folder would therefore aim the deletes below at that
  ; folder. So the uninstall runs only where the program it removes actually is; anywhere else it
  ; says so and stops before touching anything, leaving the install whole and removable from
  ; where it was put.
  IfFileExists "$INSTDIR\partitur.exe" dir_is_ours
    DetailPrint "Partitur: $INSTDIR holds no partitur.exe, so this uninstaller will not remove it."
    Abort
  dir_is_ours:

  Delete "$SMPROGRAMS\Partitur.lnk"

  ; The files are taken back by name, the four this install wrote, and the folder only when that
  ; leaves it empty: anything else found in it is not ours to delete. /REBOOTOK covers a
  ; partitur.exe a running Partitur still holds open, which Windows removes at the next start
  ; rather than leaving a half-removed folder with nothing left to retry it.
  Delete /REBOOTOK "$INSTDIR\partitur.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\README.md"
  Delete /REBOOTOK "$INSTDIR\uninstall.exe"
  RMDir /REBOOTOK "$INSTDIR"

  DeleteRegKey HKLM "${UNINSTALL_KEY}"
SectionEnd
