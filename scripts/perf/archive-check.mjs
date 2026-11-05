// Archived sessions against a running hermes-desktop (the perf instance; no model calls).
// Archives a session from the sidebar's PATCH and from the shortcut's session.mutate, checks it
// leaves the sidebar, the pins and the project tree and is listed with archived=only (Settings ▸
// Sessions), restores it, deletes an archived one, and reads the auto-archive defaults.
//   node scripts/perf/archive-check.mjs [port]
const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.archive'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) }
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(new Error(`${method}: ${m.error.message}`)) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})
const list = async archived => (await rest('GET', `/api/profiles/sessions?limit=1000&offset=0&archived=${archived}`)).body.sessions
const listed = async (archived, sid) => (await list(archived)).find(s => s.id === sid)
const inTree = async sid => JSON.stringify(await rpc('projects.tree', { preview_limit: 1000 })).includes(sid)

const stamp = Date.now()
const make = async title => {
  const { session_id: sid } = await rpc('session.create', {})
  await rest('PATCH', `/api/sessions/${sid}`, { title })
  return sid
}
const a = await make(`archive check A ${stamp}`)
const b = await make(`archive check B ${stamp}`)
check('new sessions are listed, not archived', (await listed('exclude', a))?.archived === false && (await listed('exclude', b)) !== undefined)

await rest('PATCH', `/api/sessions/${a}`, { pinned: true })
const archived = await rest('PATCH', `/api/sessions/${a}`, { archived: true, pinned: false })
check('the sidebar archives a session', archived.status === 200, archived.body)
check('it leaves the sidebar list', (await listed('exclude', a)) === undefined)
const only = await listed('only', a)
check('Settings ▸ Sessions lists it (archived=only), unpinned', only?.archived === true && only?.pinned === false, only)
check('archived=include has both', (await listed('include', a)) !== undefined && (await listed('include', b)) !== undefined)
check('archived=only has only archived ones', (await list('only')).every(s => s.archived === true))
check('the project tree leaves it out', !(await inTree(a)))
check('it still opens', (await rest('GET', `/api/sessions/${a}`)).body?.archived === true)

const restored = await rest('PATCH', `/api/sessions/${a}`, { archived: false })
check('Unarchive brings it back', restored.status === 200 && (await listed('exclude', a))?.archived === false && (await inTree(a)))

const mutated = await rpc('session.mutate', { session_id: b, operation: 'archive', payload: { archived: true } })
check('the shortcut (session.mutate archive) archives', mutated.archived === true && (await listed('only', b)) !== undefined, mutated)

const deleted = await rest('DELETE', `/api/sessions/${b}`)
check('an archived session can be deleted', deleted.status === 200 && (await listed('only', b)) === undefined)
check('archiving a missing session is refused', (await rest('PATCH', `/api/sessions/${b}`, { archived: true })).status === 404)

const config = (await rest('GET', '/api/config')).body
check('auto-archive is off by default, 3 days', config?.sessions?.auto_archive === false && config?.sessions?.auto_archive_days === 3, config?.sessions)

await rest('DELETE', `/api/sessions/${a}`)
ws.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
