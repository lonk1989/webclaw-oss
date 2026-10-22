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
