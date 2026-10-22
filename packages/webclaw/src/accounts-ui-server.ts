/**
 * Loopback-only HTTP UI for site accounts.
 *
 * Bound to 127.0.0.1 — passwords never leave the machine or enter the LLM.
 * DSH Client (when isLoopback) and the desktop browser launcher both use this.
 */

import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import type { LiteEnv } from './spawn.js'
import { listAccounts, saveAccountFromForm } from './accounts-vault.js'

/** Default port (near CDP 19222); falls back to ephemeral if busy. */
export const ACCOUNTS_UI_DEFAULT_PORT = 19229

export interface AccountsUiHandle {
  readonly port: number
  readonly url: string
  close: () => void
}

function isLoopbackOrigin(origin: string | undefined): boolean {
  if (!origin) return true
  try {
    const u = new URL(origin)
    return u.hostname === '127.0.0.1' || u.hostname === 'localhost'
  } catch {
    return false
  }
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const raw = JSON.stringify(body)
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
  })
  res.end(raw)
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    let size = 0
    req.on('data', (c: Buffer) => {
      size += c.length
      if (size > 64_000) {
        reject(new Error('body too large'))
        req.destroy()
        return
      }
      chunks.push(c)
    })
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    req.on('error', reject)
  })
}

function pageHtml(port: number): string {
  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>webclaw 网站账号</title>
<style>
  :root { color-scheme: light dark; font-family: "Segoe UI", system-ui, sans-serif; }
  body { margin: 0; padding: 24px; max-width: 720px; }
  h1 { font-size: 1.25rem; margin: 0 0 8px; }
  .hint { opacity: .75; font-size: 13px; margin-bottom: 20px; line-height: 1.5; }
  .card { border: 1px solid color-mix(in srgb, currentColor 18%, transparent);
    border-radius: 10px; padding: 14px 16px; margin-bottom: 14px; }
  .card h2 { font-size: 1rem; margin: 0 0 10px; }
  label { display: block; font-size: 12px; opacity: .7; margin: 8px 0 4px; }
  input { width: 100%; box-sizing: border-box; padding: 8px 10px; border-radius: 6px;
    border: 1px solid color-mix(in srgb, currentColor 22%, transparent); background: transparent; color: inherit; }
  .row { display: flex; gap: 10px; align-items: center; margin-top: 12px; flex-wrap: wrap; }
  button { padding: 8px 14px; border-radius: 6px; border: 0; cursor: pointer;
    background: #2563eb; color: #fff; font-weight: 600; }
  button:disabled { opacity: .5; cursor: default; }
  .badge { font-size: 12px; padding: 2px 8px; border-radius: 999px;
    background: color-mix(in srgb, currentColor 10%, transparent); }
  .badge.ok { background: color-mix(in srgb, #16a34a 25%, transparent); }
  .msg { font-size: 13px; margin-top: 8px; min-height: 1.2em; }
  .err { color: #dc2626; }
  .okmsg { color: #16a34a; }
</style>
</head>
<body>
  <h1>网站账号（本机保险箱）</h1>
  <p class="hint">
    仅监听 127.0.0.1:${port}。密码写入本机 webclaw vault（Windows DPAPI），
    <strong>不会</strong>进入 DSH 对话或模型。留空密码表示保持原密码不变。
  </p>
  <div id="list">加载中…</div>
<script>
async function load() {
  const res = await fetch('/api/accounts');
  const data = await res.json();
  const root = document.getElementById('list');
  if (!data.ok) { root.textContent = data.error || '加载失败'; return; }
  root.innerHTML = '';
  for (const item of data.items || []) {
    const card = document.createElement('div');
    card.className = 'card';
    card.innerHTML = \`
      <h2>\${escapeHtml(item.name || item.id)}
        <span class="badge \${item.has_password ? 'ok' : ''}">\${item.has_password ? '已设密码' : '未设密码'}</span>
      </h2>
      <label>登录地址</label>
      <input data-f="url" value="\${escapeAttr(item.url || '')}"/>
      <label>账号</label>
      <input data-f="username" value="\${escapeAttr(item.username || '')}" autocomplete="username"/>
      <label>密码（留空=不修改）</label>
      <input data-f="password" type="password" value="" autocomplete="new-password"/>
      <div class="row">
        <button type="button" data-save>保存</button>
        <span class="msg" data-msg></span>
      </div>\`;
    card.querySelector('[data-save]').onclick = async () => {
      const msg = card.querySelector('[data-msg]');
      const btn = card.querySelector('[data-save]');
      btn.disabled = true; msg.textContent = '保存中…'; msg.className = 'msg';
      const body = {
        id: item.id,
        name: item.name,
        url: card.querySelector('[data-f=url]').value,
        username: card.querySelector('[data-f=username]').value,
        password: card.querySelector('[data-f=password]').value,
      };
      try {
        const r = await fetch('/api/accounts/' + encodeURIComponent(item.id), {
          method: 'PUT', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        const j = await r.json();
        if (j.ok) {
          msg.textContent = '已保存到本机';
          msg.className = 'msg okmsg';
          card.querySelector('[data-f=password]').value = '';
          const badge = card.querySelector('.badge');
          if (j.item?.has_password) { badge.textContent = '已设密码'; badge.classList.add('ok'); }
        } else {
          msg.textContent = j.error || '保存失败';
          msg.className = 'msg err';
        }
      } catch (e) {
        msg.textContent = String(e?.message || e);
        msg.className = 'msg err';
      } finally { btn.disabled = false; }
    };
    root.appendChild(card);
  }
}
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
}
function escapeAttr(s) { return escapeHtml(s).replace(/\`/g, '&#96;'); }
load();
</script>
</body>
</html>`
}

function writeLaunchers(opts: {
  home: string
  binaryPath: string
  url: string
}): void {
  const binDir = join(opts.home, 'bin')
  mkdirSync(binDir, { recursive: true })
  const lite = opts.binaryPath.replace(/\//g, '\\')
  // Absolute-path wrappers so users need not put webclaw-lite on PATH.
  const liteCmd = `@echo off\r\n"${lite}" %*\r\n`
  writeFileSync(join(binDir, 'webclaw-lite.cmd'), liteCmd, 'utf8')
  const accountsCmd = `@echo off\r\necho Opening webclaw accounts UI...\r\nstart "" "${opts.url}"\r\n`
  writeFileSync(join(binDir, 'webclaw-accounts.cmd'), accountsCmd, 'utf8')
  writeFileSync(
    join(opts.home, 'accounts-ui.json'),
    JSON.stringify({ url: opts.url, port: Number(new URL(opts.url).port), bind: '127.0.0.1' }, null, 2),
    'utf8',
  )
}

/**
 * Start loopback accounts UI. Returns null if disabled / failed.
 */
export async function startAccountsUiServer(options: {
  enabled: boolean
  preferredPort: number
  getEnv: () => Promise<LiteEnv>
  home: string
  binaryPath: string
  log: {
    info: (msg: string, ...args: unknown[]) => void
    warn: (msg: string, ...args: unknown[]) => void
  }
}): Promise<AccountsUiHandle | null> {
  if (!options.enabled) return null

  const envFor = options.getEnv

  const handler = async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    const origin = req.headers.origin
    if (origin && !isLoopbackOrigin(origin)) {
      sendJson(res, 403, { ok: false, error: 'forbidden origin' })
      return
    }
    if (origin && isLoopbackOrigin(origin)) {
      res.setHeader('Access-Control-Allow-Origin', origin)
      res.setHeader('Access-Control-Allow-Methods', 'GET,PUT,OPTIONS')
      res.setHeader('Access-Control-Allow-Headers', 'Content-Type')
      res.setHeader('Vary', 'Origin')
    }
    if (req.method === 'OPTIONS') {
      res.writeHead(204)
      res.end()
      return
    }

    const host = req.headers.host || `127.0.0.1`
    const url = new URL(req.url || '/', `http://${host}`)

    try {
      if (req.method === 'GET' && url.pathname === '/api/health') {
        sendJson(res, 200, { ok: true, service: 'webclaw-accounts-ui' })
        return
      }
      if (req.method === 'GET' && (url.pathname === '/' || url.pathname === '/index.html')) {
        const html = pageHtml(Number(url.port) || options.preferredPort)
        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' })
        res.end(html)
        return
      }
      if (req.method === 'GET' && url.pathname === '/api/accounts') {
        const listed = await listAccounts(await envFor())
        sendJson(res, listed.ok ? 200 : 500, listed)
        return
      }
      const putMatch = /^\/api\/accounts\/([^/]+)$/.exec(url.pathname)
      if (req.method === 'PUT' && putMatch) {
        const id = decodeURIComponent(putMatch[1]!)
        const raw = await readBody(req)
        let body: Record<string, unknown>
        try {
          body = JSON.parse(raw) as Record<string, unknown>
        } catch {
          sendJson(res, 400, { ok: false, error: 'invalid JSON' })
          return
        }
        const saved = await saveAccountFromForm(await envFor(), {
          id,
          name: typeof body.name === 'string' ? body.name : undefined,
          url: String(body.url ?? ''),
          username: String(body.username ?? ''),
          password: String(body.password ?? ''),
        })
        sendJson(res, saved.ok ? 200 : 400, saved)
        return
      }
      sendJson(res, 404, { ok: false, error: 'not found' })
    } catch (error) {
      sendJson(res, 500, {
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      })
    }
  }

  const listen = (port: number): Promise<AccountsUiHandle> =>
    new Promise((resolve, reject) => {
      const server: Server = createServer((req, res) => {
        void handler(req, res)
      })
      server.once('error', reject)
      server.listen(port, '127.0.0.1', () => {
        const addr = server.address()
        const p =
          typeof addr === 'object' && addr && 'port' in addr ? addr.port : port
        const url = `http://127.0.0.1:${p}/`
        try {
          writeLaunchers({
            home: options.home,
            binaryPath: options.binaryPath,
            url,
          })
        } catch (error) {
          options.log.warn(
            'webclaw: write accounts launchers failed: %s',
            error instanceof Error ? error.message : error,
          )
        }
        options.log.info('webclaw: accounts UI on %s (loopback only)', url)
        resolve({
          port: p,
          url,
          close: () => {
            server.close()
          },
        })
      })
    })

  try {
    return await listen(options.preferredPort)
  } catch {
    try {
      return await listen(0)
    } catch (error) {
      options.log.warn(
        'webclaw: accounts UI failed to start: %s',
        error instanceof Error ? error.message : error,
      )
      return null
    }
  }
}

/** Probe loopback health (Client). */
export async function probeAccountsUi(
  ports: number[] = [ACCOUNTS_UI_DEFAULT_PORT],
): Promise<string | null> {
  for (const port of ports) {
    try {
      const ctrl = new AbortController()
      const t = setTimeout(() => ctrl.abort(), 400)
      const res = await fetch(`http://127.0.0.1:${port}/api/health`, {
        signal: ctrl.signal,
      })
      clearTimeout(t)
      if (!res.ok) continue
      const j = (await res.json()) as { ok?: boolean; service?: string }
      if (j.ok && j.service === 'webclaw-accounts-ui') {
        return `http://127.0.0.1:${port}/`
      }
    } catch {
      /* try next */
    }
  }
  return null
}
