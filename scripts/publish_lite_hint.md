# Publishing lite binaries

1. `cargo build --release --bin webclaw-lite --manifest-path runtime/Cargo.toml`
2. Upload Release assets (with matching `.sha256`):
   - Windows: `target/release/webclaw-lite.exe` → `webclaw-lite-windows-x64.exe`
   - macOS arm64: `target/release/webclaw-lite` → `webclaw-lite-macos-arm64`
3. Users set `WEBCLAW_LITE_BASE` to the Release asset base URL (or `WEBCLAW_WORKER_URL` to the full exe URL)
4. Prefer tagging `v*` so `release.yml` builds and uploads both platforms automatically
5. See [docs/PUBLISH.md](../docs/PUBLISH.md)
