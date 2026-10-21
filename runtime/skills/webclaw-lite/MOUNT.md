# Mount webclaw-lite in DeepSeek Harness

1. Build or download `webclaw-lite`
2. `pnpm add @lonk1989/webclaw` (in the DSH workspace / web profile)
3. Apply an `insert:` patch with `id: webclaw` and `name: '@lonk1989/webclaw'` (see `examples/cordis.patch.yml`)
4. Set `binaryPath` to your `webclaw-lite` binary
5. Add GitHub topic `dsh-plugin` on the plugin repo for discovery
