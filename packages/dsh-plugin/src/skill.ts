/**
 * Load and register the bundled webclaw-lite SKILL.md.
 */

import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Context } from '@deepseek-ai/cordis'

interface SkillsHost {
  register(skill: {
    readonly name: string
    readonly description: string
    readonly content: string
    readonly source?: string
  }): () => void
}

/** Resolve packaged assets/SKILL.md next to the compiled plugin. */
export function bundledSkillPath(): string {
  const here = dirname(fileURLToPath(import.meta.url))
  const candidates = [
    join(here, '../assets/SKILL.md'),
    join(here, '../../assets/SKILL.md'),
  ]
  return candidates.find(path => existsSync(path)) ?? candidates[0]!
}

/**
 * Parse name/description/body from YAML frontmatter SKILL.md.
 */
export function parseSkillMarkdown(raw: string): {
  readonly name: string
  readonly description: string
  readonly content: string
} | undefined {
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/u.exec(raw)
  if (match?.[1] === undefined) return undefined
  const block = match[1]
  const name = /^\s*name:\s*(.+)\s*$/m.exec(block)?.[1]?.trim().replace(/^['"]|['"]$/g, '') ?? ''
  const descFolded = /description:\s*>-\s*\n((?:[ \t]+.+\n?)+)/.exec(block)
  let description = ''
  if (descFolded?.[1]) {
    description = descFolded[1]
      .split(/\r?\n/)
      .map(line => line.replace(/^[ \t]+/, '').trim())
      .filter(line => line !== '')
      .join(' ')
  } else {
    description = /^\s*description:\s*(.+)\s*$/m.exec(block)?.[1]?.trim().replace(/^['"]|['"]$/g, '') ?? ''
  }
  const content = raw.slice(match[0].length).trim()
  if (name === '' || description === '' || content === '') return undefined
  return { name, description, content }
}

/**
 * Register the prefab skill into the host catalog.
 */
export function registerBundledSkill(ctx: Context): boolean {
  const host = Reflect.get(ctx, 'skills') as SkillsHost | undefined
  if (host === undefined) return false
  const path = bundledSkillPath()
  if (!existsSync(path)) {
    ctx.logger.warn('webclaw: bundled SKILL.md missing at %s', path)
    return false
  }
  let parsed: ReturnType<typeof parseSkillMarkdown>
  try {
    parsed = parseSkillMarkdown(readFileSync(path, 'utf8'))
  } catch (error) {
    ctx.logger.warn(
      'webclaw: failed to read SKILL.md: %s',
      error instanceof Error ? error.message : error,
    )
    return false
  }
  if (parsed === undefined) {
    ctx.logger.warn('webclaw: SKILL.md has invalid frontmatter')
    return false
  }
  try {
    host.register({
      name: parsed.name,
      description: parsed.description.slice(0, 300),
      content: [parsed.name, parsed.description, parsed.content].join('\n\n'),
      source: 'runtime',
    })
    ctx.logger.info('webclaw: registered bundled skill %s', parsed.name)
    return true
  } catch (error) {
    ctx.logger.warn(
      'webclaw: register skill failed: %s',
      error instanceof Error ? error.message : error,
    )
    return false
  }
}
