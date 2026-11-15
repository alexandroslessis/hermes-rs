// Queued prompts, steering, redirect, approval and clarify cards against a running hermes-desktop
// (the perf instance, provider http://127.0.0.1:47900/v1): this script is that provider (stop
// mock_llm.py first). A reply it holds back keeps a turn running while the queue panel is used
// (queue, cancel, replace, steer), then the queued prompts must run in order with the page's turn
// live throughout; a redirect stops a turn and runs first; a flagged command's approval card and
// a clarify card are answered (Phase 13 CP13d: this state is the session actor's).
//   node scripts/perf/queue-check.mjs [port] [folder]
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'queue-check-'))
fs.mkdirSync(folder, { recursive: true })

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

const textOf = content => typeof content === 'string' ? content
  : Array.isArray(content) ? content.map(part => part.text ?? '').join(' ') : ''
const lastUser = request => textOf([...request.messages].reverse().find(m => m.role === 'user')?.content)
const marker = text => (text.match(/QA-[a-z0-9-]+/) ?? [''])[0]

// The provider. `held` keeps a "slow" reply open until `release()`.
const turns = [] // the marker of each main request's last user message, in order
let release = () => {}
const sse = (res, delta, finish) => {
  res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
  res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: finish }] })}\n\n`)
  res.end('data: [DONE]\n\n')
}
const toolCall = (res, name, args) => sse(res, {
  role: 'assistant',
  tool_calls: [{ index: 0, id: `call_${Date.now()}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }],
}, 'tool_calls')
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    if (!request.stream || !request.tools?.length) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    // A stopped turn closes its request: a late write to it is dropped.
    res.on('error', () => {})
    const last =request.messages[request.messages.length - 1]
    if (last.role === 'tool') {
      return sse(res, { role: 'assistant', content: `After the tool: ${textOf(last.content).slice(0, 200)}` }, 'stop')
    }
    const text = lastUser(request)
    turns.push(marker(text))
    if (text.includes('QA-slow')) {
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Thinking' }, finish_reason: null }] })}\n\n`)
      release = () => { if (!res.writableEnded && !res.destroyed) sse(res, { content: ' ... done.' }, 'stop') }
      return
    }
    if (text.includes('QA-approve')) {
      return toolCall(res, 'terminal', { command: `Remove-Item -Recurse -Force "${path.join(folder, 'nothing-here')}"` })
    }
    if (text.includes('QA-ask')) {
      return toolCall(res, 'clarify', { questions: [{ question: 'Which colour?', choices: ['red', 'blue'] }] })
    }
    sse(res, { role: 'assistant', content: `Reply to ${marker(text)}.` }, 'stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.queue'])
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
const until = async (test, ms = 30000) => { const deadline = Date.now() + ms; while (!test() && Date.now() < deadline) await sleep(50); return test() }
const mine = (sid, type, from = 0) => events.slice(from).filter(e => e.session_id === sid && e.type === type)
const lastInfo = sid => mine(sid, 'session.info').at(-1)?.payload

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })
const approvalsBefore = (await rpc('config.get', { key: 'approvals.mode' })).value
await rpc('config.set', { key: 'approvals.mode', value: 'manual' })

// 1. A held turn; three prompts queued behind it.
const start = events.length
const first = await rpc('prompt.submit', { session_id: sid, text: 'QA-slow first', submission_id: 'adm-slow' })
check('the first prompt starts', first.status === 'started' && first.admission_id === 'adm-slow', first)
await until(() => turns.length === 1)
const q = []
for (const name of ['one', 'two', 'three']) {
  q.push(await rpc('prompt.submit', { session_id: sid, text: `QA-${name}`, submission_id: `adm-${name}` }))
}
check('prompts sent while it runs are queued', q.every(r => r.status === 'queued') && q.map(r => r.admission_id).join() === 'adm-one,adm-two,adm-three', q)
await until(() => lastInfo(sid)?.pending?.length === 3)
check('the queue panel lists them in order', lastInfo(sid)?.pending?.map(p => p.admission_id).join() === 'adm-one,adm-two,adm-three', lastInfo(sid)?.pending)

// 2. The queue panel: cancel one, edit one (it goes to the back), steer the running turn.
const cancelled = await rpc('prompt.cancel', { session_id: sid, admission_id: 'adm-two' })
check('prompt.cancel takes it out', cancelled.cancelled === true, cancelled)
const again = await rpc('prompt.cancel', { session_id: sid, admission_id: 'adm-two' })
check('a second cancel finds nothing', again.cancelled === false, again)
const replaced = await rpc('prompt.replace_queued', { session_id: sid, admission_id: 'adm-one', text: 'QA-one-edited', submission_id: 'adm-one-edited' })
check('prompt.replace_queued requeues the edit at the back', replaced.status === 'queued' && replaced.admission_id === 'adm-one-edited' && replaced.replaced === 'adm-one', replaced)
await until(() => lastInfo(sid)?.pending?.map(p => p.admission_id).join() === 'adm-three,adm-one-edited')
check('the panel shows the new order', lastInfo(sid)?.pending?.map(p => p.admission_id).join() === 'adm-three,adm-one-edited', lastInfo(sid)?.pending)
const steer = await rpc('session.steer', { session_id: sid, text: 'QA-steer' })
check('session.steer is taken by the running turn', steer.status === 'queued', steer)
const resumed = await rpc('session.resume', { session_id: sid })
check('session.resume shows the turn running and the queue', resumed.running === true && resumed.pending_submissions.map(p => p.admission_id).join() === 'adm-three,adm-one-edited', { running: resumed.running, pending: resumed.pending_submissions })

// 3. Let the held turn finish: the missed steer runs first, then the queue, front to back, and
//    the page's turn stays live until the last one ends.
release()
const ok = await until(() => mine(sid, 'message.complete', start).length >= 4, 60000)
check('four turns completed', ok, mine(sid, 'message.complete', start).length)
check('they ran in order: the turn, the missed steer, then the queue', turns.slice(0, 4).join() === 'QA-slow,QA-steer,QA-three,QA-one-edited', turns)
await until(() => lastInfo(sid)?.running === false)
const window = events.slice(start).filter(e => e.session_id === sid)
const lastComplete = window.findLastIndex(e => e.type === 'message.complete')
const firstIdle = window.findIndex(e => e.type === 'session.info' && e.payload.running === false)
check('running: false only after the last queued turn', firstIdle > lastComplete, { firstIdle, lastComplete })
check('nothing is left queued', (lastInfo(sid)?.pending ?? []).length === 0, lastInfo(sid)?.pending)
check('every turn ended complete', mine(sid, 'message.complete', start).every(e => e.payload.status === 'complete'), mine(sid, 'message.complete', start).map(e => e.payload.status))

// 4. Redirect: stop the running turn and run this text first.
const beforeRedirect = events.length
await rpc('prompt.submit', { session_id: sid, text: 'QA-slow second', submission_id: 'adm-slow-2' })
await until(() => turns.length === 5)
const redirected = await rpc('session.redirect', { session_id: sid, text: 'QA-redirect' })
check('session.redirect is accepted', redirected.status === 'redirected', redirected)
await until(() => mine(sid, 'message.complete', beforeRedirect).length >= 2, 30000)
const done = mine(sid, 'message.complete', beforeRedirect).map(e => e.payload.status)
check('the held turn is interrupted, then the redirect runs', done[0] === 'interrupted' && done[1] === 'complete' && turns[5] === 'QA-redirect', { done, turns })
release()
await until(() => lastInfo(sid)?.running === false)

// 5. An approval card (a flagged command under manual approvals), answered "deny".
const beforeApproval = events.length
await rpc('prompt.submit', { session_id: sid, text: 'QA-approve run it' })
check('the approval card is shown', await until(() => mine(sid, 'approval.request', beforeApproval).length === 1), mine(sid, 'approval.request', beforeApproval))
const card = mine(sid, 'approval.request', beforeApproval)[0]?.payload
const pendingCards = await rpc('approval.pending', { session_id: sid })
check('approval.pending lists it', pendingCards.approvals.some(a => a.request_id === card?.prompt_id), pendingCards)
const answered = await rpc('approval.respond', { session_id: sid, prompt_id: card?.prompt_id, choice: 'deny' })
check('approval.respond resolves it', answered.resolved === 1, answered)
check('the card settles', await until(() => mine(sid, 'approval.settled', beforeApproval).length === 1))
check('the turn completes after the denied command', await until(() => mine(sid, 'message.complete', beforeApproval).some(e => e.payload.status === 'complete')))
check('the folder is untouched', fs.existsSync(folder))
await until(() => lastInfo(sid)?.running === false)

// 6. A clarify card, answered.
const beforeClarify = events.length
await rpc('prompt.submit', { session_id: sid, text: 'QA-ask me' })
check('the clarify card is shown', await until(() => mine(sid, 'clarify.request', beforeClarify).length === 1), mine(sid, 'clarify.request', beforeClarify))
const question = mine(sid, 'clarify.request', beforeClarify)[0]?.payload
const reply = await rpc('clarify.respond', { session_id: sid, prompt_id: question?.prompt_id, answer: 'blue' })
check('clarify.respond resolves it', reply.resolved === 1, reply)
check('the turn completes with the answer', await until(() => mine(sid, 'message.complete', beforeClarify).some(e => e.payload.status === 'complete' && e.payload.text.includes('blue'))), mine(sid, 'message.complete', beforeClarify).map(e => e.payload.text))
await until(() => lastInfo(sid)?.running === false)

// 7. An idle session: queue-panel calls answer as before.
const idle = await rpc('prompt.steer_queued', { session_id: sid, admission_id: 'nothing' }).catch(e => e.message)
check('prompt.steer_queued on an idle session says so', String(idle).includes('session_idle'), idle)
const idleSteer = await rpc('session.steer', { session_id: sid, text: 'late' })
check('session.steer with no turn is rejected', idleSteer.status === 'rejected', idleSteer)

await rpc('config.set', { key: 'approvals.mode', value: approvalsBefore || 'manual' })
ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
