// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // VoipEngine 解码放在独立进程，避免 DLL 崩溃拖垮主界面
    if webclaw_lib::rhai_runtime::wxgf_voip::run_cli_if_requested() {
        return;
    }
    webclaw_lib::run()
}
