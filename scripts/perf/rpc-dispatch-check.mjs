// Every JSON-RPC method the gateway serves, over the real socket of a running hermes-desktop (the
// perf instance; no model calls): none of them answers -32601 for any params; a name the backend
// doesn't serve answers exactly -32601 "Method not found: <name>"; the errors the page reads keep
// their code and text; the methods the other checks don't call (projects, delegation, processes,
// subagents, completion, setup, the Import screen, approvals) answer their shapes. Phase 13 CP13f
// (the typed `Request` / `RpcError` dispatch): the answers were recorded before the change.
//   node scripts/perf/rpc-dispatch-check.mjs [port] [folder]
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'rpc-dispatch-'))
fs.writeFileSync(path.join(folder, 'a.txt'), 'hello\n')
const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.rpc-dispatch'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
// Replies nobody waits for (the envelope checks below expect none).
const stray = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method !== 'event') stray.push(m)
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
// A raw text message, and the reply awaited under `wantId` (a string, a number or null).
const raw = (text, wantId) => new Promise(ok => {
  if (wantId !== undefined) pending.set(wantId, ok)
  ws.send(text)
  if (wantId === undefined) ok()
})
// The whole reply frame: `{result}` or `{error: {code, message, data?}}`.
const call = (method, params) => new Promise(ok => {
  pending.set(++id, ok)
  const frame = { jsonrpc: '2.0', id, method }
  if (params !== undefined) frame.params = params
  ws.send(JSON.stringify(frame))
})
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${typeof detail === 'string' ? detail : JSON.stringify(detail)}`.slice(0, 240)}`)
  if (!ok) failed++
}
const errorOf = (m, code, message) => m.error?.code === code && (message === undefined || m.error?.message === message)

const SERVED = [
  'gateway.ping', 'client.capabilities', 'setup.status', 'onboarding.state', 'profiles.list', 'projects.tree',
  'projects.project_sessions', 'projects.list', 'projects.create', 'projects.update', 'projects.add_folder',
  'projects.delete', 'projects.set_active', 'projects.discover_repos', 'projects.record_repos', 'session.active_list',
  'model.options', 'requests.status', 'staging.list', 'staging.apply', 'staging.reject', 'config.get', 'config.set',
  'pet.info', 'setup.runtime_check', 'shared_metrics.status', 'free_tier.status', 'wake.status', 'commands.catalog',
  'complete.slash', 'slash.exec', 'session.status', 'command.dispatch', 'session.title', 'session.mutate',
  'session.rewind', 'rollback.list', 'rollback.diff', 'rollback.restore', 'session.export', 'session.foreign.list',
  'session.foreign.preview', 'session.foreign.import', 'session.branch', 'session.branch_whole', 'session.branch_stored',
  'prompt.btw', 'prompt.background', 'file.attach', 'image.attach', 'image.attach_bytes', 'image.detach',
  'insights.get', 'session.compress', 'session.delete', 'session.close', 'session.create', 'session.resume',
  'session.history', 'prompt.submit', 'approval.respond', 'clarify.respond', 'session.context_breakdown',
  'approval.pending', 'process.list', 'process.kill', 'process.stop', 'subagent.list', 'subagent.tail',
  'subagent.steer', 'subagent.interrupt', 'delegation.status', 'delegation.pause', 'complete.path', 'session.steer',
  'prompt.cancel', 'prompt.steer_queued', 'prompt.replace_queued', 'session.redirect', 'session.interrupt',
]
check('80 served methods listed', SERVED.length === 80, SERVED.length)

// 0. The envelope (the socket handler, unchanged by CP13f; checked live in its review): the id is
// echoed as sent (string, number, null); a frame without an id, without a string method, a batch
// array or a line that isn't JSON gets no reply; two frames in one message are two requests;
// extra top-level fields and `params: null` change nothing.
check('string id echoed', (await raw('{"jsonrpc":"2.0","id":"s-1","method":"gateway.ping"}', 's-1')).result?.ok === true)
const nullId = await raw('{"jsonrpc":"2.0","id":null,"method":"gateway.ping"}', null)
check('null id echoed', nullId.id === null && nullId.result?.ok === true, nullId)
await raw('{"jsonrpc":"2.0","method":"gateway.ping"}')
await raw('{"jsonrpc":"2.0","id":9001}')
await raw('{"jsonrpc":"2.0","id":9002,"method":5}')
await raw('[{"jsonrpc":"2.0","id":9003,"method":"gateway.ping"}]')
await raw('not json at all')
const both = Promise.all([new Promise(ok => pending.set(9004, ok)), new Promise(ok => pending.set(9005, ok))])
ws.send('{"jsonrpc":"2.0","id":9004,"method":"gateway.ping"}\n{"jsonrpc":"2.0","id":9005,"method":"no.such"}')
const [ping, nope] = await both
check('two frames in one message', ping.result?.ok === true && errorOf(nope, -32601, 'Method not found: no.such'), [ping, nope])
const extra = await raw('{"jsonrpc":"2.0","id":9006,"method":"session.status","params":null,"extra":{"a":1}}', 9006)
check('params null, extra fields', errorOf(extra, -32602, 'session_id is required') && extra.error.data === undefined, extra)
await raw('{"jsonrpc":"2.0","id":9007,"method":"gateway.ping"}', 9007)
await sleep(500)
check('no reply to a frame without an id or a string method, a batch or a non-JSON line', stray.length === 0, stray)

// 1. None of them is "not here", whatever the params (absent, empty, a list, a string, wrong types).
const odd = [undefined, {}, [], 'x', 7, { session_id: 5, text: null, folders: 'x', days: '7' }, { session_id: 'no-such-session' }]
const notHere = []
for (const method of SERVED) {
  for (const params of odd) {
    const m = await call(method, params)
    if (m.error?.code === -32601) notHere.push(`${method} ${JSON.stringify(params)}`)
  }
}
check('no served method answers -32601', notHere.length === 0, notHere.slice(0, 3))

// 2. A name that isn't exactly served is -32601 with the old text (the page probes some of these).
for (const name of ['nope.nothing', 'Gateway.Ping', 'gateway.ping ', ' gateway.ping', '', 'session.save', 'terminal.resize', 'voice.toggle', 'config.show', 'session.list', 'gateway.PING']) {
  const m = await call(name, { session_id: 'x' })
  check(`unknown ${JSON.stringify(name)}`, errorOf(m, -32601, `Method not found: ${name}`) && m.error.data === undefined, m.error)
}

// 3. The constant and setup answers.
check('gateway.ping', JSON.stringify((await call('gateway.ping')).result) === '{"ok":true}')
check('client.capabilities', JSON.stringify((await call('client.capabilities')).result) === '{"server_requests":[]}')
check('onboarding.state', JSON.stringify((await call('onboarding.state', {})).result) === '{"run":false,"eligible":false}')
check('pet.info', (await call('pet.info')).result?.enabled === false)
check('shared_metrics.status', (await call('shared_metrics.status')).result?.decided === true)
check('free_tier.status', (await call('free_tier.status')).result?.available === false)
check('wake.status', (await call('wake.status')).result?.hint === "Voice wake isn't available in hermes-rs.")
check('projects.discover_repos / record_repos', (await call('projects.discover_repos')).result?.repos?.length === 0 && (await call('projects.record_repos')).result?.accepted === true)
const setup = (await call('setup.status')).result
check('setup.status', setup?.ok === true && setup.profile === 'default' && 'ready' in setup, setup)
const runtime = (await call('setup.runtime_check')).result
check('setup.runtime_check', runtime?.profile === 'default' && 'ok' in runtime, runtime)
const profiles = (await call('profiles.list')).result
check('profiles.list', profiles?.profiles?.[0]?.name === 'default' && profiles.profiles[0].is_default === true, profiles)
check('commands.catalog', Array.isArray((await call('commands.catalog')).result?.pairs))
check('complete.slash', Array.isArray((await call('complete.slash', { text: '/pl' })).result?.items))
const completed = (await call('complete.path', { word: '@a', cwd: folder })).result
check('complete.path', completed?.items?.[0]?.text === '@file:a.txt', completed)
check('model.options', Array.isArray((await call('model.options', {})).result?.providers))
const status = (await call('requests.status', {})).result
check('requests.status', status && 'used' in status, status)
check('requests.status unknown provider still answers', (await call('requests.status', { provider: 'nope' })).result?.provider === 'nope')
check('insights.get', typeof (await call('insights.get', { days: 7 })).result?.days === 'number')
check('staging.list', 'items' in ((await call('staging.list')).result ?? {}))

// 4. The errors the page and ACP read.
check('session.status without an id', errorOf(await call('session.status', {}), -32602, 'session_id is required'))
check('session.status with a number', errorOf(await call('session.status', { session_id: 5 }), -32602, 'session_id is required'))
check('session.resume unknown id', errorOf(await call('session.resume', { session_id: 'nope' }), -32000, 'Session not found: nope'))
check('prompt.submit unknown id', errorOf(await call('prompt.submit', { session_id: 'nope', text: 'hi' }), -32602, 'Unknown session nope'))
check('staging.apply without an id', errorOf(await call('staging.apply', {}), -32602, 'id is required'))
check('config.set without a key', errorOf(await call('config.set', { value: 1 }), -32602, 'key is required'))
check('config.set a bad profile', errorOf(await call('config.set', { key: 'budget.profile', value: 'fast' }), -32602))
check('approval.respond without an id', errorOf(await call('approval.respond', {}), -32602, 'prompt_id is required'))
check('approval.respond unknown prompt', errorOf(await call('approval.respond', { prompt_id: 'p', choice: 'once' }), -32000, 'No pending approval request'))
check('clarify.respond unknown prompt', errorOf(await call('clarify.respond', { request_id: 'p', answer: 'a' }), -32000, 'No pending clarify request'))
check('prompt.btw empty text', errorOf(await call('prompt.btw', { session_id: 'x', text: '  ' }), -32602, 'text required'))
check('subagent.steer empty text', errorOf(await call('subagent.steer', { session_id: 'x', subagent_id: 'y', text: '' }), -32602, 'text is required'))
check('image.attach missing file', errorOf(await call('image.attach', { session_id: 'x', path: path.join(folder, 'none.png') }), -32602))
check('session.foreign.preview unknown id', (await call('session.foreign.preview', { id: 'nosuch-session' })).error?.code === -32602)

// 5. A session: create, title, status, busy refusals' shape is covered by the other checks; delete.
const created = (await call('session.create', { cwd: folder })).result
const sid = created?.session_id
check('session.create', Boolean(sid), sid)
check('session.title', (await call('session.title', { session_id: sid, title: ' rpc dispatch ' })).result?.title === 'rpc dispatch')
check('session.status', typeof (await call('session.status', { session_id: sid })).result?.output === 'string')
check('session.context_breakdown', Array.isArray((await call('session.context_breakdown', { session_id: sid })).result?.categories))
check('approval.pending', Array.isArray((await call('approval.pending', { session_id: sid })).result?.approvals))
check('process.list', Array.isArray((await call('process.list', { session_id: sid })).result?.processes))
check('process.kill unknown', (await call('process.kill', { session_id: sid, process_id: 'nope' })).result?.status === 'not_found')
check('process.stop', 'killed' in ((await call('process.stop', { session_id: sid })).result ?? {}))
check('subagent.list', Array.isArray((await call('subagent.list', { session_id: sid })).result?.subagents))
check('subagent.tail / interrupt / steer unknown', (await call('subagent.interrupt', { session_id: sid, subagent_id: 'sa' })).result?.found === false
  && (await call('subagent.steer', { session_id: sid, subagent_id: 'sa', text: 'x' })).result?.status === 'rejected'
  && (await call('subagent.tail', { session_id: sid, subagent_id: 'sa' })).error === undefined)
check('session.steer idle', (await call('session.steer', { session_id: sid, text: 'x' })).result?.status === 'rejected')
check('session.redirect idle', (await call('session.redirect', { session_id: sid, text: 'x' })).result?.status === 'rejected')
check('prompt.steer_queued idle', errorOf(await call('prompt.steer_queued', { session_id: sid, admission_id: 'a' }), -32000, 'session_idle'))
check('prompt.replace_queued unknown', errorOf(await call('prompt.replace_queued', { session_id: sid, admission_id: 'a', text: 't' }), -32000, 'admission_started'))
check('prompt.cancel unknown', (await call('prompt.cancel', { session_id: sid, admission_id: 'a' })).result?.cancelled === false)
check('session.interrupt idle', (await call('session.interrupt', { session_id: sid })).result?.status === 'not_interrupted')
check('image.detach none held', (await call('image.detach', { session_id: sid, path: 'x' })).result?.detached === false)
check('session.active_list', Array.isArray((await call('session.active_list')).result?.sessions))
check('session.close', (await call('session.close', { session_id: sid })).result?.closed === true)

// 6. Delegation switch (left as found).
const before = (await call('delegation.status')).result
check('delegation.status', before && 'paused' in before && before.max_spawn_depth === 1, before)
check('delegation.pause (absent = true)', (await call('delegation.pause', {})).result?.paused === true)
check('delegation.pause "false" is not a boolean', (await call('delegation.pause', { paused: 'false' })).result?.paused === true)
check('delegation.pause false', (await call('delegation.pause', { paused: false })).result?.paused === false)

// 7. A project: create, update, add a folder, activate, list, tree, delete (left as found).
const sub = path.join(folder, 'sub'); fs.mkdirSync(sub, { recursive: true })
const project = (await call('projects.create', { name: '  rpc dispatch  ', folders: [folder, 5], primary_path: sub })).result?.project
check('projects.create', project?.name === 'rpc dispatch' && Boolean(project?.id), project)
const pid = project?.id
check('projects.update', (await call('projects.update', { id: pid, name: ' renamed ' })).result?.project?.id === pid)
check('projects.add_folder', (await call('projects.add_folder', { id: pid, path: sub })).result?.project?.id === pid)
check('projects.update unknown', errorOf(await call('projects.update', { id: 'nope' }), -32000, 'No project nope'))
check('projects.set_active', (await call('projects.set_active', { id: pid })).result?.active_id === pid)
check('projects.list', Array.isArray((await call('projects.list')).result?.projects))
check('projects.tree', Array.isArray((await call('projects.tree', { preview_limit: 1 })).result?.projects))
check('projects.project_sessions', (await call('projects.project_sessions', { project_id: pid })).result?.project?.id === pid)
check('projects.delete', Array.isArray((await call('projects.delete', { id: pid })).result?.projects))
check('projects.set_active null', (await call('projects.set_active', { id: null })).result?.active_id === null)

// 8. The session that was left: delete it.
check('session.delete', (await call('session.delete', { session_id: sid })).result?.deleted === sid)
check('session.delete again answers the same', (await call('session.delete', { session_id: sid })).result?.deleted === sid)
check('session.foreign.list', (await call('session.foreign.list', { source: 'nosuch', offset: 0 })).error === undefined)

console.log(failed ? `${failed} FAILED` : 'all checks passed')
process.exit(failed ? 1 : 0)
