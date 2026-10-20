//! 伙伴空间 / 无头 CDP 执行入口（无桌面 UI）。
//!
//! 构建：`cargo build --release --bin webclaw-worker`
//! Linux：`cargo build --release --bin webclaw-worker --target x86_64-unknown-linux-gnu`

fn main() {
    webclaw_lib::worker::run_cli();
}
