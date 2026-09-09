; Tauri inserts NSIS_HOOK_PREUNINSTALL at the start of Section Uninstall,
; before the generated uninstaller removes files, registry keys, or shortcuts.
!macro NSIS_HOOK_PREUNINSTALL
  ; A missing executable means restoration cannot be verified. Leave the
  ; installation intact and direct the user to repair or reinstall it.
  IfFileExists "$INSTDIR\controwly.exe" controwly_restore_start controwly_restore_missing

  controwly_restore_start:
    ; ExecWait sets the error flag when the executable cannot be launched and
    ; stores the process exit code in $0 when it does launch.
    ClearErrors
    ExecWait '"$INSTDIR\controwly.exe" --restore-and-exit' $0
    IfErrors controwly_restore_launch_failed
    ${If} $0 <> 0
      StrCpy $1 "Controwly could not restore controller state (exit code $0)."
      Goto controwly_restore_failed
    ${EndIf}
    Goto controwly_restore_succeeded

  controwly_restore_launch_failed:
    StrCpy $1 "Controwly could not be started to restore controller state."
    Goto controwly_restore_failed

  controwly_restore_missing:
    StrCpy $1 "The Controwly executable is missing, so controller state could not be restored."
    Goto controwly_restore_failed

  controwly_restore_failed:
    ; Error level 2 is NSIS's script-abort status. Do not let the generated
    ; uninstall section reach its first Delete/RMDir instruction.
    SetErrorLevel 2
    IfSilent controwly_restore_abort controwly_restore_notify

  controwly_restore_notify:
    MessageBox MB_OK|MB_ICONSTOP "$1$\n$\nClose Controwly and retry. If this persists, repair or reinstall Controwly before uninstalling."

  controwly_restore_abort:
    Abort

  controwly_restore_succeeded:
!macroend
