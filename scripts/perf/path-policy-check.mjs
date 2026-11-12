// The file tools' path policy against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). One turn where
// the mock model tries write_file into the app's data folder (HERMES_RS_HOME = target/perf/home),
// its skills folder and its memories folder; each must come back refused, naming the tool to use
// instead, and nothing may be written.   node scripts/perf/path-policy-check.mjs [port]
import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const port = process.argv[2] ?? '47810'
const home = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../target/perf/home')
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

const stamp = Date.now()
const targets = {
  data: path.join(home, `path-policy-${stamp}.json`),
  skill: path.join(home, 'skills', `path-policy-${stamp}`, 'SKILL.md'),
  memory: path.join(home, 'memories', `path-policy-${stamp}.md`),
}

// The provider: the first call of a turn asks for the three writes; the next one answers.
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
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    const end = reason => { res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: reason }] })}\n\n`); res.end('data: [DONE]\n\n') }
    if (request.messages.at(-1)?.role === 'user') {
      chunk({
        tool_calls: Object.values(targets).map((file, index) => ({
          index, id: `call_${stamp}_${index}`, type: 'function',
          function: { name: 'write_file', arguments: JSON.stringify({ path: file, content: 'x' }) },
        })),
      })
      return end('tool_calls')
    }
    chunk({ role: 'assistant', content: 'I could not write there.' })
    end('stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.pathpolicy'])
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

const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
const from = events.length
await rpc('prompt.submit', { session_id: sid, text: 'write to the app folders' })
const deadline = Date.now() + 30000
while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)

check('the turn finished', events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete'))
const results = JSON.stringify(requests[1]?.messages ?? [])
check('the data folder write is refused, naming the folder', results.includes('app\'s own data folder'), results.slice(-400))
check('the skills write is refused, pointing at skill_manage', results.includes('skill_manage'))
check('the memories write is refused, pointing at the memory tool', results.includes('memory tool'))
check('nothing was written', Object.values(targets).every(file => !fs.existsSync(file)), Object.values(targets).filter(fs.existsSync))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
