// /branch, /btw and /bg against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
//   node scripts/perf/side-and-branch-check.mjs [port] [folder]
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'side-and-branch-'))
fs.mkdirSync(folder, { recursive: true })
const base = `http://127.0.0.1:${port}`

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: echoes which kind of request it got.
const requests = []
let replyCount = 0
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    if (!request.stream) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    requests.push(request)
    const system = JSON.stringify(request.messages?.[0]?.content ?? '')
    const text = system.includes('SIDE question') ? 'Side answer: we talked about apples.'
      : system.includes('focused subagent') ? 'Background result: 7 files.'
      : `Reply ${++replyCount}.`
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.side'])
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
const until = async (test, ms = 30000) => { const deadline = Date.now() + ms; while (!test() && Date.now() < deadline) await sleep(100); return test() }
const turn = async (sid, text) => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text })
  await until(() => events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete'))
}

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })
const topic = `Fruit talk ${Date.now()}`
await rpc('session.title', { session_id: sid, title: topic })
await turn(sid, 'let us talk about apples')
await turn(sid, 'and now pears')
const parentRows = (await rpc('session.resume', { session_id: sid })).messages
check('parent has 4 rows', parentRows.length === 4, parentRows.map(r => r.role))

// /branch from the first reply: the branch holds the first two messages.
const first = await rpc('session.branch', { session_id: sid, count: 2, through_message_id: parentRows[1].row_id, idempotency_key: 'k1' })
check('session.branch through the first reply', first.message_count === 2 && first.messages.length === 2 && first.parent === sid && first.title === `${topic} (branch)`, { n: first.message_count, title: first.title })
const again = await rpc('session.branch', { session_id: sid, count: 2, through_message_id: parentRows[1].row_id, idempotency_key: 'k1' })
check('a retried request gets the same branch', again.session_id === first.session_id, again.session_id)
const whole = await rpc('session.branch_whole', { session_id: sid })
check('session.branch_whole: all of it, numbered title', whole.message_count === 4 && whole.messages_omitted === true && whole.title === `${topic} (branch 2)`, whole)
const stored = await rpc('session.branch_stored', { parent_session_id: sid })
check('session.branch_stored', stored.message_count === 4 && stored.session_id !== whole.session_id, stored.session_id)
const list = await (await fetch(`${base}/api/profiles/sessions?limit=50`)).json()
const row = list.sessions.find(s => s.id === first.session_id)
check('the branch is listed under its parent', row?.parent_session_id === sid, row)
await rpc('session.resume', { session_id: first.session_id })
await turn(first.session_id, 'what about cherries')
const branchRows = (await rpc('session.resume', { session_id: first.session_id })).messages
const parentAfter = (await rpc('session.resume', { session_id: sid })).messages
check('the branch goes its own way; the parent is unchanged', branchRows.length === 4 && branchRows[2].text === 'what about cherries' && parentAfter.length === 4, branchRows.map(r => r.text))
const seeded = await rpc('session.create', { cwd: folder, parent_session_id: sid, messages: [{ role: 'user', content: 'seed q' }, { role: 'assistant', content: 'seed a' }] })
check('session.create with messages (the page\'s own branch)', seeded.message_count === 2 && seeded.messages[1].text === 'seed a', seeded.messages)

// /btw: answered from a snapshot, conversation untouched.
const before = (await rpc('session.resume', { session_id: sid })).messages.length
const btw = await rpc('prompt.btw', { session_id: sid, text: 'what fruit came first?' })
check('/btw returns a task id', /^btw_/.test(btw.task_id), btw)
await until(() => events.some(e => e.type === 'btw.complete'))
const answer = events.find(e => e.type === 'btw.complete')
check('btw.complete carries the answer and question', answer?.payload.text === 'Side answer: we talked about apples.' && answer.payload.question === 'what fruit came first?' && answer.session_id === sid, answer?.payload)
const sideRequest = requests.find(r => JSON.stringify(r.messages[0].content).includes('SIDE question'))
check('the side question saw the conversation, without tools', JSON.stringify(sideRequest.messages[1].content).includes('USER: let us talk about apples') && !(sideRequest.tools?.length), JSON.stringify(sideRequest.messages[1].content).slice(0, 160))
check('the conversation is untouched', (await rpc('session.resume', { session_id: sid })).messages.length === before)

// /bg: a background subagent; its result comes back as background.complete.
const bg = await rpc('prompt.background', { session_id: sid, text: 'count the files' })
check('/bg returns a task id', /^bg_/.test(bg.task_id), bg)
await until(() => events.some(e => e.type === 'background.complete'))
const done = events.find(e => e.type === 'background.complete')
check('background.complete carries the result', done?.payload.text === 'Background result: 7 files.' && done.payload.task_id === bg.task_id, done?.payload)
check('its subagent row ran', events.some(e => e.type === 'subagent.start' && e.payload.goal === 'count the files') && events.some(e => e.type === 'subagent.complete' && e.payload.goal === 'count the files'))
check('the conversation is still untouched', (await rpc('session.resume', { session_id: sid })).messages.length === before)

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
