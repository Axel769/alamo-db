; NSIS installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).

; Since tauri-cli 2.12 the installer checks whether the app is running with a
; single RmShutdown and aborts with "Failed to kill Alamo DB" when it fails.
; During an update the process holding the exe is the app that has just
; launched this installer and is still exiting (std::process::exit in
; tauri-plugin-updater): the shutdown races with it and fails.
; Upstream: https://github.com/tauri-apps/tauri/issues/16277
; So, during updates only, wait for the exe to be free before that check (at
; most 20 s, every 250 ms). If it is still in use afterwards, the regular check
; runs as before.
!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode = 1
    Push $0
    Push $1
    Push $2
    Push $3
    Push $R0
    Push $R9
    StrCpy $R9 0
    alamo_wait_exe:
      StrCpy $0 ""
      !insertmacro RestartManager_StartSession $R0
      ${If} $R0 != ""
        !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${MAINBINARYNAME}.exe"
        ${If} $0 = 0
          ; ERROR_MORE_DATA: some process is still using the exe
          System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
        ${EndIf}
        !insertmacro RestartManager_EndSession $R0
      ${EndIf}
      ${If} $0 = ${ERROR_MORE_DATA}
      ${AndIf} $R9 < 80
        ${IfThen} $R9 = 0 ${|} DetailPrint "Waiting for ${PRODUCTNAME} to exit..." ${|}
        IntOp $R9 $R9 + 1
        Sleep 250
        Goto alamo_wait_exe
      ${EndIf}
    Pop $R9
    Pop $R0
    Pop $3
    Pop $2
    Pop $1
    Pop $0
  ${EndIf}
!macroend
