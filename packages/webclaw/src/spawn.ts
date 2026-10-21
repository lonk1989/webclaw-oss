/**
 * Spawn webclaw-lite and capture stdout/stderr.
 */

import { spawn } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'

const OUTPUT_CAP = 2 * 1024 * 1024

export interface SpawnResult {
  readonly code: number | null
  readonly stdout: string
  readonly stderr: string
}

export interface LiteEnv {
  readonly binaryPath: string
  readonly webclawHome?: string
  readonly liteBaseUrl?: string
  readonly timeoutMs?: number
}

/**
 * Run webclaw-lite with argv; merges WEBCLAW_* env.
 */
export function runLite(
  env: LiteEnv,
  args: readonly string[],
): Promise<SpawnResult> {
  const timeoutMs = env.timeoutMs ?? 600_000
  const childEnv: NodeJS.ProcessEnv = { ...process.env }
  if (env.webclawHome?.trim()) {
    childEnv.WEBCLAW_HOME = env.webclawHome.trim()
  }
  if (env.liteBaseUrl?.trim()) {
    childEnv.WEBCLAW_LITE_BASE = env.liteBaseUrl.trim()
  }

  return new Promise(resolve => {
    const child = spawn(env.binaryPath, [...args], {
      env: childEnv,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    let settled = false
    const finish = (code: number | null): void => {
      if (settled) return
      settled = true
      resolve({ code, stdout, stderr })
    }
    const timer = setTimeout(() => {
      child.kill()
      finish(null)
    }, timeoutMs)
    child.stdout?.on('data', (chunk: Buffer) => {
      if (stdout.length < OUTPUT_CAP) {
        stdout += chunk.toString('utf8').slice(0, OUTPUT_CAP - stdout.length)
      }
    })
    child.stderr?.on('data', (chunk: Buffer) => {
      if (stderr.length < OUTPUT_CAP) {
        stderr += chunk.toString('utf8').slice(0, OUTPUT_CAP - stderr.length)
      }
    })
    child.on('error', () => {
      clearTimeout(timer)
      finish(null)
    })
    child.on('close', code => {
      clearTimeout(timer)
      finish(code)
    })
  })
}

/**
 * Write a UTF-8 recipe file under the OS temp dir and return its path.
 */
export function writeTempRecipe(recipe: string): string {
  const dir = join(tmpdir(), 'webclaw-dsh')
  mkdirSync(dir, { recursive: true })
  const path = join(dir, `recipe-${randomUUID()}.webclaw`)
  writeFileSync(path, recipe, { encoding: 'utf8' })
  return path
}

/**
 * Parse the last JSONL `done` event from run stdout.
 */
export function parseDoneEvent(stdout: string): {
  readonly ok: boolean
  readonly fail?: number
  readonly error?: string
  readonly run_dir?: string
} | undefined {
  const lines = stdout.split(/\r?\n/).map(l => l.trim()).filter(Boolean)
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i]!
    if (!line.includes('"done"')) continue
    try {
      const row = JSON.parse(line) as {
        type?: string
        ok?: boolean
        fail?: number
        error?: string
        run_dir?: string
      }
      if (row.type === 'done') {
        return {
          ok: row.ok === true,
          fail: row.fail,
          error: row.error,
          run_dir: row.run_dir,
        }
      }
    } catch {
      continue
    }
  }
  return undefined
}
