/**
 * Rescue http(s) URLs mangled by Chinese verbs glued onto the scheme, and
 * rewrite recipes so lite's「打开」step does not treat `浏览器访问 http://…`
 * as a hostname (→ `xn--%20http-…`).
 */

const SCHEME = /https?:\/\//gi
const TRAILING_JUNK = /[\s<>"'，。；、）】》」']+$/u

function isSchemeEatenPunyHost(hostname: string): boolean {
  return /^xn--/i.test(hostname) && /https?/i.test(hostname)
}

/** Pull the best absolute http(s) URL out of a noisy string. */
export function coerceHttpUrl(raw: string): string {
  let s = (raw || '').trim()
  if (s === '') return s
  while (/\\+u0026/.test(s)) {
    s = s.replace(/\\+u0026/g, '&')
  }

  const punyPath = s.match(
    /^https?:\/\/xn--[^/\s]*https?[^/\s]*(\/\/[\w.:[\]%-][^\s]*)$/i,
  )
  if (punyPath?.[1]) {
    const preferHttps = /xn--[^/\s]*https/i.test(s)
    s = `${preferHttps ? 'https:' : 'http:'}${punyPath[1]}`
  } else {
    try {
      const u = new URL(s)
      if (isSchemeEatenPunyHost(u.hostname) && u.pathname.startsWith('//')) {
        const preferHttps = /https/i.test(u.hostname)
        s = `${preferHttps ? 'https:' : 'http:'}${u.pathname}${u.search}${u.hash}`
      }
    } catch {
      // keep scanning
    }
  }

  let best: string | undefined
  SCHEME.lastIndex = 0
  let match: RegExpExecArray | null
  while ((match = SCHEME.exec(s)) !== null) {
    const slice = s.slice(match.index).replace(TRAILING_JUNK, '')
    const cut = slice.search(/[\s<>"'\u4e00-\u9fff]/)
    const candidate = (cut === -1 ? slice : slice.slice(0, cut)).replace(
      /[)\]】》」']+$/g,
      '',
    )
    try {
      const u = new URL(candidate)
      if (
        (u.protocol === 'http:' || u.protocol === 'https:')
        && u.hostname.includes('.')
        && !isSchemeEatenPunyHost(u.hostname)
      ) {
        best = u.href
      }
    } catch {
      // next
    }
  }
  return best ?? s
}

const OPEN_PREFIX =
  /(?:请)?(?:打开浏览器访问|打开浏览器|浏览器访问|打开网址|访问网址|打开页面|打开链接|前往|访问|打开)\s*/giu

/**
 * Rewrite a recipe so each URL is a clean `打开 <url>` fragment before lite parses it.
 */
export function sanitizeWebclawRecipe(recipe: string): string {
  let s = (recipe || '').replace(/^\uFEFF/, '')
  if (s.trim() === '') return s

  s = s.replace(/https?:\/\/xn--[^\s，。；、<>"']+/gi, (m) => coerceHttpUrl(m))
  s = s.replace(
    new RegExp(`${OPEN_PREFIX.source}(https?:\\/\\/)`, 'giu'),
    '打开 $1',
  )
  s = s.replace(/打开\s+打开\s+/gu, '打开 ')
  return s
}
