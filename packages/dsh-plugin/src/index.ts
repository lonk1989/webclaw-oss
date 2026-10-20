/**
 * @lonk1989/dsh-plugin — recipe-level browser RPA for DeepSeek Harness.
 *
 * Registers task tools that spawn webclaw-lite. Does not claim ctx.browserUse.
 * @module
 */

import { existsSync } from 'node:fs'
import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from '@deepseek-ai/dsh-tools'
import z from '@deepseek-ai/schemastery'
import { parseDoneEvent, runLite, writeTempRecipe, type LiteEnv } from './spawn.js'
import { registerBundledSkill } from './skill.js'

/** Cordis plugin id. */
export const name = 'webclaw'

/** Tool registry must exist; skills are optional (registered via Reflect when present). */
export const inject = ['tools']

/** Deploy-time options for locating and invoking webclaw-lite. */
export interface Config {
  /** Absolute path or PATH command for webclaw-lite. */
  binaryPath: string
  /** Maps to WEBCLAW_HOME. */
  webclawHome: string
  /** Maps to WEBCLAW_LITE_BASE (install/update); empty skips remote update. */
  liteBaseUrl: string
  /** When true, run `install` once at boot if binary exists. Default false. */
  ensureOnBoot: boolean
  /** Register packaged SKILL.md. Default true. */
  registerSkill: boolean
  /** Max ms for a single `webclaw_run` (default 10 minutes). */
  runTimeoutMs: number
}

/** Validate plugin config from cordis.yml. */
export const Config: z<Partial<Config>, Config> = z.object({
  binaryPath: z.string().default('webclaw-lite'),
  webclawHome: z.string().default(''),
  liteBaseUrl: z.string().default(''),
  ensureOnBoot: z.boolean().default(false),
  registerSkill: z.boolean().default(true),
  runTimeoutMs: z.number().min(1_000).default(600_000),
})

function liteEnv(config: Config): LiteEnv {
  return {
    binaryPath: config.binaryPath,
    webclawHome: config.webclawHome || undefined,
    liteBaseUrl: config.liteBaseUrl || undefined,
    timeoutMs: config.runTimeoutMs,
  }
}

/**
 * Register webclaw tools and optional skill; optionally ensure install marker.
 */
export function apply(ctx: Context, config: Config): void {
  if (config.registerSkill) {
    registerBundledSkill(ctx)
  }

  if (config.ensureOnBoot && existsSync(config.binaryPath)) {
    void runLite(liteEnv(config), ['install']).then(result => {
      if (result.code === 0) {
        ctx.logger.info('webclaw: ensureOnBoot install ok')
      } else {
        ctx.logger.warn(
          'webclaw: ensureOnBoot install failed: %s',
          (result.stderr || result.stdout || 'non-zero').slice(0, 300),
        )
      }
    })
  }

  const env = () => liteEnv(config)

  ctx.effect(
    () =>
      ctx.tools.register(
        defineTool({
          name: 'webclaw_status',
          description:
            'Check local webclaw-lite readiness (Chrome path, product, chromeOk). Prefer this before run when unsure the CLI is installed.',
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
          async execute(args) {
            const argv = ['status']
            if (args.run_id?.trim()) {
              argv.push('--run-id', args.run_id.trim())
            }
            const result = await runLite(env(), argv)
            const text = `${result.stdout}\n${result.stderr}`.trim()
            const start = text.indexOf('{')
            const end = text.lastIndexOf('}')
            let status: Record<string, unknown> = {
              ok: false,
              exitCode: result.code,
              raw: text.slice(0, 2000),
            }
            if (start >= 0 && end > start) {
              try {
                status = JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>
              } catch {
                /* keep raw */
              }
            }
            return {
              ...status,
              exitCode: result.code ?? -1,
              binaryPath: config.binaryPath,
            }
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
            'Execute a Chinese natural-language browser recipe via webclaw-lite (打开/点击/等待/输入…). Do not pass CSS selectors or Playwright steps. Write the full recipe as UTF-8 text in `recipe`.',
          parameters: {
            recipe: {
              type: 'string',
              required: true,
              description: 'Recipe body, steps separated by Chinese commas ，',
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
              }
              const lines = [
                v.ok ? 'webclaw_run: ok' : 'webclaw_run: failed',
                v.run_dir ? `run_dir: ${v.run_dir}` : '',
                v.error ? `error: ${v.error}` : '',
                v.eventsTail ? v.eventsTail.slice(-4000) : '',
              ].filter(Boolean)
              return [{ type: 'text', text: lines.join('\n') }]
            },
          },
          async execute(args) {
            const recipe = args.recipe.trim()
            if (!recipe) {
              throw new Error('recipe must be non-empty')
            }
            const path = writeTempRecipe(recipe)
            const argv = ['run', '--recipe', path]
            if (args.title?.trim()) {
              argv.push('--title', args.title.trim())
            }
            const result = await runLite(env(), argv)
            const done = parseDoneEvent(result.stdout)
            const ok = done?.ok === true && result.code === 0
            return {
              ok,
              exitCode: result.code ?? -1,
              run_dir: done?.run_dir ?? '',
              error: done?.error || (ok ? '' : (result.stderr || 'run failed').slice(0, 500)),
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
            const result = await runLite(env(), argv)
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
            const result = await runLite(env(), ['cancel'])
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
