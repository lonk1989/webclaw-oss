//! 本机轻客户端：Skill/CLI 驱动的 RPA（无桌面 UI）。
//!
//! 构建：`cargo build --release --bin webclaw-lite --no-default-features`

fn main() {
    std::env::set_var("WEBCLAW_LITE", "1");
    webclaw_lib::lite::run_cli();
}
