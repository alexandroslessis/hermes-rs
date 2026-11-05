// The coding commands against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
// /plan, /init and /learn answer a `send` dispatch with their prompt; /review on an empty session
// says so; after a message, /review starts a reviewer subagent on the conversation (no model call
// first) and the parent presents its verdict.   node scripts/perf/coding-commands-check.mjs [port] [folder]
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'coding-commands-'))
fs.mkdirSync(folder, { recursive: true })
fs.writeFileSync(path.join(folder, 'AGENTS.md'), '# My project\nKeep this line.\n')

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: the reviewer child gives a verdict; the parent answers plainly.
const requests = []
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
    const child = JSON.stringify(request.messages?.[0]?.content ?? '').includes('focused subagent')
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const text = child ? 'Verdict: the parser change looks correct.' : 'Noted.'
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.coding'])
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
const turn = async text => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text })
  await until(() => events.slice(from).some(e => e.type === 'message.complete'))
}

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })

const plan = await rpc('slash.exec', { session_id: sid, command: 'plan add a cache' })
check('/plan sends its prompt', plan.type === 'send' && plan.display === '/plan add a cache' && plan.message.startsWith('[/plan — plan mode]') && plan.message.includes('Task to plan:\nadd a cache'), plan.display)
const init = await rpc('slash.exec', { session_id: sid, command: 'init keep it short' })
check('/init updates the folder\'s AGENTS.md', init.type === 'send' && init.message.includes('MERGE DISCIPLINE') && init.message.includes('Keep this line.') && init.message.includes(folder.replace(/\\/g, '/').split('/').pop()), init.display)
const learn = await rpc('command.dispatch', { session_id: sid, name: 'learn', arg: 'our release steps' })
check('/learn (command.dispatch)', learn.type === 'send' && learn.message.includes('THE REQUEST:\nour release steps'), learn.display)
const empty = await rpc('slash.exec', { session_id: sid, command: 'review' })
check('/review with nothing to review', empty.type === 'exec' && /Nothing to review/.test(empty.output), empty)

// A /plan prompt sent as the page would, then read back from history as the command.
await turn(plan.message)
const history = await rpc('session.resume', { session_id: sid })
const shown = JSON.stringify(history.messages ?? history)
check('the stored /plan reads back as the command', shown.includes('/plan add a cache') && !shown.includes('PLAN MODE'), shown.slice(0, 200))

await turn('I fixed the parser in lexer.rs')
const review = await rpc('slash.exec', { session_id: sid, command: 'review check edge cases' })
check('/review sends itself', review.type === 'send' && review.message === '/review check edge cases', review)
const before = requests.length
const from = events.length
await turn(review.message)
const started = events.slice(from).find(e => e.type === 'subagent.start')
check('a reviewer subagent starts', started?.payload.goal === 'Review: check edge cases', started?.payload)
const reviewer = requests[before]
const briefing = JSON.stringify(reviewer?.messages ?? [])
check('first call is the reviewer, briefed on the conversation', JSON.stringify(reviewer?.messages?.[0]?.content ?? '').includes('focused subagent') && briefing.includes('I fixed the parser in lexer.rs') && briefing.includes('check edge cases'), briefing.slice(0, 200))
check('then the parent presents the verdict', requests.length === before + 2 && JSON.stringify(requests[before + 1].messages).includes('Verdict: the parser change looks correct.'))
check('the review completes', events.slice(from).some(e => e.type === 'subagent.complete' && e.payload.status === 'completed'))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
