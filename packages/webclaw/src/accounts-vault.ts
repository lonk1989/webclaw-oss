/**
 * Local vault helpers via webclaw-lite accounts CLI (never for LLM tools that
 * accept passwords — Host UI / loopback server only).
 */

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { runLite, type LiteEnv } from './spawn.js'

export interface AccountStatus {
  id: string
  name: string
  url: string
  username: string
  has_password: boolean
}

/** True when vault URL is SSO / OAuth — password is not required. */
export function isSsoAccountUrl(url: string): boolean {
  const u = normalizeSystemUrl(url).toLowerCase()
  if (!u) return false
  return (
    /[?&]sso=/.test(u) ||
    u.includes('/sso') ||
    u.includes('oauth') ||
    u.includes('openid') ||
    u.includes('sso=lark') ||
    u.includes('passport.feishu') ||
    u.includes('accounts.feishu') ||
    u.includes('accounts.larksuite')
  )
}

/**
 * Soft hint when a password form login fails.
 * Never paste `accounts set` CLI or ask for passwords in chat.
 * SSO sites only need the correct login URL in 设置 → 网站账号.
 */
function accountsLauncherHint(): string {
  if (process.platform === 'win32') {
    return '%LOCALAPPDATA%\\webclaw\\bin\\webclaw-accounts.cmd'
  }
  return '~/.webclaw/bin/webclaw-accounts'
}

export const CREDENTIALS_SETUP_HINT =
  '多数站点（含飞书 SSO）只需正确的登录 URL，不要要密码、不要发 accounts set 命令。\n' +
  '请用户在 DSH「设置 → 网站账号」把 EHR URL 设为 SSO 入口（例如 …/login?sso=lark&next=…），保存即可。\n' +
  `也可打开 ${accountsLauncherHint()}。禁止 probe / 禁止在对话里要密码。`

/**
 * Fix YAML / double-encoding: `\\u0026` → `&` so SSO query strings stay intact.
 */
export function normalizeSystemUrl(raw: string): string {
  let s = (raw || '').trim()
  // YAML/ Cordis may leave `\u0026` or `\\u0026` instead of `&`
  while (/\\+u0026/.test(s)) {
    s = s.replace(/\\+u0026/g, '&')
  }
  return s
}

/**
 * Extract「登录…」targets from a recipe body (Chinese/ASCII commas).
 */
export function extractLoginQueries(content: string): string[] {
  const text = content.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n')
  const parts = text.split(/[，,]/).map(s => s.trim()).filter(Boolean)
  const out: string[] = []
  const seen = new Set<string>()
  for (const part of parts) {
    if (!part.startsWith('登录')) continue
    const q = part.slice('登录'.length).trim()
    if (!q || seen.has(q)) continue
    seen.add(q)
    out.push(q)
  }
  return out
}

/**
 * Soft status: missing vault URL for a login target.
 * SSO URLs without password are OK.
 */
export function missingCredentialsForRecipe(
  content: string,
  items: readonly AccountStatus[],
): Array<{ query: string; id: string; name: string; url: string; reason: string }> {
  const queries = extractLoginQueries(content)
  const out: Array<{
    query: string
    id: string
    name: string
    url: string
    reason: string
  }> = []
  for (const query of queries) {
    const hit = items.find(
      it =>
        it.id.toLowerCase() === query.toLowerCase() ||
        it.name === query ||
        it.name.replace(/系统$/u, '') === query.replace(/系统$/u, ''),
    )
    if (!hit) {
      out.push({
        query,
        id: '',
        name: query,
        url: '',
        reason: 'no_vault_entry',
      })
      continue
    }
    if (isSsoAccountUrl(hit.url)) continue
    if (!hit.url.trim()) {
      out.push({
        query,
        id: hit.id,
        name: hit.name,
        url: hit.url,
        reason: 'missing_url',
      })
    }
    // URL-only entries are fine to open; password only needed for form login.
  }
  return out
}

function parseJsonObject(stdout: string): Record<string, unknown> | undefined {
  const lines = stdout
    .split(/\r?\n/)
    .map(l => l.trim())
    .filter(Boolean)
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i]!
    try {
      const row = JSON.parse(line) as Record<string, unknown>
      if (row && typeof row === 'object') return row
    } catch {
      continue
    }
  }
  return undefined
}

function mapItem(row: Record<string, unknown>): AccountStatus {
  const it = (row.item as Record<string, unknown> | undefined) ?? row
  return {
    id: String(it.id ?? ''),
    name: String(it.name ?? ''),
    url: normalizeSystemUrl(String(it.url ?? '')),
    username: String(it.username ?? ''),
    has_password: Boolean(it.has_password ?? it.hasPassword),
  }
}

/** List vault entries (no password values). */
export async function listAccounts(
  env: LiteEnv,
): Promise<{ ok: boolean; items: AccountStatus[]; error?: string }> {
  const result = await runLite(env, ['accounts', 'list'])
  const parsed = parseJsonObject(result.stdout)
  if (result.code === 0 && parsed) {
    const itemsRaw = parsed.items
    const items = Array.isArray(itemsRaw)
      ? itemsRaw.map(x => mapItem(x as Record<string, unknown>))
      : []
    return { ok: true, items }
  }
  return {
    ok: false,
    items: [],
    error: (result.stderr || result.stdout || 'accounts list failed').slice(0, 500),
  }
}

interface DiskSystem {
  id: string
  name: string
  kind?: string
  url: string
  username?: string
  password?: string
  updated_at?: string
  source?: string
  remark?: string
  remote_status?: number | null
}

/**
 * Write deploy `systems` URLs (and optional username) into WEBCLAW_HOME/web-systems.json.
 * Avoids CLI/`&` truncation and keeps Local source so login prefers overlay URLs.
 * Passwords are NOT written here — use `setAccount` so lite encrypts via DPAPI.
 */
export function patchSystemsFile(
  webclawHome: string,
  entries: readonly {
    id: string
    url: string
    name?: string
    username?: string
  }[],
): { ok: boolean; path: string; updated: string[] } {
  const path = join(webclawHome, 'web-systems.json')
  mkdirSync(webclawHome, { recursive: true })
  let systems: DiskSystem[] = []
  try {
    const raw = readFileSync(path, 'utf8')
    const db = JSON.parse(raw) as { systems?: DiskSystem[] }
    systems = Array.isArray(db.systems) ? db.systems : []
  } catch {
    systems = []
  }
  const updated: string[] = []
  const now = new Date().toISOString()
  for (const entry of entries) {
    const id = entry.id.trim().toLowerCase()
    const url = normalizeSystemUrl(entry.url)
    const name = entry.name?.trim() || id
    const username = entry.username?.trim()
    if (!id || !url) continue
    const existing = systems.find(s => s.id.toLowerCase() === id)
    if (existing) {
      existing.url = url
      existing.name = name
      existing.source = 'local'
      existing.updated_at = now
      if (username) existing.username = username
      // keep encrypted password unless setAccount updates it
    } else {
      systems.push({
        id,
        name,
        kind: 'internal',
        url,
        username: username || '',
        password: '',
        updated_at: now,
        source: 'local',
        remark: '',
        remote_status: null,
      })
    }
    updated.push(`${id}=${url}`)
  }
  writeFileSync(path, `${JSON.stringify({ systems }, null, 2)}\n`, 'utf8')
  return { ok: true, path, updated }
}

/** Upsert URL/name without touching password. */
export async function ensureAccount(
  env: LiteEnv,
  input: { id: string; url: string; name?: string },
): Promise<{ ok: boolean; item?: AccountStatus; error?: string }> {
  const url = normalizeSystemUrl(input.url)
  const home = env.webclawHome?.trim()
  // Direct file patch only — CLI `--url=` can lose `&` on some Windows hosts.
  if (home) {
    patchSystemsFile(home, [{ id: input.id, url, name: input.name }])
    return {
      ok: true,
      item: {
        id: input.id,
        name: input.name ?? input.id,
        url,
        username: '',
        has_password: false,
      },
    }
  }
  const { writeFileSync, unlinkSync, mkdirSync } = await import('node:fs')
  const { join } = await import('node:path')
  const { tmpdir } = await import('node:os')
  const { randomUUID } = await import('node:crypto')
  const dir = join(tmpdir(), 'webclaw-dsh')
  mkdirSync(dir, { recursive: true })
  const urlFile = join(dir, `ensure-url-${randomUUID()}.txt`)
  writeFileSync(urlFile, url, 'utf8')
  try {
    const argv = ['accounts', 'ensure', '--id', input.id, `--url-file=${urlFile}`]
    if (input.name?.trim()) argv.push('--name', input.name.trim())
    const result = await runLite(env, argv)
    const parsed = parseJsonObject(result.stdout)
    if (result.code === 0 && parsed) {
      return { ok: true, item: mapItem(parsed) }
    }
    return {
      ok: false,
      error: (result.stderr || result.stdout || 'accounts ensure failed').slice(0, 500),
    }
  } finally {
    try {
      unlinkSync(urlFile)
    } catch {
      /* ignore */
    }
  }
}

/**
 * Write credentials. Empty password means "keep existing" — caller must omit
 * set when password blank and only URL/user changed (use ensure + set with
 * password only when nonempty).
 */
export async function setAccount(
  env: LiteEnv,
  input: {
    id: string
    user: string
    password: string
    url?: string
    name?: string
  },
): Promise<{ ok: boolean; item?: AccountStatus; error?: string }> {
  const argv = [
    'accounts',
    'set',
    '--id',
    input.id,
    '--user',
    input.user,
    '--password',
    input.password,
  ]
  if (input.url?.trim()) argv.push(`--url=${normalizeSystemUrl(input.url)}`)
  if (input.name?.trim()) argv.push('--name', input.name.trim())
  const result = await runLite(env, argv)
  const parsed = parseJsonObject(result.stdout)
  if (result.code === 0 && parsed) {
    return { ok: true, item: mapItem(parsed) }
  }
  return {
    ok: false,
    error: (result.stderr || result.stdout || 'accounts set failed').slice(0, 500),
  }
}

/** Save one site from the UI form. */
export async function saveAccountFromForm(
  env: LiteEnv,
  input: {
    id: string
    name?: string
    url: string
    username: string
    password: string
  },
): Promise<{ ok: boolean; item?: AccountStatus; error?: string }> {
  const id = input.id.trim()
  const url = normalizeSystemUrl(input.url)
  if (!id || !url) {
    return { ok: false, error: 'id and url are required' }
  }
  const ensured = await ensureAccount(env, {
    id,
    url,
    name: input.name,
  })
  if (!ensured.ok) return ensured

  const user = input.username.trim()
  const password = input.password
  if (!user) {
    if (password) {
      return { ok: false, error: 'username required when setting a password' }
    }
    return { ok: true, item: ensured.item }
  }
  if (!password) {
    return { ok: true, item: ensured.item }
  }
  return setAccount(env, {
    id,
    user,
    password,
    url,
    name: input.name,
  })
}
