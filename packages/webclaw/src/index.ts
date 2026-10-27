/**
 * @lonk1989/webclaw — recipe-level browser RPA for DeepSeek Harness.
 *
 * Registers task tools that spawn webclaw-lite. Does not claim ctx.browserUse.
 * On Windows, ensureOnBoot (default true) downloads lite from GitHub Releases.
 * @module
 */

import { readFileSync } from 'node:fs'
import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from '@deepseek-ai/dsh-tools'
import z from '@deepseek-ai/schemastery'
import {
  ensureLite,
  resolveBinaryPath,
  resolveLiteBaseUrl,
  resolveWebclawHome,
  type EnsureLiteResult,
} from './ensure-lite.js'
import { parseDoneEvent, runLite, writeTempRecipe, type LiteEnv } from './spawn.js'
import {
  agentCwd,
  freezeRecipe,
  importRecipe,
  listRecipes,
  resolveRecipePath,
  type RecipesConfig,
} from './recipes.js'
import {
  applyParamOverrides,
  extractRecipeParams,
  parseParamsArg,
  type RecipeParamKv,
} from './params.js'
import { redactSecrets } from './redact.js'
import {
  ACCOUNTS_UI_DEFAULT_PORT,
  startAccountsUiServer,
  type AccountsUiHandle,
} from './accounts-ui-server.js'
import {
  CREDENTIALS_SETUP_HINT,
  ensureAccount,
  isSsoAccountUrl,
  listAccounts,
  missingCredentialsForRecipe,
  normalizeSystemUrl,
  patchSystemsFile,
  setAccount,
} from './accounts-vault.js'
import { registerBundledSkill } from './skill.js'

/** Cordis plugin id. */
export const name = 'webclaw'

/** Tool registry must exist; skills are optional (deferred via ctx.inject). */
export const inject = ['tools']

/** Deploy-time options for locating and invoking webclaw-lite. */
export interface Config {
  /**
   * Absolute path to webclaw-lite, or empty / `webclaw-lite` to use the managed
   * `%LOCALAPPDATA%/webclaw/bin/webclaw-lite.exe` (auto-downloaded on Windows).
   */
  binaryPath: string
  /** Maps to WEBCLAW_HOME. */
  webclawHome: string
  /**
   * Release directory prefix for download/update (no filename).
   * Empty → GitHub `…/releases/download/v{pluginVersion}`.
   */
  liteBaseUrl: string
  /** When true, ensure lite at boot (download if missing). Default true. */
  ensureOnBoot: boolean
  /** Register packaged SKILL.md. Default true. */
  registerSkill: boolean
  /** Max ms for a single `webclaw_run` (default 10 minutes). */
  runTimeoutMs: number
  /** Max ms for `webclaw_probe` (default 2 minutes). */
  probeTimeoutMs: number
  /**
   * Absolute recipes library directory. When set, wins over workspace-relative.
   * Empty: use `{agent.cwd}/{recipesRel}` or `WEBCLAW_HOME/recipes`.
   */
  recipesDir: string
  /** Relative recipes folder under session cwd (default `recipes`). */
  recipesRel: string
  /**
   * When true (default), recipes that declare `参数名=值` refuse to run until
   * the model shows the param form and calls again with `paramsConfirmed=true`.
   */
  requireParamConfirm: boolean
  /**
   * Private deploy overlay: seed local vault entries (id + name + login URL).
   * Optional username/password for private test accounts — applied via
   * `accounts set` (DPAPI) after lite is ready. Not for public OSS defaults.
   */
  systems: SystemEntryConfig[]
  /**
   * Start loopback accounts UI (127.0.0.1) for Settings form / browser page.
   * Default true. Passwords never enter the LLM.
   */
  accountsUi: boolean
  /** Preferred loopback port for accounts UI (default 19229). */
  accountsUiPort: number
}

/** One site entry for `config.systems` (private fork may include test credentials). */
export interface SystemEntryConfig {
  id: string
  name?: string
  url: string
  /** Optional — private deploy test account only. */
  username?: string
  /** Optional — private deploy test account only; never logged / never to LLM. */
  password?: string
}

/** Validate plugin config from cordis.yml. */
export const Config: z<Partial<Config>, Config> = z.object({
  binaryPath: z.string().default('webclaw-lite'),
  webclawHome: z.string().default(''),
  liteBaseUrl: z.string().default(''),
  ensureOnBoot: z.boolean().default(true),
  registerSkill: z.boolean().default(true),
  runTimeoutMs: z.number().min(1_000).default(600_000),
  probeTimeoutMs: z.number().min(1_000).default(120_000),
  recipesDir: z.string().default(''),
  recipesRel: z.string().default('recipes'),
  requireParamConfirm: z.boolean().default(true),
  systems: z
    .array(
      z.object({
        id: z.string(),
        name: z.string().default(''),
        url: z.string(),
        username: z.string().default(''),
        password: z.string().default(''),
      }),
    )
    .default([]),
  accountsUi: z.boolean().default(true),
  accountsUiPort: z.number().min(1).max(65_535).default(ACCOUNTS_UI_DEFAULT_PORT),
})

/**
 * Prefill local vault site entries from deploy config (url + optional credentials).
 */
async function ensureConfiguredSystems(
  config: Config,
  ensured: EnsureLiteResult,
  log: { info: (msg: string, ...args: unknown[]) => void; warn: (msg: string, ...args: unknown[]) => void },
): Promise<void> {
  const entries = config.systems ?? []
  if (entries.length === 0) {
    log.warn('webclaw: config.systems empty — vault overlay not applied')
    return
  }
  const env = liteEnvFrom(config, ensured, 60_000)
  const home = env.webclawHome?.trim() || ensured.webclawHome
  const normalized = entries
    .map(e => ({
      id: e.id?.trim() ?? '',
      name: e.name?.trim(),
      url: normalizeSystemUrl(e.url ?? ''),
      username: e.username?.trim() || '',
      password: e.password ?? '',
    }))
    .filter(e => e.id && e.url)

  // 1) Direct file write — reliable for `&` in URLs (no CLI/YAML truncation).
  try {
    const patched = patchSystemsFile(
      home,
      normalized.map(e => ({
        id: e.id,
        url: e.url,
        name: e.name,
        username: e.username || undefined,
      })),
    )
    log.info(
      'webclaw: systems file patched %s → %s',
      patched.path,
      patched.updated.join('; ') || '(none)',
    )
  } catch (error) {
    log.warn(
      'webclaw: systems file patch failed: %s',
      error instanceof Error ? error.message : error,
    )
  }

  // 2) URL ensure + optional credential set (encrypted by lite).
  for (const entry of normalized) {
    try {
      const result = await ensureAccount(env, {
        id: entry.id,
        url: entry.url,
        name: entry.name,
      })
      if (!result.ok) {
        log.warn('webclaw: accounts ensure failed id=%s: %s', entry.id, result.error)
        continue
      }
      log.info('webclaw: accounts ensure ok id=%s url=%s', entry.id, entry.url)
      if (entry.username && entry.password) {
        const set = await setAccount(env, {
          id: entry.id,
          user: entry.username,
          password: entry.password,
          url: entry.url,
          name: entry.name,
        })
        if (!set.ok) {
          log.warn('webclaw: accounts set failed id=%s: %s', entry.id, set.error)
        } else {
          log.info('webclaw: accounts set ok id=%s user=%s', entry.id, entry.username)
        }
      }
    } catch (error) {
      log.warn(
        'webclaw: accounts ensure error id=%s: %s',
        entry.id,
        error instanceof Error ? error.message : error,
      )
    }
  }
}

function formatParamForm(params: readonly RecipeParamKv[]): string {
  const rows = params.map(p => {
    // Strip accidental verb glue from older clients (家中有事登录EHR系统).
    let value = p.value || '（空）'
    value = value.replace(/登录[\u4e00-\u9fffA-Za-z0-9_-]*$/u, '').trim() || value
    return `- ${p.name}：${value}`
  })
  return [
    '【请编辑参数后再跑 — 表内多为占位值，默认不当真】',
    '',
    '把下表一次性展示给用户，请他们直接改字段（或整表贴回）。',
    '不要逐项口头追问；不要主动劝「用默认」。',
    ...rows,
    '',
    '用户改完后再次 webclaw_run：paramsConfirmed=true，并传 params 对象（不要传 JSON 字符串），如 {"公司":"山西阳泉","月份":"2026-07"}。',
    '仅当用户明确说「用默认 / 不用改」时：paramsConfirmed=true，params 可省略。',
    '确认前禁止 paramsConfirmed=true，禁止静默用占位值开跑。',
  ].join('\n')
}

/** Plain JSON rows for tool results (dsh-tools JsonValue). */
function paramsJson(
  params: readonly RecipeParamKv[],
): Array<{ name: string; value: string }> {
  return params.map(p => ({ name: p.name, value: p.value }))
}

function recipesCfg(config: Config): RecipesConfig {
  return {
    recipesDir: config.recipesDir || undefined,
    recipesRel: config.recipesRel || 'recipes',
    webclawHome: config.webclawHome || undefined,
  }
}

function liteEnvFrom(
  config: Config,
  ensured: EnsureLiteResult | undefined,
  timeoutMs?: number,
): LiteEnv {
  return {
    binaryPath: ensured?.binaryPath ?? resolveBinaryPath(config),
    webclawHome: ensured?.webclawHome ?? resolveWebclawHome(config),
    liteBaseUrl: ensured?.liteBaseUrl ?? resolveLiteBaseUrl(config),
    timeoutMs: timeoutMs ?? config.runTimeoutMs,
  }
}

function parseJsonObject(text: string): Record<string, any> | undefined {
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start < 0 || end <= start) return undefined
  try {
    return JSON.parse(text.slice(start, end + 1)) as Record<string, any>
  } catch {
    return undefined
  }
}

/**
 * Register webclaw tools and optional skill; optionally ensure lite at boot.
 */
export function apply(ctx: Context, config: Config): void {
  if (config.registerSkill) {
    // Wait for skills without hard-requiring it in `inject` (keeps tools usable alone).
    ctx.inject(['skills'], skillCtx => {
      registerBundledSkill(skillCtx)
    })
  }

  const log = {
    info: (msg: string, ...args: unknown[]) => ctx.logger.info(msg, ...args),
    warn: (msg: string, ...args: unknown[]) => ctx.logger.warn(msg, ...args),
  }

  let lastEnsure: EnsureLiteResult | undefined
  let accountsUi: AccountsUiHandle | null = null

  const ready = async (): Promise<EnsureLiteResult> => {
    lastEnsure = await ensureLite(config, log)
    try {
      await startUi(lastEnsure)
    } catch (error) {
      log.warn(
        'webclaw: accounts UI start failed: %s',
        error instanceof Error ? error.message : error,
      )
    }
    return lastEnsure
  }

  const env = async (timeoutMs?: number): Promise<LiteEnv> => {
    const ensured = await ready()
    return liteEnvFrom(config, ensured, timeoutMs)
  }

  const startUi = async (ensured: EnsureLiteResult): Promise<void> => {
    if (accountsUi || !config.accountsUi) return
    // Use the already-resolved lite env — do not call ready()/env() (re-entrancy).
    accountsUi = await startAccountsUiServer({
      enabled: config.accountsUi,
      preferredPort: config.accountsUiPort || ACCOUNTS_UI_DEFAULT_PORT,
      getEnv: async () => liteEnvFrom(config, ensured, 60_000),
      home: ensured.webclawHome,
      binaryPath: ensured.binaryPath,
      log,
    })
  }

  ctx.effect(() => {
    return () => {
      accountsUi?.close()
      accountsUi = null
    }
  }, 'webclaw: accounts UI lifetime')

  if (config.ensureOnBoot || (config.systems?.length ?? 0) > 0 || config.accountsUi) {
    void ready()
      .then(async r => {
        ctx.logger.info(
          'webclaw: ensureOnBoot ok (downloaded=%s path=%s)',
          String(r.downloaded),
          r.binaryPath,
        )
        await ensureConfiguredSystems(config, r, log)
        await startUi(r)
      })
      .catch((error: unknown) => {
        ctx.logger.warn(
          'webclaw: ensureOnBoot failed: %s',
          error instanceof Error ? error.message : error,
        )
      })
  }

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_status',
          description:
            'Check local webclaw-lite readiness (Chrome path, product, chromeOk). Prefer this before run when unsure the CLI is installed. On Windows, missing lite is auto-downloaded from GitHub Releases.',
          parameters: {
            run_id: {
              type: 'string',
              description: 'Optional run id to query waiting-human state',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => [
              { type: 'text', text: JSON.stringify(value, null, 2) },
            ],
          },
          async execute(args): Promise<Record<string, any>> {
            let ensured: EnsureLiteResult
            try {
              ensured = await ready()
            } catch (error) {
              return {
                ok: false,
                product: 'webclaw-lite',
                error: error instanceof Error ? error.message : String(error),
                binaryPath: resolveBinaryPath(config),
              }
            }
            const argv = ['status']
            if (args.run_id?.trim()) {
              argv.push('--run-id', args.run_id.trim())
            }
            const result = await runLite(liteEnvFrom(config, ensured), argv)
            const text = `${result.stdout}\n${result.stderr}`.trim()
            const start = text.indexOf('{')
            const end = text.lastIndexOf('}')
            let status: Record<string, any> = {
              ok: false,
              exitCode: result.code,
              raw: text.slice(0, 2000),
            }
            if (start >= 0 && end > start) {
              try {
                status = JSON.parse(text.slice(start, end + 1)) as Record<string, any>
              } catch {
                /* keep raw */
              }
            }
            status.exitCode = result.code ?? -1
            status.binaryPath = ensured.binaryPath
            return status
          },
        }),
      ),
    'webclaw: tool webclaw_status',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_run',
          description:
            'Execute a Chinese natural-language browser recipe via webclaw-lite in ONE call. Full recipe/.webclaw → pass ENTIRE body (never probe step-by-step). Prefab 参数名=值 are PLACEHOLDERS users usually must edit: first call returns needParams — show the table once for editing (no verbal Q&A per field; do not push「用默认」), then paramsConfirmed=true + params JSON.「用默认」only if user explicitly says so. Missing site password does NOT block. No CSS/Playwright.',
          parameters: {
            recipe: {
              type: 'string',
              description:
                'Inline recipe body (Chinese commas ，). Prefer the FULL user-supplied recipe in one call. Short segments only when authoring without a complete recipe. Mutually exclusive with recipePath.',
            },
            recipePath: {
              type: 'string',
              description:
                'Path to .webclaw file: workspace-relative, recipes/<name>, absolute, or attachment read path. Prefer this when a frozen/uploaded file already exists.',
            },
            title: {
              type: 'string',
              description: 'Optional run title',
            },
            // Loose schema: models pass object or JSON string; Host parses both.
            // Must declare type — typeless params make defineTool abort the whole fiber.
            params: {
              type: 'json',
              description:
                'Param overrides. Prefer object {"休假项目":"年假",...}; JSON string also ok.',
            },
            paramsConfirmed: {
              type: 'boolean',
              description:
                'Set true only after the user edited the 参数 table (or explicitly said「用默认」). Required to run when the recipe declares 参数… — most prefab values need editing.',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              const v = value as {
                ok?: boolean
                needParams?: boolean
                form?: string
                setupHint?: string
                run_dir?: string
                error?: string
                eventsTail?: string
                recipePath?: string
              }
              if (v.needParams && v.form) {
                return [{ type: 'text', text: v.form }]
              }
              const lines = [
                v.ok ? 'webclaw_run: ok' : 'webclaw_run: failed',
                v.recipePath ? `recipePath: ${v.recipePath}` : '',
                v.run_dir ? `run_dir: ${v.run_dir}` : '',
                v.error ? `error: ${v.error}` : '',
                v.setupHint ? v.setupHint : '',
                v.eventsTail ? v.eventsTail.slice(-4000) : '',
              ].filter(Boolean)
              return [{ type: 'text', text: lines.join('\n') }]
            },
          },
          async execute(args, exec): Promise<Record<string, any>> {
            const inline = args.recipe?.trim() ?? ''
            const pathArg = args.recipePath?.trim() ?? ''
            if (inline && pathArg) {
              throw new Error('pass either recipe or recipePath, not both')
            }
            if (!inline && !pathArg) {
              throw new Error('recipe or recipePath is required')
            }
            const cwd = agentCwd(exec)
            const sourcePath = pathArg
              ? resolveRecipePath(recipesCfg(config), pathArg, cwd)
              : undefined
            const rawContent = inline
              ? inline
              : readFileSync(sourcePath!, { encoding: 'utf8' })

            const declared = extractRecipeParams(rawContent)
            // Tool schema may deliver object or string; parseParamsArg accepts both.
            const overrides = parseParamsArg(
              (args as { params?: unknown }).params,
            )
            const confirmed = args.paramsConfirmed === true

            if (
              config.requireParamConfirm &&
              declared.length > 0 &&
              !confirmed
            ) {
              const preview = applyParamOverrides(rawContent, overrides)
              return {
                ok: false,
                needParams: true,
                params: paramsJson(preview.params),
                form: formatParamForm(preview.params),
                error:
                  'needParams: show the editable param table once; wait for user edits (not verbal interview); do not start the browser yet',
                recipePath: sourcePath ?? '',
              }
            }

            const merged = applyParamOverrides(rawContent, overrides)

            // Soft status only — never block open for missing password (SSO / session OK).
            const lite = await env()
            // Re-apply deploy systems (URL + optional test credentials).
            if ((config.systems?.length ?? 0) > 0) {
              const ensured = await ready()
              await ensureConfiguredSystems(config, ensured, log)
            }
            const listed = await listAccounts(lite)
            const missingCreds = missingCredentialsForRecipe(
              merged.content,
              listed.items,
            )

            const path =
              declared.length > 0 || overrides
                ? writeTempRecipe(merged.content)
                : sourcePath ?? writeTempRecipe(rawContent)

            const argv = ['run', '--recipe', path]
            if (args.title?.trim()) {
              argv.push('--title', args.title.trim())
            }
            const result = await runLite(lite, argv)
            const done = parseDoneEvent(result.stdout)
            // Prefer JSONL done (ok/fail are step counts). Exit 0 alone is not enough
            // if done is missing; exit non-zero with fail>0 is a real failure.
            const ok =
              done != null
                ? done.ok === true && (result.code === 0 || result.code == null)
                : result.code === 0
            const errorText =
              done?.error ||
              (ok
                ? ''
                : (result.stderr ||
                    (done
                      ? `run failed (ok=${done.okCount ?? '?'}, fail=${done.fail ?? '?'})`
                      : 'run failed: no done event')
                  ).slice(0, 500))
            const needsCredHint =
              !ok &&
              missingCreds.length > 0 &&
              /账号|密码|网站账号|凭据/u.test(errorText)
            // Omit undefined fields — dsh tools require lossless JSON (no nested undefined).
            return {
              ok,
              exitCode: result.code ?? -1,
              okCount: done?.okCount ?? null,
              fail: done?.fail ?? null,
              run_dir: done?.run_dir ?? '',
              params: paramsJson(merged.params),
              ...(missingCreds.length > 0
                ? { missingCredentials: missingCreds }
                : {}),
              ...(needsCredHint ? { setupHint: CREDENTIALS_SETUP_HINT } : {}),
              error: errorText,
              eventsTail: redactSecrets(result.stdout.slice(-8000)),
              recipePath: path,
            }
          },
        }),
      ),
    'webclaw: tool webclaw_run',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_accounts_status',
          description:
            'List local site accounts (id/name/url/username/has_password only — NEVER passwords). SSO URLs (sso=lark etc.) need NO password — just webclaw_run. Never ask for passwords or invent `accounts set` CLI. If URL wrong, tell user to fix it in 设置 → 网站账号 (URL only).',
          parameters: {},
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              return [
                {
                  type: 'text',
                  text: typeof value === 'object'
                    ? JSON.stringify(value, null, 2)
                    : String(value),
                },
              ]
            },
          },
          async execute(): Promise<Record<string, any>> {
            const ensured = await ready()
            if ((config.systems?.length ?? 0) > 0) {
              await ensureConfiguredSystems(config, ensured, log)
            }
            const listed = await listAccounts(liteEnvFrom(config, ensured))
            const needsUrl = listed.items.filter(it => !it.url.trim())
            const needsSsoUrl = listed.items.filter(
              it =>
                !!it.url.trim() &&
                !isSsoAccountUrl(it.url) &&
                /hcmcloud|\behr\b/i.test(`${it.id} ${it.name} ${it.url}`),
            )
            return {
              ok: listed.ok,
              type: 'accounts',
              items: listed.items.map(it => ({
                id: it.id,
                name: it.name,
                url: it.url,
                username: it.username,
                has_password: it.has_password,
                sso: isSsoAccountUrl(it.url),
                // Never signal passwordRequired — models invent `accounts set` otherwise.
                passwordRequired: false,
              })),
              incompleteCredentials: needsUrl.length > 0,
              incomplete: needsUrl.map(it => ({
                id: it.id,
                name: it.name,
                reason: 'missing_url',
              })),
              setupHint:
                needsUrl.length > 0
                  ? CREDENTIALS_SETUP_HINT
                  : needsSsoUrl.length > 0
                    ? 'EHR URL missing sso=lark — user should fix 设置 → 网站账号 URL only. Never ask for passwords or invent accounts set CLI.'
                    : 'OK to webclaw_run. Never ask for passwords or invent accounts set CLI.',
              error: listed.error || '',
              binaryPath: ensured.binaryPath,
            }
          },
        }),
      ),
    'webclaw: tool webclaw_accounts_status',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_inspect_params',
          description:
            'Extract editable 参数名=值 fields from a recipe. Prefer letting webclaw_run return needParams. Show table once for user to edit; then webclaw_run with paramsConfirmed=true + params JSON. Do not interview field-by-field.',
          parameters: {
            recipe: {
              type: 'string',
              description: 'Inline recipe body. Mutually exclusive with recipePath.',
            },
            recipePath: {
              type: 'string',
              description: 'Path to .webclaw file',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              const v = value as {
                form?: string
                params?: RecipeParamKv[]
                error?: string
              }
              if (v.form) return [{ type: 'text', text: v.form }]
              if (v.error) return [{ type: 'text', text: v.error }]
              return [
                {
                  type: 'text',
                  text: JSON.stringify(v.params ?? [], null, 2),
                },
              ]
            },
          },
          async execute(args, exec): Promise<Record<string, any>> {
            const inline = args.recipe?.trim() ?? ''
            const pathArg = args.recipePath?.trim() ?? ''
            if (inline && pathArg) {
              throw new Error('pass either recipe or recipePath, not both')
            }
            if (!inline && !pathArg) {
              throw new Error('recipe or recipePath is required')
            }
            const cwd = agentCwd(exec)
            const content = inline
              ? inline
              : readFileSync(
                  resolveRecipePath(recipesCfg(config), pathArg, cwd),
                  { encoding: 'utf8' },
                )
            const params = extractRecipeParams(content)
            return {
              ok: true,
              params: paramsJson(params),
              form:
                params.length > 0
                  ? formatParamForm(params)
                  : '（该菜谱没有「参数」声明，可直接 webclaw_run）',
              hint:
                params.length > 0
                  ? 'Show editable table once; after user edits → webclaw_run(..., paramsConfirmed=true, params={...})'
                  : 'No static params; run directly',
            }
          },
        }),
      ),
    'webclaw: tool webclaw_inspect_params',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_freeze',
          description:
            'Freeze a finished recipe as UTF-8 recipes/<name>.webclaw under the session workspace (or WEBCLAW_HOME/recipes). After freeze, replay with webclaw_run(recipePath). Tell the user to open the file in Files for sidebar preview.',
          parameters: {
            name: {
              type: 'string',
              required: true,
              description: 'Recipe stem (becomes <name>.webclaw)',
            },
            recipe: {
              type: 'string',
              required: true,
              description: 'Full frozen recipe body',
            },
            title: {
              type: 'string',
              description: 'Optional # title line prepended when body has no leading #',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              const v = value as { path?: string; name?: string; hint?: string }
              return [
                {
                  type: 'text',
                  text: [
                    `webclaw_freeze: ${v.name ?? ''}`,
                    v.path ? `path: ${v.path}` : '',
                    v.hint ?? '',
                  ]
                    .filter(Boolean)
                    .join('\n'),
                },
              ]
            },
          },
          async execute(args, exec) {
            const saved = freezeRecipe({
              config: recipesCfg(config),
              name: args.name,
              recipe: args.recipe,
              title: args.title,
              cwd: agentCwd(exec),
            })
            return {
              ok: true,
              ...saved,
              hint: 'Open this path in Files for plain-text preview; edit via open-in-app or fs tools; replay with webclaw_run(recipePath).',
            }
          },
        }),
      ),
    'webclaw: tool webclaw_freeze',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_list_recipes',
          description: 'List frozen .webclaw files in the local recipes library (workspace recipes/ or WEBCLAW_HOME/recipes).',
          parameters: {},
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => [
              { type: 'text', text: JSON.stringify(value, null, 2) },
            ],
          },
          async execute(_args, exec) {
            const listed = listRecipes(recipesCfg(config), agentCwd(exec))
            return { type: 'recipes', ...listed }
          },
        }),
      ),
    'webclaw: tool webclaw_list_recipes',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_import',
          description:
            'Copy an existing .webclaw (attachment path or any file) into the recipes library for durable replay/edit. Prefer this over treating chat attachments as the long-term store.',
          parameters: {
            sourcePath: {
              type: 'string',
              required: true,
              description: 'Absolute or resolvable path to source .webclaw',
            },
            name: {
              type: 'string',
              description: 'Optional stem override for the destination file',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => [
              { type: 'text', text: JSON.stringify(value, null, 2) },
            ],
          },
          async execute(args, exec) {
            const saved = importRecipe({
              config: recipesCfg(config),
              sourcePath: args.sourcePath,
              name: args.name,
              cwd: agentCwd(exec),
            })
            return {
              ok: true,
              ...saved,
              hint: 'Imported into recipes library. Preview in Files; replay with webclaw_run(recipePath).',
            }
          },
        }),
      ),
    'webclaw: tool webclaw_import',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_probe',
          description:
            'Authoring-only when NO complete recipe exists yet: scan the page (or open url) and return textCmd/indexCmd/clickCmd/inputCmd. NEVER call this if the user already pasted a full recipe or gave recipePath—use webclaw_run once instead. After a failed full run, at most one probe to edit the recipe, then re-run the full body. Do not invent labels.',
          parameters: {
            url: {
              type: 'string',
              description: 'Optional. First hop or explicit site change only; omit after navigation to scan current page',
            },
            types: {
              type: 'string',
              description: 'Comma-separated types, default 输入框,按钮,图标按钮,链接',
            },
            search: {
              type: 'string',
              description: 'Optional keyword filter',
            },
            limit: {
              type: 'number',
              description: 'Max candidates (default 60)',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              const v = value as {
                ok?: boolean
                url?: string
                candidateCount?: number
                hint?: string
                candidates?: unknown[]
              }
              const head = [
                v.ok === false ? 'webclaw_probe: failed' : 'webclaw_probe: ok',
                v.url ? `url: ${v.url}` : '',
                typeof v.candidateCount === 'number'
                  ? `candidates: ${v.candidateCount}`
                  : '',
                v.hint || '',
              ].filter(Boolean)
              const sample = Array.isArray(v.candidates)
                ? v.candidates.slice(0, 12)
                : []
              return [
                {
                  type: 'text',
                  text: `${head.join('\n')}\n${JSON.stringify(sample, null, 2)}`,
                },
              ]
            },
          },
          async execute(args) {
            const argv = ['probe']
            if (args.url?.trim()) {
              argv.push('--url', args.url.trim())
            }
            if (args.types?.trim()) {
              argv.push('--types', args.types.trim())
            }
            if (args.search?.trim()) {
              argv.push('--search', args.search.trim())
            }
            if (typeof args.limit === 'number' && Number.isFinite(args.limit) && args.limit > 0) {
              argv.push('--limit', String(Math.floor(args.limit)))
            }
            const ensured = await ready()
            const result = await runLite(
              liteEnvFrom(config, ensured, config.probeTimeoutMs),
              argv,
            )
            const text = `${result.stdout}\n${result.stderr}`.trim()
            const parsed = parseJsonObject(result.stdout) ?? parseJsonObject(text)
            if (parsed && parsed.type === 'probe') {
              Object.assign(parsed, {
                exitCode: result.code ?? -1,
                binaryPath: ensured.binaryPath,
              })
              return parsed
            }
            return {
              type: 'probe',
              ok: false,
              exitCode: result.code ?? -1,
              error: (result.stderr || result.stdout || 'probe failed').slice(0, 500),
              raw: text.slice(0, 2000),
              binaryPath: ensured.binaryPath,
            }
          },
        }),
      ),
    'webclaw: tool webclaw_probe',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_human_reply',
          description:
            'Answer a webclaw need_human pause: captcha text (--text), slider confirmation (--ok), or cancel.',
          parameters: {
            mode: {
              type: 'string',
              required: true,
              description: 'One of: text | ok | cancel',
            },
            text: {
              type: 'string',
              description: 'Captcha code when mode=text',
            },
            run_dir: {
              type: 'string',
              description: 'Optional run directory containing human/',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => [
              { type: 'text', text: JSON.stringify(value) },
            ],
          },
          async execute(args) {
            const mode = args.mode.trim().toLowerCase()
            const argv = ['human-reply']
            if (mode === 'ok') argv.push('--ok')
            else if (mode === 'cancel') argv.push('--cancel')
            else if (mode === 'text') {
              if (!args.text?.trim()) throw new Error('text required when mode=text')
              argv.push('--text', args.text.trim())
            } else {
              throw new Error('mode must be text|ok|cancel')
            }
            if (args.run_dir?.trim()) {
              argv.push('--run-dir', args.run_dir.trim())
            }
            const result = await runLite(await env(), argv)
            return {
              exitCode: result.code ?? -1,
              stdout: result.stdout.trim().slice(0, 2000),
              stderr: result.stderr.trim().slice(0, 500),
            }
          },
        }),
      ),
    'webclaw: tool webclaw_human_reply',
  )

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_cancel',
          description: 'Request cancellation of the current webclaw-lite run (writes control/cancel).',
          parameters: {},
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => [
              { type: 'text', text: JSON.stringify(value) },
            ],
          },
          async execute() {
            const result = await runLite(await env(), ['cancel'])
            return {
              exitCode: result.code ?? -1,
              stdout: result.stdout.trim().slice(0, 1000),
              stderr: result.stderr.trim().slice(0, 500),
            }
          },
        }),
      ),
    'webclaw: tool webclaw_cancel',
  )
}
