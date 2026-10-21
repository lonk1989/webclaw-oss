/**
 * Bootstrap webclaw-lite from GitHub Releases (Windows x64).
 * npm ships only the Cordis plugin; the native CLI is downloaded on demand.
 * Download tries config → official → env mirrors → built-in CN-friendly mirrors.
 */

import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { runLite } from './spawn.js'

const require = createRequire(import.meta.url)
const PKG = require('../package.json') as { version: string }

/** npm package version — Release tag is `v${PLUGIN_VERSION}`. */
export const PLUGIN_VERSION = PKG.version

const EXE_NAME = 'webclaw-lite-windows-x64.exe'
const MANAGED_EXE = 'webclaw-lite.exe'
const VERSION_STAMP = '.webclaw-lite-npm-version'
const FETCH_TIMEOUT_MS = 60_000

/** Sentinel / empty binaryPath → use managed `%LOCALAPPDATA%/webclaw/bin/…`. */
const MANAGED_SENTINELS = new Set(['', 'webclaw-lite', 'webclaw-lite.exe'])

/**
 * Built-in mirrors that proxy the official GitHub Release directory.
 * Each entry is a prefix: `${mirror}${officialPath}` where officialPath starts with https://github.com/...
 */
const BUILTIN_MIRROR_PREFIXES = [
  'https://ghfast.top/',
  'https://mirror.ghproxy.com/',
] as const

export interface EnsureLiteConfig {
  readonly binaryPath: string
  readonly webclawHome: string
  /** Release directory prefix (no filename). Empty → multi-source default chain. */
  readonly liteBaseUrl: string
}

export interface EnsureLiteResult {
  readonly binaryPath: string
  readonly webclawHome: string
  /** Base that succeeded (or preferred config/official when no download). */
  readonly liteBaseUrl: string
  readonly downloaded: boolean
}

export type EnsureLog = {
  info: (msg: string, ...args: unknown[]) => void
  warn: (msg: string, ...args: unknown[]) => void
}

let inflight: Promise<EnsureLiteResult> | undefined

/**
 * Official GitHub Releases directory for this plugin version (no trailing slash).
 */
export function defaultLiteBaseUrl(version = PLUGIN_VERSION): string {
  return `https://github.com/lonk1989/webclaw-oss/releases/download/v${version}`
}

function normalizeBase(url: string): string {
  return url.trim().replace(/\/+$/, '')
}

function isGithubReleaseDir(url: string): boolean {
  return /https?:\/\/github\.com\//i.test(url)
}

/**
 * Candidate Release directory prefixes, ordered for try-in-order download.
 * Direct github.com is last so CN users hit ghfast.top first (avoids a long dead wait).
 * 1. config.liteBaseUrl when it is not github.com (self-hosted / CDN)
 * 2. WEBCLAW_LITE_MIRRORS (comma-separated release dirs or proxy prefixes)
 * 3. built-in mirrors (ghfast.top, then ghproxy) for each github primary
 * 4. github.com primaries (custom liteBaseUrl if github, then official)
 */
export function candidateLiteBaseUrls(config: EnsureLiteConfig): string[] {
  const official = defaultLiteBaseUrl()
  const out: string[] = []
  const seen = new Set<string>()
  const push = (raw: string): void => {
    const b = normalizeBase(raw)
    if (!b || seen.has(b)) return
    seen.add(b)
    out.push(b)
  }

  const githubPrimaries: string[] = []
  const custom = config.liteBaseUrl?.trim()
  if (custom) {
    const normalized = normalizeBase(custom)
    if (isGithubReleaseDir(normalized)) {
      githubPrimaries.push(normalized)
    } else {
      push(normalized)
    }
  }
  githubPrimaries.push(official)

  const envMirrors = process.env.WEBCLAW_LITE_MIRRORS?.trim()
  if (envMirrors) {
    for (const part of envMirrors.split(',')) {
      const p = part.trim()
      if (!p) continue
      if (/^https?:\/\//i.test(p) && p.includes('github.com')) {
        push(p)
      } else if (/^https?:\/\//i.test(p)) {
        const prefix = p.endsWith('/') ? p : `${p}/`
        for (const primary of githubPrimaries) {
          push(`${prefix}${primary}`)
        }
      }
    }
  }

  for (const primary of githubPrimaries) {
    for (const prefix of BUILTIN_MIRROR_PREFIXES) {
      push(`${prefix}${primary}`)
    }
  }

  for (const primary of githubPrimaries) {
    push(primary)
  }

  return out
}

/**
 * Resolve WEBCLAW_HOME (config → env → %LOCALAPPDATA%/webclaw | ~/.webclaw).
 */
export function resolveWebclawHome(config: EnsureLiteConfig): string {
  const fromConfig = config.webclawHome?.trim()
  if (fromConfig) return fromConfig
  const fromEnv = process.env.WEBCLAW_HOME?.trim()
  if (fromEnv) return fromEnv
  if (process.platform === 'win32') {
    const local = process.env.LOCALAPPDATA?.trim()
    if (local) return join(local, 'webclaw')
    return join(homedir(), 'AppData', 'Local', 'webclaw')
  }
  return join(homedir(), '.webclaw')
}

/**
 * Managed install path for the lite CLI.
 */
export function managedBinaryPath(home: string): string {
  if (process.platform === 'win32') {
    return join(home, 'bin', MANAGED_EXE)
  }
  return join(home, 'bin', 'webclaw-lite')
}

/**
 * Whether config asks for the managed binary (auto-download eligible).
 */
export function usesManagedBinary(binaryPath: string): boolean {
  return MANAGED_SENTINELS.has(binaryPath.trim())
}

/**
 * Effective binary path: managed home/bin or user override.
 */
export function resolveBinaryPath(config: EnsureLiteConfig): string {
  if (!usesManagedBinary(config.binaryPath)) {
    return config.binaryPath.trim()
  }
  return managedBinaryPath(resolveWebclawHome(config))
}

/**
 * Preferred Release prefix when no download runs (config or official).
 */
export function resolveLiteBaseUrl(config: EnsureLiteConfig): string {
  return candidateLiteBaseUrls(config)[0] ?? defaultLiteBaseUrl()
}

function stampPath(home: string): string {
  return join(home, 'bin', VERSION_STAMP)
}

function stampMatches(home: string, version: string): boolean {
  try {
    return readFileSync(stampPath(home), 'utf8').trim() === version
  } catch {
    return false
  }
}

function writeStamp(home: string, version: string): void {
  mkdirSync(join(home, 'bin'), { recursive: true })
  writeFileSync(stampPath(home), `${version}\n`, 'utf8')
}

function parseSha256File(text: string): string {
  const line = text.trim().split(/\r?\n/)[0]?.trim() ?? ''
  const hex = line.split(/\s+/)[0] ?? ''
  if (!/^[a-fA-F0-9]{64}$/.test(hex)) {
    throw new Error(`invalid sha256 file content: ${line.slice(0, 80)}`)
  }
  return hex.toLowerCase()
}

async function download(url: string, timeoutMs = FETCH_TIMEOUT_MS): Promise<Buffer> {
  const ac = new AbortController()
  const timer = setTimeout(() => ac.abort(), timeoutMs)
  try {
    const res = await fetch(url, {
      redirect: 'follow',
      signal: ac.signal,
      headers: { 'user-agent': `webclaw-plugin/${PLUGIN_VERSION}` },
    })
    if (!res.ok) {
      throw new Error(`download failed ${res.status} ${res.statusText}: ${url}`)
    }
    return Buffer.from(await res.arrayBuffer())
  } catch (error) {
    if (error instanceof Error && error.name === 'AbortError') {
      throw new Error(`download timed out after ${timeoutMs}ms: ${url}`)
    }
    throw error
  } finally {
    clearTimeout(timer)
  }
}

async function downloadFromBase(
  home: string,
  baseUrl: string,
  log: EnsureLog,
): Promise<void> {
  const exeUrl = `${normalizeBase(baseUrl)}/${EXE_NAME}`
  const shaUrl = `${exeUrl}.sha256`
  log.info('webclaw: downloading %s', exeUrl)

  const [body, shaText] = await Promise.all([
    download(exeUrl),
    download(shaUrl).then(b => b.toString('utf8')),
  ])
  const expected = parseSha256File(shaText)
  const actual = createHash('sha256').update(body).digest('hex')
  if (actual !== expected) {
    throw new Error(
      `sha256 mismatch for ${EXE_NAME}: expected ${expected}, got ${actual}`,
    )
  }

  const binDir = join(home, 'bin')
  mkdirSync(binDir, { recursive: true })
  const dest = managedBinaryPath(home)
  const tmp = `${dest}.download`
  writeFileSync(tmp, body)
  try {
    if (existsSync(dest)) unlinkSync(dest)
  } catch {
    /* replace below */
  }
  renameSync(tmp, dest)
  writeStamp(home, PLUGIN_VERSION)
  log.info('webclaw: installed %s (%s) from %s', dest, PLUGIN_VERSION, baseUrl)
}

/**
 * Try each candidate base until one succeeds; return the winning base.
 */
async function downloadManagedWithFallback(
  home: string,
  bases: readonly string[],
  log: EnsureLog,
): Promise<string> {
  if (process.platform !== 'win32') {
    throw new Error(
      'webclaw-lite auto-install currently supports Windows x64 only. ' +
        'Build from source or set config.binaryPath to a local binary. ' +
        'See https://github.com/lonk1989/webclaw-oss',
    )
  }

  const errors: string[] = []
  for (const base of bases) {
    try {
      await downloadFromBase(home, base, log)
      return base
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error)
      log.warn('webclaw: source failed (%s): %s', base, msg.slice(0, 200))
      errors.push(`${base}: ${msg}`)
    }
  }
  throw new Error(
    `webclaw-lite download failed from all sources:\n${errors.join('\n')}`,
  )
}

async function runInstallMarker(
  binaryPath: string,
  home: string,
  liteBaseUrl: string,
  log: EnsureLog,
): Promise<void> {
  try {
    const result = await runLite(
      {
        binaryPath,
        webclawHome: home,
        liteBaseUrl,
        timeoutMs: 120_000,
      },
      ['install'],
    )
    if (result.code === 0) {
      log.info('webclaw: lite install marker ok')
    } else {
      log.warn(
        'webclaw: lite install marker failed: %s',
        (result.stderr || result.stdout || 'non-zero').slice(0, 300),
      )
    }
  } catch (error) {
    log.warn(
      'webclaw: lite install marker error: %s',
      error instanceof Error ? error.message : error,
    )
  }
}

/**
 * Ensure webclaw-lite exists (download from Releases when using managed path).
 * Single-flight across concurrent tool calls.
 */
export function ensureLite(
  config: EnsureLiteConfig,
  log: EnsureLog = console,
): Promise<EnsureLiteResult> {
  if (!inflight) {
    inflight = doEnsure(config, log).finally(() => {
      inflight = undefined
    })
  }
  return inflight
}

async function doEnsure(
  config: EnsureLiteConfig,
  log: EnsureLog,
): Promise<EnsureLiteResult> {
  const home = resolveWebclawHome(config)
  const bases = candidateLiteBaseUrls(config)
  const preferredBase = bases[0] ?? defaultLiteBaseUrl()
  mkdirSync(join(home, 'bin'), { recursive: true })

  if (!usesManagedBinary(config.binaryPath)) {
    const binaryPath = config.binaryPath.trim()
    if (!existsSync(binaryPath)) {
      throw new Error(
        `webclaw-lite not found at binaryPath=${binaryPath}. ` +
          'Install the CLI, or clear binaryPath to use GitHub Releases auto-install.',
      )
    }
    await runInstallMarker(binaryPath, home, preferredBase, log)
    return {
      binaryPath,
      webclawHome: home,
      liteBaseUrl: preferredBase,
      downloaded: false,
    }
  }

  const dest = managedBinaryPath(home)
  let liteBaseUrl = preferredBase
  let downloaded = false
  if (!existsSync(dest) || !stampMatches(home, PLUGIN_VERSION)) {
    let tryBases = bases
    if (process.env.SMOKE_FORCE_MIRROR === '1') {
      // Skip official for mirror-path smoke; keep config custom if any.
      tryBases = bases.filter(b => !b.startsWith('https://github.com/'))
      if (tryBases.length === 0) tryBases = bases
    }
    liteBaseUrl = await downloadManagedWithFallback(home, tryBases, log)
    downloaded = true
  }
  await runInstallMarker(dest, home, liteBaseUrl, log)
  return {
    binaryPath: dest,
    webclawHome: home,
    liteBaseUrl,
    downloaded,
  }
}
