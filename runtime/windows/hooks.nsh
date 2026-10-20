; webclaw NSIS hooks：安装时补齐 Visual C++ 2015–2022 x64 运行库
; （Tauri 默认只处理 WebView2，不装 VC++）

!macro NSIS_HOOK_POSTINSTALL
  ; VC++ 2015–2022 Redistributable (x64)
  ReadRegDWord $0 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Installed"
  ${If} $0 == 1
    DetailPrint "Visual C++ Redistributable (x64) already installed"
    Goto vcredist_done
  ${EndIf}

  ${If} ${FileExists} "$INSTDIR\resources\vc_redist.x64.exe"
    DetailPrint "Installing Visual C++ Redistributable (x64)..."
    CopyFiles "$INSTDIR\resources\vc_redist.x64.exe" "$TEMP\webclaw_vc_redist.x64.exe"
    ExecWait '"$TEMP\webclaw_vc_redist.x64.exe" /install /passive /norestart' $0
    ${If} $0 == 0
      DetailPrint "Visual C++ Redistributable installed successfully"
    ${ElseIf} $0 == 1638
      ; Already installed (newer or same)
      DetailPrint "Visual C++ Redistributable already present (exit 1638)"
    ${ElseIf} $0 == 3010
      ; Success, reboot required
      DetailPrint "Visual C++ Redistributable installed (reboot recommended)"
    ${Else}
      MessageBox MB_ICONEXCLAMATION "Visual C++ 运行库安装失败 (code $0)。$\r$\n若启动报缺少 VCRUNTIME140.dll，请手动安装：$\r$\nhttps://aka.ms/vs/17/release/vc_redist.x64.exe"
    ${EndIf}
    Delete "$TEMP\webclaw_vc_redist.x64.exe"
    ; 安装包内的 redist 仅供安装器用，装完后可删以节省磁盘
    Delete "$INSTDIR\resources\vc_redist.x64.exe"
  ${Else}
    DetailPrint "vc_redist.x64.exe not bundled; skip VC++ install"
  ${EndIf}

  vcredist_done:
  ; ONNX Runtime 按文件名加载 DirectML.dll，只搜索 exe 所在目录，不搜索 resources\
  ${If} ${FileExists} "$INSTDIR\resources\DirectML.dll"
    ; 跳过占位空文件（开发机 cargo check 可能写出 0 字节）
    FileOpen $1 "$INSTDIR\resources\DirectML.dll" r
    FileSeek $1 0 END $2
    FileClose $1
    ${If} $2 > 1024
      DetailPrint "Installing DirectML.dll next to webclaw.exe"
      CopyFiles /SILENT "$INSTDIR\resources\DirectML.dll" "$INSTDIR\DirectML.dll"
    ${Else}
      DetailPrint "DirectML.dll placeholder skipped (size $2)"
    ${EndIf}
    Delete "$INSTDIR\resources\DirectML.dll"
  ${Else}
    DetailPrint "DirectML.dll not bundled"
  ${EndIf}
!macroend
