/**
 * Bundle the browser Client half to lib/client.js (ESM, peers external).
 */
import { writeFileSync } from 'node:fs'
import { build } from 'esbuild'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

await build({
  absWorkingDir: root,
  entryPoints: [join(root, 'src/client/index.ts')],
  outfile: join(root, 'lib/client.js'),
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: ['es2022'],
  jsx: 'automatic',
  sourcemap: true,
  logLevel: 'info',
  external: [
    'react',
    'react/jsx-runtime',
    'react-dom',
    '@deepseek-ai/cordis',
    '@deepseek-ai/dsh-client-ui-primitives',
    '@deepseek-ai/dsh-client-ui-sidebar-documentpreview',
    '@deepseek-ai/dsh-client-ui-sidebar-documentpreview/client',
    '@deepseek-ai/dsh-client-ui-slots',
    '@deepseek-ai/dsh-client-locale',
    '@deepseek-ai/dsh-client-locale/client',
    '@deepseek-ai/dsh-util-workspace-path',
  ],
})

// Host tsc excludes src/client; ship a small ambient .d.ts next to the bundle.
writeFileSync(
  join(root, 'lib/client.d.ts'),
  `/** Ambient types for the prebundled Client entry (\`lib/client.js\`). */
export declare const name: string
export declare const inject: string[]
export declare function apply(ctx: unknown): void
export declare const WEBCLAW_PREVIEW_ID: string
export declare function installWebclawSurfaces(ctx: unknown): void
`,
)

console.log('webclaw client → lib/client.js + lib/client.d.ts')
