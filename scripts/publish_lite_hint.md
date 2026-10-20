# Publishing lite binaries

1. `cargo build --release --bin webclaw-lite --manifest-path runtime/Cargo.toml`
2. Upload `target/release/webclaw-lite.exe` as `webclaw-lite-windows-x64.exe` on the GitHub Release
3. Users set `WEBCLAW_LITE_BASE` to the Release asset base URL (or `WEBCLAW_WORKER_URL` to the full exe URL)
4. See [docs/PUBLISH.md](../docs/PUBLISH.md)
