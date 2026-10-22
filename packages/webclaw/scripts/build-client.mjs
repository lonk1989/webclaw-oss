/**
 * Emit DSH Client factory bundle: window.__ModuleLoader__.load({ id, factory }).
 * Plain ESM imports fail web boot ("import failed") — the shell only answers require().
 */
import { build } from 'esbuild'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'))
const id = pkg.name
const outfile = join(root, 'lib', 'client.cjs.tmp.js')

await build({
  absWorkingDir: root,
  entryPoints: [join(root, 'src/client/index.ts')],
  outfile,
  bundle: true,
  format: 'cjs',
  platform: 'browser',
  target: ['es2022'],
  jsx: 'automatic',
  sourcemap: 'inline',
  logLevel: 'info',
  // Platform / preloaded table rows — answered by the shell require().
  external: [
    'react',
    'react/jsx-runtime',
    'react-dom',
    '@deepseek-ai/cordis',
    '@deepseek-ai/dsh-client-ui-primitives',
    '@deepseek-ai/dsh-client-ui-slots',
    '@deepseek-ai/dsh-client-store',
  ],
})

const cjs = readFileSync(outfile, 'utf8')
const wrapped = `window.__ModuleLoader__.load({
  id: ${JSON.stringify(id)},
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });
${cjs}
    return module.exports;
  }
});
`
writeFileSync(join(root, 'lib', 'client.js'), wrapped)
writeFileSync(
  join(root, 'lib', 'client.js.map'),
  JSON.stringify({ version: 3, file: 'client.js', sources: [], mappings: '' }),
)

try {
  const { unlinkSync } = await import('node:fs')
  unlinkSync(outfile)
} catch {
  /* ignore */
}

console.log(`webclaw client → lib/client.js (ModuleLoader factory, id=${id})`)
