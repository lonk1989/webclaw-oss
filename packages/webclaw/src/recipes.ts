/**
 * Local `.webclaw` recipe library helpers for the DSH plugin.
 */

import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { basename, extname, isAbsolute, join, resolve } from 'node:path'
import { homedir } from 'node:os'

export interface RecipesConfig {
  /** Absolute override; wins over cwd-relative. */
  readonly recipesDir?: string
  /** Relative under agent cwd (default `recipes`). */
  readonly recipesRel?: string
  /** WEBCLAW_HOME override from plugin config. */
  readonly webclawHome?: string
}

function defaultWebclawHome(): string {
  if (process.platform === 'win32') {
    const local = process.env.LOCALAPPDATA?.trim()
    if (local) return join(local, 'webclaw')
  }
  return join(homedir(), '.webclaw')
}

export function webclawHome(config: RecipesConfig): string {
  const fromEnv = process.env.WEBCLAW_HOME?.trim()
  if (fromEnv) return fromEnv
  const fromCfg = config.webclawHome?.trim()
  if (fromCfg) return fromCfg
  return defaultWebclawHome()
}

/** Sanitize stem for `foo.webclaw`. */
export function safeRecipeName(raw: string): string {
  const stem = raw
    .trim()
    .replace(/\.webclaw$/i, '')
    .replace(/[^\w.\u4e00-\u9fff-]+/g, '-')
    .replace(/^[-_.]+|[-_.]+$/g, '')
    .slice(0, 80)
  if (!stem) throw new Error('invalid recipe name')
  return stem
}

/**
 * Resolve the authoritative recipes directory.
 * Priority: absolute recipesDir → cwd/recipesRel → WEBCLAW_HOME/recipes.
 */
export function resolveRecipesDir(
  config: RecipesConfig,
  cwd?: string | null,
): string {
  const abs = config.recipesDir?.trim()
  if (abs) {
    mkdirSync(abs, { recursive: true })
    return abs
  }
  const rel = (config.recipesRel?.trim() || 'recipes').replace(/^[\\/]+/, '')
  const root = cwd?.trim()
  if (root) {
    const dir = join(root, rel)
    mkdirSync(dir, { recursive: true })
    return dir
  }
  const dir = join(webclawHome(config), 'recipes')
  mkdirSync(dir, { recursive: true })
  return dir
}

/** Extract optional `# title` from the first non-empty line. */
export function titleFromBody(body: string): string | undefined {
  const text = body.replace(/^\uFEFF/, '')
  for (const line of text.split(/\r?\n/)) {
    const t = line.trim()
    if (!t) continue
    if (t.startsWith('#')) {
      const title = t.replace(/^#+\s*/, '').trim()
      return title || undefined
    }
    break
  }
  return undefined
}

export function freezeRecipe(options: {
  readonly config: RecipesConfig
  readonly name: string
  readonly recipe: string
  readonly cwd?: string | null
  readonly title?: string
}): { path: string; name: string; bytes: number } {
  const stem = safeRecipeName(options.name)
  let body = options.recipe.replace(/^\uFEFF/, '').trim()
  if (!body) throw new Error('recipe must be non-empty')
  const title = options.title?.trim()
  if (title && !body.startsWith('#')) {
    body = `# ${title}\n${body}`
  }
  if (!body.endsWith('\n')) body += '\n'
  const dir = resolveRecipesDir(options.config, options.cwd)
  const path = join(dir, `${stem}.webclaw`)
  writeFileSync(path, body, { encoding: 'utf8' })
  return { path, name: stem, bytes: Buffer.byteLength(body, 'utf8') }
}

export function listRecipes(config: RecipesConfig, cwd?: string | null): {
  dir: string
  items: Array<{
    name: string
    path: string
    bytes: number
    mtimeMs: number
    title?: string
  }>
} {
  const dir = resolveRecipesDir(config, cwd)
  const items: Array<{
    name: string
    path: string
    bytes: number
    mtimeMs: number
    title?: string
  }> = []
  if (!existsSync(dir)) return { dir, items }
  for (const ent of readdirSync(dir, { withFileTypes: true })) {
    if (!ent.isFile()) continue
    if (!ent.name.toLowerCase().endsWith('.webclaw')) continue
    const path = join(dir, ent.name)
    const st = statSync(path)
    const body = readFileSync(path, 'utf8')
    items.push({
      name: basename(ent.name, '.webclaw'),
      path,
      bytes: st.size,
      mtimeMs: st.mtimeMs,
      title: titleFromBody(body),
    })
  }
  items.sort((a, b) => a.name.localeCompare(b.name))
  return { dir, items }
}

export function importRecipe(options: {
  readonly config: RecipesConfig
  readonly sourcePath: string
  readonly name?: string
  readonly cwd?: string | null
}): { path: string; name: string; bytes: number } {
  const src = resolve(options.sourcePath.trim())
  if (!existsSync(src)) throw new Error(`source not found: ${src}`)
  const stem = safeRecipeName(
    options.name?.trim() || basename(src, extname(src)),
  )
  const dir = resolveRecipesDir(options.config, options.cwd)
  const dest = join(dir, `${stem}.webclaw`)
  copyFileSync(src, dest)
  const st = statSync(dest)
  return { path: dest, name: stem, bytes: st.size }
}

/**
 * Resolve a recipe path for run: absolute/existing, else under recipes dir / cwd.
 */
export function resolveRecipePath(
  config: RecipesConfig,
  recipePath: string,
  cwd?: string | null,
): string {
  const raw = recipePath.trim()
  if (!raw) throw new Error('recipePath must be non-empty')
  if (isAbsolute(raw) && existsSync(raw)) return raw
  if (existsSync(raw)) return resolve(raw)
  if (cwd?.trim()) {
    const underCwd = resolve(cwd.trim(), raw)
    if (existsSync(underCwd)) return underCwd
  }
  const dir = resolveRecipesDir(config, cwd)
  const base = basename(raw)
  const withExt = base.toLowerCase().endsWith('.webclaw')
    ? base
    : `${safeRecipeName(base)}.webclaw`
  const cand = join(dir, withExt)
  if (existsSync(cand)) return cand
  if (isAbsolute(raw)) return raw
  return resolve(cwd?.trim() || process.cwd(), raw)
}

/** Best-effort agent session cwd from ToolRunContext. */
export function agentCwd(exec: { agent?: unknown }): string | undefined {
  const agent = exec.agent as
    | { session?: { header?: { cwd?: string } } }
    | undefined
  const cwd = agent?.session?.header?.cwd?.trim()
  return cwd || undefined
}
