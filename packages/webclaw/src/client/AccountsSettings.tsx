/**
 * DSH Settings → 网站账号: edit local vault via loopback Host UI API.
 * Passwords never go through the LLM — only 127.0.0.1 Host process.
 */
import { useEffect, useState, type ReactNode } from 'react'
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots'

export type AccountsSettingsProps = PropsLocale<'webclawAccounts'>

interface AccountRow {
  id: string
  name: string
  url: string
  username: string
  has_password: boolean
  passwordDraft: string
  msg: string
  err: boolean
  saving: boolean
}

const CANDIDATE_PORTS = [19229, 19230, 19231, 19232, 19233]

async function findBase(): Promise<string | null> {
  for (const port of CANDIDATE_PORTS) {
    try {
      const ctrl = new AbortController()
      const timer = setTimeout(() => ctrl.abort(), 500)
      const res = await fetch(`http://127.0.0.1:${port}/api/health`, {
        signal: ctrl.signal,
      })
      clearTimeout(timer)
      if (!res.ok) continue
      const j = (await res.json()) as { ok?: boolean; service?: string }
      if (j.ok && j.service === 'webclaw-accounts-ui') {
        return `http://127.0.0.1:${port}`
      }
    } catch {
      /* next */
    }
  }
  // Ephemeral port: Host writes accounts-ui.json — Client cannot read it.
  // User can open the standalone page via webclaw-accounts.cmd.
  return null
}

/**
 * Settings section body for site accounts.
 */
export function AccountsSettings({ t }: AccountsSettingsProps): ReactNode {
  const [base, setBase] = useState<string | null | undefined>(undefined)
  const [rows, setRows] = useState<AccountRow[]>([])
  const [loadError, setLoadError] = useState('')

  const reload = async (apiBase: string) => {
    setLoadError('')
    try {
      const res = await fetch(`${apiBase}/api/accounts`)
      const data = (await res.json()) as {
        ok?: boolean
        items?: Array<{
          id: string
          name: string
          url: string
          username: string
          has_password: boolean
        }>
        error?: string
      }
      if (!data.ok) {
        setLoadError(data.error || t('loadFailed'))
        return
      }
      setRows(
        (data.items ?? []).map(it => ({
          ...it,
          passwordDraft: '',
          msg: '',
          err: false,
          saving: false,
        })),
      )
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : t('loadFailed'))
    }
  }

  useEffect(() => {
    let cancelled = false
    void (async () => {
      const found = await findBase()
      if (cancelled) return
      setBase(found)
      if (found) await reload(found)
    })()
    return () => {
      cancelled = true
    }
  }, [])

  const updateRow = (id: string, patch: Partial<AccountRow>) => {
    setRows(prev => prev.map(r => (r.id === id ? { ...r, ...patch } : r)))
  }

  const saveRow = async (row: AccountRow) => {
    if (!base) return
    updateRow(row.id, { saving: true, msg: t('saving'), err: false })
    try {
      const res = await fetch(`${base}/api/accounts/${encodeURIComponent(row.id)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: row.id,
          name: row.name,
          url: row.url,
          username: row.username,
          password: row.passwordDraft,
        }),
      })
      const data = (await res.json()) as {
        ok?: boolean
        error?: string
        item?: { has_password?: boolean; username?: string; url?: string }
      }
      if (data.ok) {
        updateRow(row.id, {
          saving: false,
          msg: t('saved'),
          err: false,
          passwordDraft: '',
          has_password: Boolean(data.item?.has_password ?? row.has_password),
          username: data.item?.username ?? row.username,
          url: data.item?.url ?? row.url,
        })
      } else {
        updateRow(row.id, {
          saving: false,
          msg: data.error || t('saveFailed'),
          err: true,
        })
      }
    } catch (error) {
      updateRow(row.id, {
        saving: false,
        msg: error instanceof Error ? error.message : t('saveFailed'),
        err: true,
      })
    }
  }

  if (base === undefined) {
    return <div style={{ padding: 16 }}>{t('loading')}</div>
  }

  if (base === null) {
    return (
      <div style={{ padding: 16, maxWidth: 560, lineHeight: 1.55, fontSize: 14 }}>
        <p style={{ marginTop: 0 }}>{t('offlineTitle')}</p>
        <p style={{ opacity: 0.8 }}>{t('offlineHint')}</p>
        <pre
          style={{
            padding: 12,
            overflow: 'auto',
            fontSize: 12,
            background: 'color-mix(in srgb, currentColor 6%, transparent)',
            borderRadius: 8,
          }}
        >
          {t('offlineCmd')}
        </pre>
      </div>
    )
  }

  return (
    <div style={{ padding: 16, maxWidth: 640 }}>
      <p style={{ marginTop: 0, opacity: 0.8, fontSize: 13, lineHeight: 1.5 }}>
        {t('intro')}
      </p>
      {loadError ? (
        <p style={{ color: '#dc2626' }}>{loadError}</p>
      ) : (
        rows.map(row => (
          <div
            key={row.id}
            style={{
              border: '1px solid color-mix(in srgb, currentColor 14%, transparent)',
              borderRadius: 10,
              padding: '12px 14px',
              marginBottom: 12,
            }}
          >
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 8 }}>
              <strong>{row.name || row.id}</strong>
              <span style={{ fontSize: 12, opacity: 0.7 }}>
                {row.has_password ? t('hasPassword') : t('noPassword')}
              </span>
            </div>
            <label style={{ display: 'block', fontSize: 12, opacity: 0.65, marginBottom: 4 }}>
              {t('url')}
            </label>
            <input
              value={row.url}
              onChange={e => updateRow(row.id, { url: e.target.value })}
              style={{ width: '100%', boxSizing: 'border-box', marginBottom: 8, padding: '6px 8px' }}
            />
            <label style={{ display: 'block', fontSize: 12, opacity: 0.65, marginBottom: 4 }}>
              {t('username')}
            </label>
            <input
              value={row.username}
              autoComplete="username"
              onChange={e => updateRow(row.id, { username: e.target.value })}
              style={{ width: '100%', boxSizing: 'border-box', marginBottom: 8, padding: '6px 8px' }}
            />
            <label style={{ display: 'block', fontSize: 12, opacity: 0.65, marginBottom: 4 }}>
              {t('password')}
            </label>
            <input
              type="password"
              value={row.passwordDraft}
              autoComplete="new-password"
              placeholder={t('passwordPlaceholder')}
              onChange={e => updateRow(row.id, { passwordDraft: e.target.value })}
              style={{ width: '100%', boxSizing: 'border-box', marginBottom: 8, padding: '6px 8px' }}
            />
            <div style={{ display: 'flex', gap: 10, alignItems: 'center' }}>
              <button
                type="button"
                disabled={row.saving}
                onClick={() => void saveRow(row)}
                style={{
                  padding: '6px 12px',
                  borderRadius: 6,
                  border: 0,
                  background: '#2563eb',
                  color: '#fff',
                  fontWeight: 600,
                  cursor: row.saving ? 'default' : 'pointer',
                }}
              >
                {t('save')}
              </button>
              <span style={{ fontSize: 12, color: row.err ? '#dc2626' : undefined }}>
                {row.msg}
              </span>
            </div>
          </div>
        ))
      )}
      <p style={{ fontSize: 12, opacity: 0.65 }}>
        <a href={base + '/'} target="_blank" rel="noreferrer">
          {t('openStandalone')}
        </a>
      </p>
    </div>
  )
}
