// Builds Hermes's webapp with source maps into a scratch folder (for startup-bytes.mjs).
//   node scripts/perf/sourcemap-build.mjs <prepared webapp workspace> <out dir>
import { pathToFileURL } from 'node:url'
import { join } from 'node:path'
const ws = process.argv[2], out = process.argv[3]
const vite = join(ws, 'node_modules/vite/dist/node/index.js')
const { build } = await import(pathToFileURL(vite).href)
await build({ root: join(ws, 'apps/desktop'), configLoader: 'runner', logLevel: 'warn',
  build: { outDir: out, emptyOutDir: true, sourcemap: true, manifest: 'renderer-manifest.json' } })
