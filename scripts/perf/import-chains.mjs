// Why does a library load before the first paint? Builds Hermes's webapp (into a scratch folder)
// with a plugin that walks the module graph and prints, for each target, the shortest chain of
// STATIC imports from the entry (dynamic `import()` edges are lazy and don't count).
//   node scripts/perf/import-chains.mjs <prepared webapp workspace> <out dir> [pattern ...]
// The workspace is target/hermes-src/.build/webapp-workspace after scripts/build-hermes-webapp.ps1.
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const [workspace, out, ...patterns] = process.argv.slice(2)
const targets = (patterns.length ? patterns : ['@xterm/', '@codemirror/', 'react-simple-icons', '/katex/']).map(
  p => ({ pattern: p, test: id => id.replaceAll('\\', '/').includes(p) })
)
const short = id => id.replaceAll('\\', '/').replace(/^.*?\/(apps\/desktop\/src|node_modules)\//, '$1/')

const chains = {
  name: 'hermes-rs:import-chains',
  buildEnd() {
    const ids = [...this.getModuleIds()]
    // The entry is a small launcher that `import()`s the app at once (index.html → src/main), so
    // its own dynamic imports are startup too.
    const staticClosure = roots => {
      const seen = new Set(roots)
      const queue = [...roots]
      while (queue.length) for (const next of this.getModuleInfo(queue.shift())?.importedIds ?? []) if (!seen.has(next)) { seen.add(next); queue.push(next) }
      return seen
    }
    const launcher = staticClosure(ids.filter(id => this.getModuleInfo(id)?.isEntry))
    const entries = [...new Set([...launcher, ...[...launcher].flatMap(id => this.getModuleInfo(id)?.dynamicallyImportedIds ?? [])])]
    console.log(`launcher: ${launcher.size} modules; app roots: ${entries.filter(e => !launcher.has(e)).map(short).join(', ')}`)
    // BFS over static imports from every entry; dynamic imports start no edge.
    const prev = new Map(entries.map(e => [e, null]))
    const queue = [...entries]
    while (queue.length) {
      const id = queue.shift()
      for (const next of this.getModuleInfo(id)?.importedIds ?? []) {
        if (!prev.has(next)) { prev.set(next, id); queue.push(next) }
      }
    }
    for (const { pattern, test } of targets) {
      const hits = ids.filter(test)
      const reached = hits.filter(id => prev.has(id))
      console.log(`\n=== ${pattern}: ${hits.length} modules, ${reached.length} reached by static imports`)
      if (!reached.length) continue
      // The chain to the first reached module, shown from the entry.
      const chain = []
      for (let id = reached[0]; id; id = prev.get(id)) chain.unshift(short(id))
      for (const [i, step] of chain.entries()) console.log(`${'  '.repeat(Math.min(i, 12))}${step}`)
    }
  }
}

const { build } = await import(pathToFileURL(join(workspace, 'node_modules/vite/dist/node/index.js')).href)
await build({
  root: join(workspace, 'apps/desktop'), configLoader: 'runner', logLevel: 'error',
  plugins: [chains], build: { outDir: out, emptyOutDir: true }
})
