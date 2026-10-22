/**
 * Local DSH sync (no npm publish):
 * 1) cargo build webclaw-lite → %LOCALAPPDATA%/webclaw/bin/webclaw-lite.exe
 * 2) hot-copy this package into fengxing bundle node_modules
 *
 * Default plugin dest:
 *   D:/tools/harness-apps/fengxing/bundle/node_modules/@lonk1989/webclaw
 *
 * Env:
 *   WEBCLAW_SYNC_DEST     — override plugin install dir
 *   WEBCLAW_SYNC_SKIP_LITE=1 — only copy plugin (skip cargo)
 *   WEBCLAW_HOME          — lite home (default %LOCALAPPDATA%/webclaw)
 *
 * Flags: --skip-lite
 *
 *   npm run sync:local
 *   npm run sync:local:plugin
 */
import { spawnSync } from 'node:child_process'
import {
  cpSync,
  existsSync,
  mkdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), '..')
const repoRoot = join(pkgRoot, '..', '..')
const DEFAULT_DEST =
  'D:/tools/harness-apps/fengxing/bundle/node_modules/@lonk1989/webclaw'

function fail(msg) {
  console.error(`sync-local: ${msg}`)
  process.exit(1)
}

function resolveCargo() {
  const which = spawnSync(
    process.platform === 'win32' ? 'where' : 'which',
    ['cargo'],
    { encoding: 'utf8', shell: true },
  )
  const first = which.stdout?.trim().split(/\r?\n/)[0]
  if (first && existsSync(first)) return first

  const home = process.env.USERPROFILE || process.env.HOME || homedir()
  const candidates = [
    join(home, '.cargo', 'bin', process.platform === 'win32' ? 'cargo.exe' : 'cargo'),
    join(
      home,
      '.rustup',
      'toolchains',
      'stable-x86_64-pc-windows-msvc',
      'bin',
      'cargo.exe',
    ),
  ]
  for (const c of candidates) {
    if (existsSync(c)) return c
  }
  return null
}

function defaultLiteHome() {
  if (process.env.WEBCLAW_HOME?.trim()) return process.env.WEBCLAW_HOME.trim()
  if (process.platform === 'win32') {
    const local = process.env.LOCALAPPDATA?.trim()
    if (local) return join(local, 'webclaw')
  }
  return join(homedir(), '.webclaw')
}

const skipLite =
  process.env.WEBCLAW_SYNC_SKIP_LITE === '1' ||
  process.argv.includes('--skip-lite')

function buildAndInstallLite() {
  if (skipLite) {
    console.log('sync-local: skip lite (--skip-lite / WEBCLAW_SYNC_SKIP_LITE=1)')
    return null
  }

  const cargo = resolveCargo()
  if (!cargo) {
    fail(
      'cargo not found. Install Rust, or set WEBCLAW_SYNC_SKIP_LITE=1 to sync plugin only.',
    )
  }

  const manifest = join(repoRoot, 'runtime', 'Cargo.toml')
  if (!existsSync(manifest)) {
    fail(`runtime Cargo.toml missing: ${manifest}`)
  }

  console.log(`sync-local: cargo build --release --bin webclaw-lite`)
  const build = spawnSync(
    cargo,
    ['build', '--release', '--bin', 'webclaw-lite', '--manifest-path', manifest],
    { cwd: repoRoot, stdio: 'inherit', shell: false, env: process.env },
  )
  if (build.status !== 0) {
    fail(`cargo build failed (exit ${build.status ?? '?'})`)
  }

  const built = join(repoRoot, 'target', 'release', 'webclaw-lite.exe')
  const builtUnix = join(repoRoot, 'target', 'release', 'webclaw-lite')
  const src = existsSync(built) ? built : existsSync(builtUnix) ? builtUnix : null
  if (!src) fail(`built binary missing under target/release/`)

  const home = defaultLiteHome()
  const binDir = join(home, 'bin')
  mkdirSync(binDir, { recursive: true })
  const destName =
    process.platform === 'win32' ? 'webclaw-lite.exe' : 'webclaw-lite'
  const destLite = join(binDir, destName)
  try {
    cpSync(src, destLite)
  } catch (e) {
    // Windows: exe may be locked — write beside then replace
    const tmp = `${destLite}.new`
    cpSync(src, tmp)
    try {
      rmSync(destLite, { force: true })
    } catch {
      /* ignore */
    }
    cpSync(tmp, destLite)
    try {
      rmSync(tmp, { force: true })
    } catch {
      /* ignore */
    }
    console.warn(`sync-local: lite copy used replace path (${String(e?.message || e)})`)
  }

  console.log(`sync-local: lite → ${destLite.replace(/\\/g, '/')}`)
  return destLite
}

function syncPlugin() {
  const dest = (process.env.WEBCLAW_SYNC_DEST || DEFAULT_DEST).trim()
  if (!dest) fail('WEBCLAW_SYNC_DEST is empty')

  const lib = join(pkgRoot, 'lib')
  if (!existsSync(join(lib, 'index.js'))) {
    fail('lib/index.js missing — run npm run build first')
  }

  mkdirSync(dirname(dest), { recursive: true })
  rmSync(dest, { recursive: true, force: true })
  mkdirSync(dest, { recursive: true })

  const copyFile = (name) => {
    const from = join(pkgRoot, name)
    if (!existsSync(from)) return
    cpSync(from, join(dest, name), { recursive: true })
  }

  cpSync(lib, join(dest, 'lib'), { recursive: true })
  copyFile('package.json')
  copyFile('cordis.patch.yml')
  copyFile('README.md')
  copyFile('assets')

  writeFileSync(
    join(dest, '.webclaw-sync-local'),
    `syncedAt=${new Date().toISOString()}\nsource=${pkgRoot.replace(/\\/g, '/')}\nliteBuilt=${skipLite ? 'skipped' : 'yes'}\n`,
    'utf8',
  )

  console.log(`sync-local: plugin ${pkgRoot.replace(/\\/g, '/')} → ${dest}`)
  return dest
}

const litePath = buildAndInstallLite()
syncPlugin()
console.log('sync-local: done — restart DSH to pick up changes')
if (litePath) {
  console.log(`sync-local: lite binary: ${litePath.replace(/\\/g, '/')}`)
}
