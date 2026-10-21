# Publishing webclaw-oss to the DSH community

## One-time setup

1. Create a GitHub repository (suggested name `webclaw-oss`).
2. Push this tree; add repository topic **`dsh-plugin`** (required for [DSH discoverability](https://github.com/topics/dsh-plugin)).
3. Create npm scope/user that can publish **`@lonk1989/webclaw`**.
4. Prefer **npm Trusted Publishing** (OIDC) over long-lived tokens:
   - On https://www.npmjs.com/package/@lonk1989/webclaw → **Settings** → **Trusted Publisher** → GitHub Actions  
   - Organization/user: `lonk1989` · Repository: `webclaw-oss` · Workflow filename: `release.yml`  
   - Allow `npm publish`  
   - Do **not** use a Granular Token with “Bypass 2FA” for CI (npm warns against it).

## Release flow

Keep **npm `packages/webclaw` version** and the **git tag** in lockstep (e.g. package `0.1.8` → tag `v0.1.8`).  
The Cordis plugin auto-downloads  
`https://github.com/lonk1989/webclaw-oss/releases/download/v{version}/webclaw-lite-windows-x64.exe`  
(with built-in mirror fallbacks for unstable GitHub access).

```sh
# after bumping packages/webclaw/package.json version
git tag v0.1.8
git push origin v0.1.8
```

GitHub Actions (`release.yml`) will:

- Build `webclaw-lite-windows-x64.exe` (+ `.sha256`) and attach them to the GitHub Release
- `npm publish` `@lonk1989/webclaw` via Trusted Publishing (OIDC)

## Manual publish

```powershell
cd packages/webclaw
npm run build
npm publish --access public
```

```powershell
cd ../..
cargo build --release --bin webclaw-lite --manifest-path runtime/Cargo.toml
# upload target/release/webclaw-lite.exe to the Release
```

## After publish

- Announce in [DeepSeek Harness Discussions](https://github.com/deepseek-ai/deepseek-harness/discussions)
- Point users at `examples/cordis.patch.yml`
