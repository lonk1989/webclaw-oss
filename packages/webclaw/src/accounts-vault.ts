/**
 * Local vault helpers via webclaw-lite accounts CLI (never for LLM tools that
 * accept passwords — Host UI / loopback server only).
 */

import { runLite, type LiteEnv } from './spawn.js'

export interface AccountStatus {
  id: string
  name: string
  url: string
  username: string
  has_password: boolean
}

/**
 * Soft hint when login fill fails for lack of vault credentials.
 * Do NOT use this to block opening the site — many sites work via SSO / existing session.
 * Never ask for passwords in chat.
 */
export const CREDENTIALS_SETUP_HINT =
  '站点不一定需要账号密码（可能已登录或 SSO）。应先 webclaw_run 打开页面。\n' +
  '仅当登录步明确要求填表且本地无凭据时：请用户打开 DSH「设置 → 网站账号」填写（只进本机 vault，勿把密码发给你）；\n' +
  '也可双击 %LOCALAPPDATA%\\webclaw\\bin\\webclaw-accounts.cmd。禁止 probe / 禁止在对话里要密码。'

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

function accountMatchesQuery(item: AccountStatus, query: string): boolean {
  const q = query.trim().toLowerCase().replace(/系统$/u, '')
  const id = item.id.toLowerCase()
  const name = item.name.toLowerCase().replace(/系统$/u, '')
  if (id === query.trim().toLowerCase() || name === q) return true
  if (q.includes('ehr') && id === 'ehr') return true
  if (q.includes('mes') && (id === 'old-mes' || id === 'mes')) return true
  if (q === 'oa' && id === 'oa') return true
  if (name.includes(q) || q.includes(name)) return true
  return false
}

/**
 * Login steps that lack username or password in the local vault.
 */
export function missingCredentialsForRecipe(
  content: string,
  items: readonly AccountStatus[],
): Array<{ query: string; id: string; name: string; url: string; reason: string }> {
  const missing: Array<{
    query: string
    id: string
    name: string
    url: string
    reason: string
  }> = []
  for (const query of extractLoginQueries(content)) {
    const hit = items.find(it => accountMatchesQuery(it, query))
    if (!hit) {
      missing.push({
        query,
        id: '',
        name: query,
        url: '',
        reason: 'no_vault_entry',
      })
      continue
    }
    if (!hit.username.trim() || !hit.has_password) {
      missing.push({
        query,
        id: hit.id,
        name: hit.name,
        url: hit.url,
        reason: !hit.username.trim() ? 'missing_username' : 'missing_password',
      })
    }
  }
  return missing
}

function parseJsonObject(text: string): Record<string, unknown> | undefined {
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start < 0 || end <= start) return undefined
  try {
    return JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>
  } catch {
    return undefined
  }
}

function mapItem(it: Record<string, unknown>): AccountStatus {
  return {
    id: String(it.id ?? ''),
    name: String(it.name ?? ''),
    url: String(it.url ?? ''),
    username: String(it.username ?? ''),
    has_password: Boolean(it.has_password ?? it.hasPassword ?? false),
  }
}

/** List vault entries (no password values). */
export async function listAccounts(env: LiteEnv): Promise<{
  ok: boolean
  items: AccountStatus[]
  error?: string
}> {
  const result = await runLite(env, ['accounts', 'list'])
  const text = `${result.stdout}\n${result.stderr}`.trim()
  const parsed = parseJsonObject(result.stdout) ?? parseJsonObject(text)
  if (parsed && parsed.type === 'accounts' && Array.isArray(parsed.items)) {
    return {
      ok: result.code === 0,
      items: (parsed.items as Record<string, unknown>[]).map(mapItem),
    }
  }
  return {
    ok: false,
    items: [],
    error: (result.stderr || result.stdout || 'accounts list failed').slice(0, 500),
  }
}

/** Upsert URL/name without touching password. */
export async function ensureAccount(
  env: LiteEnv,
  input: { id: string; url: string; name?: string },
): Promise<{ ok: boolean; item?: AccountStatus; error?: string }> {
  const argv = ['accounts', 'ensure', '--id', input.id, '--url', input.url]
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
  if (input.url?.trim()) argv.push('--url', input.url.trim())
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
  const url = input.url.trim()
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
    // URL/name updated; keep existing password. If never set, has_password stays false.
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
