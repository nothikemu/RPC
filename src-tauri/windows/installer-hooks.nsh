; NSIS hooks for the DeskPresence installer (see tauri.conf.json → bundle.windows.nsis).
;
; The autostart plugin stores a value named after the product in the per-user
; Run key. Remove it on uninstall so Windows does not try to start a program
; that no longer exists. User configuration is NOT deleted here; the
; uninstaller's own "Delete the application data" checkbox controls that.

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "DeskPresence"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "DeskPresence"
!macroend
