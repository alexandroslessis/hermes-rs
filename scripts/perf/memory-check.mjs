// The memory manager against a running hermes-desktop (the perf instance): seeds MEMORY.md,
// USER.md and a used skill in its data folder, then checks the Star map's routes (graph, node
// detail, edit, delete, a stale id refused) and the memory status.
// Phase 9 (CP9.3), against `memory.db` in the same folder: a card's `skill:` / `memory:` links are
// real edges and dangling ones break nothing, the additive `archive` and `memory_links` fields are
// there, another repository's project memory is never in the map, and the curator's memory pass
// stages a demotion (zero model requests: the perf instance has no consolidation model) that
// Apply carries out through the stores.
//   node scripts/perf/memory-check.mjs [port] [data folder]
import fs from 'node:fs'
import path from 'node:path'
import { DatabaseSync } from 'node:sqlite'

const port = process.argv[2] ?? '47810'
const home = process.argv[3] ?? path.resolve('target/perf/home')
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const rest = async (method, url, body) => {
  const r = await fetch(base + url, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

const memories = path.join(home, 'memories')
fs.mkdirSync(memories, { recursive: true })
const keep = name => { const p = path.join(memories, name); return fs.existsSync(p) ? fs.readFileSync(p) : null }
const saved = { memory: keep('MEMORY.md'), user: keep('USER.md') }
fs.writeFileSync(path.join(memories, 'MEMORY.md'), '# Check build\nRun cargo build before tests.\n§\nThe perf app listens on 47810.')
fs.writeFileSync(path.join(memories, 'USER.md'), 'The user prefers short answers.')
const skillDir = path.join(home, 'skills', 'checks', 'memory-check-skill')
fs.mkdirSync(skillDir, { recursive: true })
fs.writeFileSync(path.join(skillDir, 'SKILL.md'), '---\nname: memory-check-skill\ndescription: A skill for the memory check.\n---\n# Memory check skill\n')
const usagePath = path.join(home, 'skills', '.usage.json')
const usage = fs.existsSync(usagePath) ? JSON.parse(fs.readFileSync(usagePath, 'utf8')) : {}
fs.writeFileSync(usagePath, JSON.stringify({ ...usage, 'memory-check-skill': { uses: 1 } }))

try {
  const graph = await rest('GET', '/api/learning/graph')
  const nodes = graph.body?.nodes ?? []
  const memoryNodes = nodes.filter(n => n.kind === 'memory')
  check('graph: three memory cards, the used skill', graph.status === 200 && memoryNodes.length === 3 && nodes.some(n => n.id === 'memory-check-skill'), nodes.map(n => n.label))
  check('cards carry fingerprints', graph.body.memory.every(c => /^[0-9a-f]{12}$/.test(c.fingerprint)), graph.body.memory.map(c => c.fingerprint))
  // CP9.3: links become edges. The graph call above made the app take the files into memory.db.
  const db = new DatabaseSync(path.join(home, 'memory.db'))
  const now = Math.floor(Date.now() / 1000)
  const insert = db.prepare('INSERT INTO entry(tier, scope, kind, text, tags, links, created, last_confirmed) VALUES (?, ?, ?, ?, ?, ?, ?, ?)')
  db.prepare("UPDATE entry SET links = ? WHERE tier = 'core' AND text LIKE 'The perf app listens%'").run(JSON.stringify(['skill:memory-check-skill', 'skill:gone-skill', 'memory:999999', 'file:C:/x/y.md']))
  insert.run('archive', '', 'fact', 'Archived: the perf instance restarts nightly', '[]', JSON.stringify(['skill:memory-check-skill']), now, now)
  insert.run('project', 'project:c:/some/other/repo', 'fact', 'PROJECT-SECRET-NOTE of another repository', '[]', '[]', now, now)
  try {
    const linked = (await rest('GET', '/api/learning/graph')).body
    const card = linked.nodes.find(n => n.label.includes('47810'))
    check('a card\'s skill link is a real edge', linked.edges.some(e => e.source === card.id && e.target === 'memory-check-skill'), linked.edges.filter(e => e.source === card.id))
    const ids = new Set(linked.nodes.map(n => n.id))
    check('every edge joins two nodes (nothing dangling)', linked.edges.every(e => ids.has(e.source) && ids.has(e.target)))
    const byTarget = t => linked.memory_links?.find(l => l.target === t)
    check('memory_links: resolved / dangling / not looked up', byTarget('skill:memory-check-skill')?.resolved === true && byTarget('skill:gone-skill')?.resolved === false && byTarget('memory:999999')?.resolved === false && byTarget('file:C:/x/y.md')?.resolved === null, linked.memory_links)
    check('the archive is listed (additive)', linked.archive?.some(a => a.text.startsWith('Archived: the perf')), linked.archive)
    check('another repository\'s project memory is not in the map', !JSON.stringify(linked).includes('PROJECT-SECRET-NOTE'))
    check('the old fields are all still there', ['nodes', 'edges', 'clusters', 'memory', 'stats'].every(k => k in linked) && linked.nodes.every(n => n.kind === 'memory' || n.kind === 'skill'))
  } finally {
    db.prepare("DELETE FROM entry WHERE text LIKE 'Archived: the perf%' OR text LIKE 'PROJECT-SECRET-NOTE%'").run()
    db.prepare("UPDATE entry SET links = '[]' WHERE tier = 'core'").run()
    db.close()
  }
  const port = memoryNodes.find(n => n.label.includes('47810'))
  const detail = await rest('GET', `/api/learning/node?id=${encodeURIComponent(port.id)}`)
  check('node detail', detail.body?.content === 'The perf app listens on 47810.', detail.body)
  const edited = await rest('PUT', '/api/learning/node', { id: port.id, content: 'The perf app listens on 47810 (debug build).' })
  check('edit a memory', edited.status === 200 && fs.readFileSync(path.join(memories, 'MEMORY.md'), 'utf8').includes('47810 (debug build)'), edited.body)
  const stale = await rest('PUT', '/api/learning/node', { id: port.id, content: 'again' })
  check('its old id is stale now', stale.status === 400 && /stale/.test(stale.body?.detail), stale.body)
  const user = memoryNodes.find(n => n.memorySource === 'profile')
  check('delete a memory', (await rest('DELETE', '/api/learning/node', { id: user.id })).status === 200 && fs.readFileSync(path.join(memories, 'USER.md'), 'utf8').trim() === '')
  const skill = await rest('GET', '/api/learning/node?id=memory-check-skill')
  check('skill detail', skill.body?.kind === 'skill' && skill.body.content.includes('# Memory check skill'), skill.body?.kind)
  const archived = await rest('DELETE', '/api/learning/node', { id: 'memory-check-skill' })
  check('delete a skill (archived)', archived.status === 200 && !fs.existsSync(skillDir), archived.body)
  const status = await rest('GET', '/api/memory')
  check('memory status', status.status === 200 && status.body.active === '' && typeof status.body.builtin_files.memory === 'number', status.body)
  const limit = await rest('PUT', '/api/config', { config: { memory: { memory_char_limit: 40 } } })
  const tooLong = await rest('PUT', '/api/learning/node', { id: (await rest('GET', '/api/learning/graph')).body.nodes.find(n => n.kind === 'memory').id, content: 'x'.repeat(100) })
  check('Settings limits reach the store', limit.status === 200 && tooLong.status === 400 && /\/40 chars|char limit/.test(tooLong.body?.detail), tooLong.body)
  await rest('PUT', '/api/config', { config: { memory: { memory_char_limit: 2200 } } })

  // CP9.3 hygiene: a core entry nobody has touched for 90 days is proposed for the archive.
  fs.writeFileSync(path.join(memories, 'MEMORY.md'), 'An old entry nobody uses any more.\n§\nA fresh entry confirmed today.')
  await rest('GET', '/api/learning/graph') // takes the file into memory.db
  const hdb = new DatabaseSync(path.join(home, 'memory.db'))
  const longAgo = Math.floor(Date.now() / 1000) - 90 * 86400
  hdb.prepare("UPDATE entry SET created = ?, last_confirmed = ? WHERE tier = 'core' AND text LIKE 'An old entry%'").run(longAgo, longAgo)
  hdb.prepare("DELETE FROM meta WHERE key LIKE 'hygiene:proposed:%'").run()
  hdb.close()
  const staged = async () => (await rest('GET', '/api/staging')).body?.items ?? []
  const before = (await staged()).length
  check('the curator run starts', (await rest('POST', '/api/curator/run')).status === 200)
  let item
  for (let i = 0; i < 40 && !item; i++) {
    await new Promise(r => setTimeout(r, 250))
    item = (await staged()).find(x => x.summary === 'demote memory' && x.diff.includes('An old entry nobody uses'))
  }
  check('a demotion is staged, with a reason and a diff', !!item && /60 days/.test(item.reason) && item.diff.includes('moved to the archive'), item)
  check('nothing changed yet (it is a proposal)', fs.readFileSync(path.join(memories, 'MEMORY.md'), 'utf8').includes('An old entry nobody uses'))
  check('only the stale entry was proposed', (await staged()).length === before + 1)
  const applied = await rest('POST', `/api/staging/${item.id}/apply`)
  check('Apply moves it to the archive', applied.status === 200 && !fs.readFileSync(path.join(memories, 'MEMORY.md'), 'utf8').includes('An old entry nobody uses') && (await rest('GET', '/api/learning/graph')).body.archive.some(a => a.text.startsWith('An old entry nobody uses')), applied.body)
  const cleanup = new DatabaseSync(path.join(home, 'memory.db'))
  cleanup.prepare("DELETE FROM entry WHERE tier = 'archive' AND text LIKE 'An old entry%'").run()
  cleanup.close()
} finally {
  const restore = (name, data) => (data === null ? fs.rmSync(path.join(memories, name), { force: true }) : fs.writeFileSync(path.join(memories, name), data))
  restore('MEMORY.md', saved.memory)
  restore('USER.md', saved.user)
}

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
