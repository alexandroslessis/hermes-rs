// Checkpoint round trip against a running hermes-desktop: the agent (mock_llm.py) writes a file
// in an empty folder; the checkpoint diff must show it, and restoring must remove it.
//   node scripts/perf/rollback-check.mjs <port> <empty folder>
import fs from 'node:fs'
import path from 'node:path'

const [port, folder] = process.argv.slice(2)
fs.mkdirSync(folder, { recursive: true })
// A folder with something in it (an empty one has nothing to snapshot).
fs.writeFileSync(path.join(folder, 'seed.txt'), 'already here\n')
const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.check'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method === 'event') events.push(m.params)
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(new Error(`${method}: ${m.error.message}`)) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})
const sleep = ms => new Promise(r => setTimeout(r, ms))

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })
const from = events.length
await rpc('prompt.submit', { session_id: sid, text: 'please write file made.txt' })
while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete')) await sleep(100)

const file = path.join(folder, 'made.txt')
console.log('written:', fs.existsSync(file))
const list = await rpc('rollback.list', { session_id: sid })
console.log('checkpoints:', list.checkpoints.map(c => `${c.hash.slice(0, 8)} ${c.message}`))
const diff = await rpc('rollback.diff', { session_id: sid, hash: '1' })
console.log('diff stat:', JSON.stringify(diff.stat), 'diff bytes:', diff.diff.length)
const restored = await rpc('rollback.restore', { session_id: sid, hash: '1' })
console.log('restore:', JSON.stringify(restored))
console.log('still there after restore:', fs.existsSync(file))
ws.close()
