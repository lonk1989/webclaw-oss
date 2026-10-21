/**
 * Smoke: download lite from an existing Release into a temp WEBCLAW_HOME.
 * Uses v0.1.6 assets until v0.1.7 is tagged (override via SMOKE_LITE_BASE).
 *
 *   node --import tsx/esm scripts/smoke-ensure.mts
 */
import { mkdtempSync, rmSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { ensureLite, PLUGIN_VERSION } from '../src/ensure-lite.ts'

const home = mkdtempSync(join(tmpdir(), 'webclaw-ensure-'))
const liteBaseUrl =
  process.env.SMOKE_LITE_BASE?.trim() ||
  'https://github.com/lonk1989/webclaw-oss/releases/download/v0.1.6'

console.log('PLUGIN_VERSION', PLUGIN_VERSION)
console.log('WEBCLAW_HOME', home)
console.log('liteBaseUrl', liteBaseUrl)

try {
  const result = await ensureLite(
    {
      binaryPath: 'webclaw-lite',
      webclawHome: home,
      liteBaseUrl,
    },
    console,
  )
  if (!existsSync(result.binaryPath)) {
    throw new Error(`missing binary at ${result.binaryPath}`)
  }
  console.log('ok', result)
} finally {
  try {
    rmSync(home, { recursive: true, force: true })
  } catch {
    /* ignore */
  }
}
