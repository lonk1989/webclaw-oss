/**
 * Offline smoke: local HTTP release dir → download + sha256 + stamp.
 * Does not require GitHub. Fake exe may fail `install` marker (ignored if file present).
 *
 *   npx tsx scripts/smoke-ensure-local.mts
 */
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
  mkdirSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  candidateLiteBaseUrls,
  ensureLite,
  PLUGIN_VERSION,
} from '../src/ensure-lite.ts'

const releaseDir = mkdtempSync(join(tmpdir(), 'webclaw-release-'))
const body = Buffer.from('webclaw-lite-fake-for-ensure-local')
const exeName = 'webclaw-lite-windows-x64.exe'
writeFileSync(join(releaseDir, exeName), body)
const sha = createHash('sha256').update(body).digest('hex')
writeFileSync(join(releaseDir, `${exeName}.sha256`), `${sha}  ${exeName}`)

const server = createServer((req, res) => {
  const name = (req.url ?? '/').replace(/^\//, '')
  const file = join(releaseDir, name)
  if (!existsSync(file)) {
    res.writeHead(404)
    res.end('missing')
    return
  }
  res.writeHead(200)
  res.end(readFileSync(file))
})

await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
const addr = server.address()
if (!addr || typeof addr === 'string') throw new Error('no port')
const base = `http://127.0.0.1:${addr.port}`

const home = mkdtempSync(join(tmpdir(), 'webclaw-ensure-home-'))
mkdirSync(join(home, 'bin'), { recursive: true })

const config = {
  binaryPath: 'webclaw-lite',
  webclawHome: home,
  liteBaseUrl: base,
}

console.log('PLUGIN_VERSION', PLUGIN_VERSION)
console.log('candidates head', candidateLiteBaseUrls(config).slice(0, 3))

try {
  try {
    await ensureLite(config, console)
  } catch (error) {
    console.log(
      'ensure finished with error (install marker may fail on fake bytes):',
      error instanceof Error ? error.message.slice(0, 160) : error,
    )
  }
  const exe = join(home, 'bin', 'webclaw-lite.exe')
  const stamp = join(home, 'bin', '.webclaw-lite-npm-version')
  if (!existsSync(exe)) throw new Error(`missing ${exe}`)
  if (readFileSync(stamp, 'utf8').trim() !== PLUGIN_VERSION) {
    throw new Error('version stamp mismatch')
  }
  console.log('local-smoke ok', { exe, stamp: PLUGIN_VERSION, base })
} finally {
  server.close()
  rmSync(releaseDir, { recursive: true, force: true })
  rmSync(home, { recursive: true, force: true })
}
