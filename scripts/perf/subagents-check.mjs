// Live subagents against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). The parent
// delegates one task; the child runs a slow command, so it can be listed, tailed, steered and
// stopped while it works; the page's `subagent.*` events are checked along the way.
//   node scripts/perf/subagents-check.mjs [port] [folder]
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'subagents-check-'))
fs.mkdirSync(folder, { recursive: true })

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: the parent delegates, the child runs a long command, then both answer.
const childRequests = []
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
    const messages = request.messages ?? []
    const child = JSON.stringify(messages[0]?.content ?? '').includes('focused subagent')
    const last = messages.at(-1)
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    const end = reason => { res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: reason }] })}\n\n`); res.end('data: [DONE]\n\n') }
    const call = (name, args) => { chunk({ tool_calls: [{ index: 0, id: `call_${Date.now()}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] }); end('tool_calls') }
    if (child) {
      childRequests.push(request)
      if (last?.role === 'user' && childRequests.length === 1 || /^short wait$/.test(String(last?.content))) {
        const seconds = /short/.test(String(last?.content)) ? 4 : 120
        chunk({ role: 'assistant', content: 'Looking into it first.' })
        return call('terminal', { command: `Start-Sleep -Seconds ${seconds}`, timeout: 300 })
      }
      chunk({ role: 'assistant', content: 'Child finished.' })
      return end('stop')
    }
    if (last?.role === 'user') return call('delegate_task', { tasks: [{ goal: /short/.test(String(last.content)) ? 'short wait' : 'wait a while' }] })
    chunk({ role: 'assistant', content: 'Parent done.' })
    end('stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.subagents'])
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
const of = type => events.filter(e => e.type === type)

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })
await rpc('prompt.submit', { session_id: sid, text: 'please delegate' })

check('subagent.start', await until(() => of('subagent.start').length === 1), of('subagent.start')[0]?.payload)
const child = of('subagent.start')[0]?.payload ?? {}
check('subagent.tool (the slow command)', await until(() => of('subagent.tool').length === 1) && of('subagent.tool')[0].payload.tool_name === 'terminal', of('subagent.tool')[0]?.payload)
check('subagent.progress (its text)', of('subagent.progress')[0]?.payload.text === 'Looking into it first.', of('subagent.progress')[0]?.payload)

const listed = await rpc('subagent.list', { session_id: sid })
check('subagent.list while it runs', listed.subagents.length === 1 && listed.subagents[0].subagent_id === child.subagent_id && listed.subagents[0].status === 'running' && listed.subagents[0].last_tool === 'terminal', listed)
const status = await rpc('delegation.status', {})
check('delegation.status', status.active.length === 1 && status.paused === false && status.max_concurrent_children === 10, status)
const tail = await rpc('subagent.tail', { session_id: sid, subagent_id: child.subagent_id })
check('subagent.tail', tail.available && tail.text.includes('Looking into it first.') && tail.text.includes('terminal: Start-Sleep'), tail)
const steered = await rpc('subagent.steer', { session_id: sid, subagent_id: child.subagent_id, text: 'wrap up soon' })
check('subagent.steer (queued for its next tool results)', steered.status === 'queued', steered)
const other = await rpc('subagent.steer', { session_id: sid, subagent_id: 'sa-nope', text: 'x' })
check('steering an unknown child is rejected', other.status === 'rejected', other)

const stopped = await rpc('subagent.interrupt', { session_id: sid, subagent_id: child.subagent_id })
check('subagent.interrupt', stopped.found === true, stopped)
check('subagent.complete: interrupted', await until(() => of('subagent.complete').length === 1) && of('subagent.complete')[0].payload.status === 'interrupted', of('subagent.complete')[0]?.payload)
check('the parent turn goes on and finishes', await until(() => of('message.complete').length >= 1), of('message.complete').at(-1)?.payload?.status)
const after = await rpc('subagent.list', { session_id: sid })
check('the list is empty afterwards', after.subagents.length === 0, after)

// A steer reaches the child after its tool results: its next request carries it.
const before = of('subagent.start').length
const seen = childRequests.length
await rpc('prompt.submit', { session_id: sid, text: 'please delegate short' })
await until(() => of('subagent.tool').length === 2)
const second = of('subagent.start')[before]?.payload ?? {}
const queued = await rpc('subagent.steer', { session_id: sid, subagent_id: second.subagent_id, text: 'wrap up soon' })
check('steer a second child mid-command', queued.status === 'queued', queued)
check('it finishes on its own', await until(() => of('subagent.complete').length === 2) && of('subagent.complete')[1].payload.status === 'completed', of('subagent.complete')[1]?.payload)
const followUp = JSON.stringify(childRequests.slice(seen).at(-1)?.messages ?? [])
check('the steer reached the child after its tool results', followUp.includes('wrap up soon'), followUp.slice(-300))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
