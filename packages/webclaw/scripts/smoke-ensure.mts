/**
 * Smoke: download lite into a temp WEBCLAW_HOME.
 *
 * Default pins an existing Release (v0.1.6) via liteBaseUrl; mirrors wrap that URL too.
 * SMOKE_FORCE_MIRROR=1 skips direct github.com bases (tests mirror path only).
 *
 *   npx tsx scripts/smoke-ensure.mts
 *   $env:SMOKE_FORCE_MIRROR='1'; npx tsx scripts/smoke-ensure.mts
 */
import { mkdtempSync, rmSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  candidateLiteBaseUrls,
  ensureLite,
  PLUGIN_VERSION,
} from '../src/ensure-lite.ts'

const home = mkdtempSync(join(tmpdir(), 'webclaw-ensure-'))
const forceMirror = process.env.SMOKE_FORCE_MIRROR === '1'
const liteBaseUrl =
  process.env.SMOKE_LITE_BASE?.trim() ||
  'https://github.com/lonk1989/webclaw-oss/releases/download/v0.1.6'

const config = {
  binaryPath: 'webclaw-lite',
  webclawHome: home,
  liteBaseUrl,
}

console.log('PLUGIN_VERSION', PLUGIN_VERSION)
console.log('WEBCLAW_HOME', home)
console.log('SMOKE_FORCE_MIRROR', forceMirror)
console.log('candidates', candidateLiteBaseUrls(config))

try {
  const result = await ensureLite(config, console)
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
