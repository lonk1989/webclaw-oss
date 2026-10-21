/**
 * @lonk1989/webclaw — recipe-level browser RPA for DeepSeek Harness.
 *
 * Registers task tools that spawn webclaw-lite. Does not claim ctx.browserUse.
 * On Windows, ensureOnBoot (default true) downloads lite from GitHub Releases.
 * @module
 */

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
})

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

  const ready = async (): Promise<EnsureLiteResult> => {
    lastEnsure = await ensureLite(config, log)
    return lastEnsure
  }

  const env = async (timeoutMs?: number): Promise<LiteEnv> => {
    const ensured = await ready()
    return liteEnvFrom(config, ensured, timeoutMs)
  }

  if (config.ensureOnBoot) {
    void ready()
      .then(r => {
        ctx.logger.info(
          'webclaw: ensureOnBoot ok (downloaded=%s path=%s)',
          String(r.downloaded),
          r.binaryPath,
        )
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
            'Execute a Chinese natural-language browser recipe via webclaw-lite. Pass either inline `recipe` (authoring short segment) or `recipePath` (frozen .webclaw / attachment path). Replay: prefer recipePath only. No CSS/Playwright.',
          parameters: {
            recipe: {
              type: 'string',
              description: 'Inline recipe body (Chinese commas ，). Mutually exclusive with recipePath.',
            },
            recipePath: {
              type: 'string',
              description:
                'Path to .webclaw file: workspace-relative, recipes/<name>, absolute, or attachment read path',
            },
            title: {
              type: 'string',
              description: 'Optional run title',
            },
          },
          output: {
            schema: { type: 'object', additionalProperties: true },
            render: (_args, value) => {
              const v = value as {
                ok?: boolean
                run_dir?: string
                error?: string
                eventsTail?: string
                recipePath?: string
              }
              const lines = [
                v.ok ? 'webclaw_run: ok' : 'webclaw_run: failed',
                v.recipePath ? `recipePath: ${v.recipePath}` : '',
                v.run_dir ? `run_dir: ${v.run_dir}` : '',
                v.error ? `error: ${v.error}` : '',
                v.eventsTail ? v.eventsTail.slice(-4000) : '',
              ].filter(Boolean)
              return [{ type: 'text', text: lines.join('\n') }]
            },
          },
          async execute(args, exec) {
            const inline = args.recipe?.trim() ?? ''
            const pathArg = args.recipePath?.trim() ?? ''
            if (inline && pathArg) {
              throw new Error('pass either recipe or recipePath, not both')
            }
            if (!inline && !pathArg) {
              throw new Error('recipe or recipePath is required')
            }
            const cwd = agentCwd(exec)
            const path = pathArg
              ? resolveRecipePath(recipesCfg(config), pathArg, cwd)
              : writeTempRecipe(inline)
            const argv = ['run', '--recipe', path]
            if (args.title?.trim()) {
              argv.push('--title', args.title.trim())
            }
            const result = await runLite(await env(), argv)
            const done = parseDoneEvent(result.stdout)
            // Prefer JSONL done (ok/fail are step counts). Exit 0 alone is not enough
            // if done is missing; exit non-zero with fail>0 is a real failure.
            const ok =
              done != null
                ? done.ok === true && (result.code === 0 || result.code == null)
                : result.code === 0
            return {
              ok,
              exitCode: result.code ?? -1,
              okCount: done?.okCount ?? null,
              fail: done?.fail ?? null,
              run_dir: done?.run_dir ?? '',
              error:
                done?.error ||
                (ok
                  ? ''
                  : (result.stderr ||
                      (done
                        ? `run failed (ok=${done.okCount ?? '?'}, fail=${done.fail ?? '?'})`
                        : 'run failed: no done event')
                    ).slice(0, 500)),
              eventsTail: result.stdout.slice(-8000),
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
            'Authoring-only: scan the current browser page (or open url first) and return recipe-ready textCmd/indexCmd/clickCmd/inputCmd. Multi-round: probe → short webclaw_run → probe again without url. Do not invent control labels; do not use for stable replay.',
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
