// The memory manager against a running hermes-desktop (the perf instance): seeds MEMORY.md,
// USER.md and a used skill in its data folder, then checks the Star map's routes (graph, node
// detail, edit, delete, a stale id refused) and the memory status.
//   node scripts/perf/memory-check.mjs [port] [data folder]
import fs from 'node:fs'
import path from 'node:path'

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
} finally {
  const restore = (name, data) => (data === null ? fs.rmSync(path.join(memories, name), { force: true }) : fs.writeFileSync(path.join(memories, name), data))
  restore('MEMORY.md', saved.memory)
  restore('USER.md', saved.user)
}

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
