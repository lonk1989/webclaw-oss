/**
 * Recipe「参数」helpers for DSH (mirrors runtime extract/merge for static params).
 *
 * Desktop shows these in the top 参数栏 before run. DSH has no native form API,
 * so the Host tools extract defaults, gate run until confirmed, then rewrite
 * `参数名=值` into the temp recipe (file declarations otherwise beat CLI --params).
 */

export interface RecipeParamKv {
  readonly name: string
  readonly value: string
}

/** Normalize BOM / newlines so comma-split sees one step stream. */
function normalizeRecipeText(content: string): string {
  return content.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n').trim()
}

/**
 * Split recipe into step segments on Chinese/ASCII commas.
 * Escape sequences `\,` `\，` keep the comma literal (same idea as runtime).
 *
 * Newlines become `，` (not empty): otherwise
 * `参数原因=家中有事\n登录EHR系统` collapses to `参数原因=家中有事登录EHR系统`
 * and the login step is lost.
 */
export function splitRecipeSteps(content: string): string[] {
  const text = normalizeRecipeText(content).replace(/\n+/g, '，')
  if (!text) return []
  const steps: string[] = []
  let buf = ''
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!
    if (ch === '\\' && i + 1 < text.length) {
      const next = text[i + 1]!
      if (next === ',' || next === '，' || next === '.' || next === '。') {
        buf += next
        i++
        continue
      }
    }
    if (ch === '，' || ch === ',') {
      const t = buf.trim()
      if (t) steps.push(t)
      buf = ''
      continue
    }
    buf += ch
  }
  const last = buf.trim()
  if (last) steps.push(last)
  return steps
}

/**
 * Extract static `参数名=值` declarations (top bar / form fields).
 * Skips「动态参数」; last duplicate name wins order of first occurrence with last value.
 */
export function extractRecipeParams(content: string): RecipeParamKv[] {
  const steps = splitRecipeSteps(content)
  const order: string[] = []
  const map = new Map<string, string>()
  for (const step of steps) {
    if (step.startsWith('动态参数')) continue
    if (!step.startsWith('参数')) continue
    const body = step.slice('参数'.length)
    const eq = body.indexOf('=')
    if (eq <= 0) continue
    const name = body.slice(0, eq).trim()
    // Drop glued verbs from older newline bugs: 家中有事登录EHR系统 → 家中有事
    let value = body.slice(eq + 1).trim()
    value = value.replace(
      /(?<=[\u4e00-\u9fffA-Za-z0-9])(登录|打开|点击|输入|等待)[\u4e00-\u9fffA-Za-z0-9#{}={{}}\s_-]*$/u,
      '',
    ).trim()
    if (!name || name.includes('.')) continue
    if (!map.has(name)) order.push(name)
    map.set(name, value)
  }
  return order.map(name => ({ name, value: map.get(name) ?? '' }))
}

/** Drop static「参数」steps; keep「动态参数」and everything else. */
export function stripRecipeParamSteps(content: string): {
  body: string
  params: RecipeParamKv[]
} {
  const steps = splitRecipeSteps(content)
  const params = extractRecipeParams(content)
  // Static「参数」stripped;「动态参数」and all other steps kept.
  const kept = steps.filter(step => {
    if (step.startsWith('动态参数')) return true
    if (step.startsWith('参数')) return false
    return true
  })
  return { body: kept.join('，'), params }
}

/** Rewrite content: prefix merged `参数名=值` then body without old static params. */
export function mergeParamsIntoContent(
  content: string,
  entries: readonly RecipeParamKv[],
): string {
  const { body } = stripRecipeParamSteps(content)
  const cleaned = entries
    .map(e => ({ name: e.name.trim(), value: e.value }))
    .filter(e => e.name !== '')
  if (cleaned.length === 0) return body
  const prefix = cleaned.map(e => `参数${e.name}=${e.value}`).join('，')
  if (!body.trim()) return prefix
  return `${prefix}，${body}`
}

/**
 * If a bad split once glued `登录…` into a 参数 value and then stripped it,
 * recover login (and following) steps from the raw recipe.
 */
export function recoverLostActionSteps(
  rawContent: string,
  mergedContent: string,
): string {
  const rawSteps = splitRecipeSteps(rawContent)
  const mergedSteps = splitRecipeSteps(mergedContent)
  const rawActions = rawSteps.filter(
    s => !s.startsWith('参数') || s.startsWith('动态参数'),
  )
  const mergedActions = mergedSteps.filter(
    s => !s.startsWith('参数') || s.startsWith('动态参数'),
  )
  const rawHasLogin = rawActions.some(s => s.startsWith('登录'))
  const mergedHasLogin = mergedActions.some(s => s.startsWith('登录'))
  if (!rawHasLogin || mergedHasLogin) return mergedContent
  const params = extractRecipeParams(mergedContent)
  const prefix =
    params.length > 0
      ? params.map(p => `参数${p.name}=${p.value}`).join('，') + '，'
      : ''
  return `${prefix}${rawActions.join('，')}`
}

/**
 * Apply overrides onto declared params (declared order preserved; new keys appended).
 */
export function applyParamOverrides(
  content: string,
  overrides?: Record<string, string> | null,
): { content: string; params: RecipeParamKv[] } {
  const base = extractRecipeParams(content)
  const map = new Map(base.map(p => [p.name, p.value]))
  if (overrides) {
    for (const [rawKey, rawVal] of Object.entries(overrides)) {
      const name = rawKey.trim()
      if (!name) continue
      map.set(name, String(rawVal ?? ''))
    }
  }
  const params: RecipeParamKv[] = []
  const seen = new Set<string>()
  for (const p of base) {
    params.push({ name: p.name, value: map.get(p.name) ?? p.value })
    seen.add(p.name)
  }
  if (overrides) {
    for (const rawKey of Object.keys(overrides)) {
      const name = rawKey.trim()
      if (!name || seen.has(name)) continue
      params.push({ name, value: map.get(name) ?? '' })
      seen.add(name)
    }
  }
  const merged = mergeParamsIntoContent(content, params)
  return {
    content: recoverLostActionSteps(content, merged),
    params,
  }
}

/** Parse tool `params` arg: JSON object string or already-plain record. */
export function parseParamsArg(raw: unknown): Record<string, string> | undefined {
  if (raw == null || raw === '') return undefined
  if (typeof raw === 'object' && !Array.isArray(raw)) {
    const out: Record<string, string> = {}
    for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
      if (typeof k === 'string' && k.trim()) out[k.trim()] = String(v ?? '')
    }
    return Object.keys(out).length ? out : undefined
  }
  if (typeof raw !== 'string') return undefined
  const text = raw.trim()
  if (!text) return undefined
  try {
    const parsed = JSON.parse(text) as unknown
    return parseParamsArg(parsed)
  } catch {
    throw new Error(`params must be a JSON object string, got: ${text.slice(0, 120)}`)
  }
}
