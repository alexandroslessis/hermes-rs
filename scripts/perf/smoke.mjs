// A protocol smoke test against a running hermes-desktop (the perf instance, port 47810, with
// mock_llm.py): JSON-RPC over /api/ws and REST, as the page uses them. Prints each step's result
// and exits non-zero on the first failed check.
//   node scripts/perf/smoke.mjs [port] [working folder]
const port = process.argv[2] ?? '47810'
const cwd = process.argv[3] ?? process.cwd()
const base = `http://127.0.0.1:${port}`

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.smoke'])
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
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${typeof detail === 'string' ? detail : JSON.stringify(detail)}`.slice(0, 300)}`)
  if (!ok) failed++
}
// Waits for the session's turn to end (`message.complete`).
const turn = async (sid, text) => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text })
  for (let i = 0; i < 300; i++) {
    if (events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete')) return
    await sleep(100)
  }
  throw new Error(`no message.complete for "${text}"`)
}

const created = await rpc('session.create', { cwd })
const sid = created.session_id
check('session.create', Boolean(sid), sid)
await rpc('session.resume', { session_id: sid })

await turn(sid, 'please write file smoke-note.txt')
await turn(sid, 'hello there')
await sleep(500)

const list = await rpc('rollback.list', { session_id: sid })
check('rollback.list has a checkpoint', list.enabled && list.checkpoints.length > 0, list.checkpoints?.[0])
const diff = await rpc('rollback.diff', { session_id: sid, hash: '1' })
check('rollback.diff', typeof diff.diff === 'string', diff.stat)

for (const command of ['status', 'context', 'usage', 'diff', 'tools', 'memory', 'insights']) {
  const r = await rpc('slash.exec', { session_id: sid, command })
  check(`/${command}`, typeof r.output === 'string' && r.output.length > 0, r.output.split('\n')[0])
}
const status = await rpc('session.status', { session_id: sid })
check('session.status', status.output.includes('Model: mock'), status.output.split('\n')[1])
const insights = await rpc('insights.get', { days: 7 })
check('insights.get', insights.sessions >= 1 && insights.messages >= 1, insights)

for (const format of ['json', 'md', 'html']) {
  const file = await rpc('session.export', { session_id: sid, format })
  check(`session.export ${format}`, file.content.includes('hello there') && file.filename.endsWith(`.${format}`), file.filename)
}

const replay = await rpc('session.rewind', { session_id: sid, derived_target: true, replay_only: true })
check('session.rewind replay_only', replay.status === 'nothing', replay)
const snapshot = await rpc('session.resume', { session_id: sid })
const lastUser = [...snapshot.messages].reverse().find(m => m.role === 'user')
const rewound = await rpc('session.rewind', { session_id: sid, target_message_id: lastUser.row_id, derived_target: true })
check('session.rewind (/undo)', rewound.status === 'applied' && rewound.target_message.content === 'hello there', rewound)

const restored = await rpc('rollback.restore', { session_id: sid, hash: '1' })
check('rollback.restore', restored.success === true, restored)

await turn(sid, 'another message to compress')
await turn(sid, 'and one more')
const compressed = await rpc('session.compress', { session_id: sid })
check('session.compress', ['compressed', 'noop'].includes(compressed.status) && Array.isArray(compressed.messages), compressed.summary)

const curator = await rest('GET', '/api/curator')
check('GET /api/curator', curator.status === 200 && curator.body.enabled === true, curator.body)
const paused = await rest('PUT', '/api/curator/paused', { paused: true })
check('PUT /api/curator/paused', paused.body?.paused === true)
await rest('PUT', '/api/curator/paused', { paused: false })
const run = await rest('POST', '/api/curator/run', {})
check('POST /api/curator/run', run.body?.name === 'curator-run', run.body)
await sleep(1500)
const action = await rest('GET', '/api/actions/curator-run/status?lines=20')
check('GET /api/actions/curator-run/status', action.status === 200 && 'running' in action.body, action.body)
const memory = await rest('GET', '/api/memory')
check('GET /api/memory', memory.body?.builtin_files !== undefined, memory.body)
const search = await rest('GET', '/api/sessions/search?q=another')
check('GET /api/sessions/search', search.body?.results?.some(r => r.session_id === sid), search.body?.results?.[0])
const logs = await rest('GET', '/api/logs?lines=5')
check('GET /api/logs', logs.body?.lines?.length > 0, logs.body?.file)

ws.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
