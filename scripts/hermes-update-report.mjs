// What a Hermes update means for hermes-rs: a report, nothing changed.
//
//   tools\node\node.exe scripts\hermes-update-report.mjs [--from <commit>] [--to <commit>] [--hermes ..\hermes-agent]
//                                                       [--out target\hermes-update-report.md]
//
// From the pinned commit (HERMES_COMMIT) to --to (default: the Hermes checkout's HEAD):
//   1. Ported code to review: every changed Hermes file that hermes-rs cites (a `Hermes:` line or
//      any `path/file.py` / `apps/.../*.ts` mention in crates/**/*.rs), with the citing modules.
//   2. Gateway protocol: JSON-RPC methods added, removed or changed in
//      apps/shared/src/gateway-contract.openrpc.json (what the page calls the backend with).
//   3. REST routes added or removed (hermes_cli/web_routers/*.py).
//   4. Frontend patches (crates/desktop/patches/*.patch) that no longer apply.
//   5. Prompt / tool-schema / skill sources whose files changed (re-run their extractors).
// Then: update HERMES_COMMIT, rebuild the webapp, run the app with the wire log and read the
// `hermes_desktop::missing` lines (PORTING.md, "Updating Hermes").

import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = {}
for (let i = 2; i < process.argv.length; i++) {
  const flag = process.argv[i]
  const value = process.argv[i + 1]
  if (!flag.startsWith('--') || value === undefined || value.startsWith('--')) {
    console.error(`expected --name value pairs, got "${flag}"`)
    process.exit(2)
  }
  args[flag.slice(2)] = value
  i++
}
const hermes = resolve(root, args.hermes ?? '../hermes-agent')
const git = (...a) => execFileSync('git', ['-C', hermes, ...a], { encoding: 'utf8', maxBuffer: 256 << 20 })
const commit = name => {
  try {
    return git('rev-parse', '--verify', '--quiet', `${name}^{commit}`).trim()
  } catch {
    console.error(`${name} isn't in ${hermes}: run \`git -C ${hermes} fetch --unshallow\` (or --deepen=N) first`)
    process.exit(1)
  }
}
const from = commit(args.from ?? readFileSync(join(root, 'HERMES_COMMIT'), 'utf8').trim())
const to = commit(args.to ?? 'HEAD')
const show = (commit, path) => {
  try {
    return git('show', `${commit}:${path}`)
  } catch {
    return null
  }
}

// ── Hermes files hermes-rs cites ─────────────────────────────────────────────────────────────
const CITE = /\b((?:apps\/(?:desktop|shared)\/src|tui_gateway|hermes_cli|agent|gateway|tools|plugins)\/[\w./-]+\.(?:py|tsx?))\b/g
function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) {
      if (name !== 'target' && name !== 'node_modules') walk(path, out)
    } else if (name.endsWith('.rs')) {
      out.push(path)
    }
  }
  return out
}
const citedBy = new Map() // hermes path -> Set of hermes-rs files
for (const file of walk(join(root, 'crates'))) {
  for (const [, path] of readFileSync(file, 'utf8').matchAll(CITE)) {
    if (!citedBy.has(path)) citedBy.set(path, new Set())
    citedBy.get(path).add(relative(root, file).replaceAll('\\', '/'))
  }
}

const changed = git('diff', '--name-status', from, to)
  .trim()
  .split('\n')
  .filter(Boolean)
  .map(line => line.split('\t'))
  .map(([status, ...paths]) => ({ status, path: paths.at(-1), old: paths[0] }))

const lines = [`# Hermes update report`, '', `From \`${from.slice(0, 10)}\` to \`${to.slice(0, 10)}\`: ${changed.length} files changed.`, '']

// 1. Ported code.
const ported = changed.filter(c => citedBy.has(c.old) || citedBy.has(c.path))
lines.push(`## 1. Ported code to review (${ported.length})`, '')
lines.push('Each changed Hermes file hermes-rs ports, and where. Read the Hermes diff, then port it.', '')
for (const c of ported) {
  const users = [...(citedBy.get(c.old) ?? citedBy.get(c.path))].join(', ')
  const stat = git('diff', '--shortstat', from, to, '--', c.old, c.path).trim()
  lines.push(`- \`${c.path}\` (${c.status}; ${stat || 'renamed'}) → ${users}`)
  lines.push(`  \`git -C ${relative(root, hermes) || '.'} diff ${from.slice(0, 8)} ${to.slice(0, 8)} -- ${c.path}\``)
}
lines.push('')

// 2. Gateway protocol.
const CONTRACT = 'apps/shared/src/gateway-contract.openrpc.json'
const methods = commit => {
  const text = show(commit, CONTRACT)
  if (!text) return new Map()
  return new Map(JSON.parse(text).methods.map(m => [m.name, JSON.stringify({ params: m.params, result: m.result })]))
}
const before = methods(from)
const after = methods(to)
const added = [...after.keys()].filter(m => !before.has(m))
const removed = [...before.keys()].filter(m => !after.has(m))
const altered = [...after.keys()].filter(m => before.has(m) && before.get(m) !== after.get(m))
const served = new Set()
for (const file of walk(join(root, 'crates', 'desktop'))) {
  for (const [, name] of readFileSync(file, 'utf8').matchAll(/"([a-z_]+(?:\.[a-z_]+)+)"\s*(?:\||=>|if)/g)) served.add(name)
}
lines.push('## 2. Gateway protocol (`gateway-contract.openrpc.json`)', '')
lines.push(`- Added (${added.length}): ${added.map(m => `\`${m}\``).join(', ') || 'none'}`)
lines.push(`- Removed (${removed.length}): ${removed.map(m => `\`${m}${served.has(m) ? ' (served here)' : ''}\``).join(', ') || 'none'}`)
const alteredServed = altered.filter(m => served.has(m))
lines.push(`- Changed and served by hermes-desktop (${alteredServed.length}): ${alteredServed.map(m => `\`${m}\``).join(', ') || 'none'}`)
lines.push(`- Changed, not served (${altered.length - alteredServed.length}): only matters if the page starts depending on them`)
lines.push('')

// 3. REST routes.
const ROUTE = /@(?:router|config_router|app)\.(get|post|put|patch|delete)\(\s*"([^"]+)"/g
const routes = commit => {
  const set = new Set()
  const listing = git('ls-tree', '-r', '--name-only', commit, 'hermes_cli/web_routers').trim().split('\n')
  for (const path of listing.filter(p => p.endsWith('.py'))) {
    for (const [, verb, route] of (show(commit, path) ?? '').matchAll(ROUTE)) set.add(`${verb.toUpperCase()} ${route}`)
  }
  return set
}
const routesBefore = routes(from)
const routesAfter = routes(to)
lines.push('## 3. REST routes (`hermes_cli/web_routers`)', '')
lines.push(`- Added: ${[...routesAfter].filter(r => !routesBefore.has(r)).map(r => `\`${r}\``).join(', ') || 'none'}`)
lines.push(`- Removed: ${[...routesBefore].filter(r => !routesAfter.has(r)).map(r => `\`${r}\``).join(', ') || 'none'}`)
lines.push('')

// 4. Frontend patches.
const frontend = changed.filter(c => /^apps\/(desktop|shared)\//.test(c.path))
lines.push('## 4. Frontend (`apps/desktop`, `apps/shared`) and its patches', '')
lines.push(`${frontend.length} frontend files changed; the webapp is rebuilt from the new commit as is.`, '')
const patchDir = join(root, 'crates', 'desktop', 'patches')
for (const name of readdirSync(patchDir).filter(n => n.endsWith('.patch')).sort()) {
  const patch = readFileSync(join(patchDir, name), 'utf8')
  const scratch = mkdtempSync(join(tmpdir(), 'hermes-patch-'))
  try {
    for (const [, path] of patch.matchAll(/^\+\+\+ b\/(\S+)/gm)) {
      const text = show(to, path)
      if (text !== null) {
        mkdirSync(dirname(join(scratch, path)), { recursive: true })
        writeFileSync(join(scratch, path), text)
      }
    }
    execFileSync('git', ['apply', '--check', join(patchDir, name)], { cwd: scratch, stdio: 'pipe' })
    lines.push(`- ${name}: applies`)
  } catch {
    lines.push(`- ${name}: **no longer applies**: redo the change on the new file, then regenerate the patch`)
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
}
lines.push('')

// 5. Extracted sources.
const extractor = readFileSync(join(root, 'scripts', 'extract_hermes_prompts.py'), 'utf8')
const promptFiles = new Set([...extractor.matchAll(/"((?:agent|tools|hermes_cli|tui_gateway)\/[\w./-]+\.py)"/g)].map(m => m[1]))
const promptsChanged = changed.filter(c => promptFiles.has(c.path))
const skillsChanged = changed.filter(c => c.path.startsWith('skills/'))
lines.push('## 5. Extracted sources', '')
lines.push(`- Prompt / tool-schema files changed (${promptsChanged.length}): ${promptsChanged.map(c => `\`${c.path}\``).join(', ') || 'none'}`)
lines.push(`  ${promptsChanged.length ? 're-run `scripts/extract_hermes_prompts.py` and review the diff of prompts/hermes/' : ''}`)
lines.push(`- Bundled skills changed: ${skillsChanged.length} file(s)${skillsChanged.length ? ' (refresh skills/ per skills/SOURCE.md)' : ''}`)
lines.push('')

const out = resolve(root, args.out ?? 'target/hermes-update-report.md')
mkdirSync(dirname(out), { recursive: true })
writeFileSync(out, lines.join('\n'))
console.log(lines.join('\n'))
console.log(`\n(written to ${relative(root, out)})`)
