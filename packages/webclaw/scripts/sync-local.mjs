/**
 * Hot-copy built @lonk1989/webclaw into a DSH app's node_modules (no npm publish).
 *
 * Default dest (fengxing二次改造):
 *   D:/tools/harness-apps/fengxing/bundle/node_modules/@lonk1989/webclaw
 *
 * Override:
 *   $env:WEBCLAW_SYNC_DEST = "D:/path/to/.../node_modules/@lonk1989/webclaw"
 *   npm run sync:local
 */
import {
  cpSync,
  existsSync,
  mkdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const DEFAULT_DEST =
  'D:/tools/harness-apps/fengxing/bundle/node_modules/@lonk1989/webclaw'

const dest = (process.env.WEBCLAW_SYNC_DEST || DEFAULT_DEST).trim()
if (!dest) {
  console.error('sync-local: WEBCLAW_SYNC_DEST is empty')
  process.exit(1)
}

const lib = join(root, 'lib')
if (!existsSync(join(lib, 'index.js'))) {
  console.error('sync-local: lib/index.js missing — run npm run build first')
  process.exit(1)
}

mkdirSync(dirname(dest), { recursive: true })
rmSync(dest, { recursive: true, force: true })
mkdirSync(dest, { recursive: true })

const copyFile = (name) => {
  const from = join(root, name)
  if (!existsSync(from)) return
  cpSync(from, join(dest, name), { recursive: true })
}

cpSync(lib, join(dest, 'lib'), { recursive: true })
copyFile('package.json')
copyFile('cordis.patch.yml')
copyFile('README.md')
copyFile('assets')

// Marker so you can tell this install is a local overlay
writeFileSync(
  join(dest, '.webclaw-sync-local'),
  `syncedAt=${new Date().toISOString()}\nsource=${root.replace(/\\/g, '/')}\n`,
  'utf8',
)

console.log(`sync-local: ${root} → ${dest}`)
console.log('sync-local: restart DSH to pick up changes')
