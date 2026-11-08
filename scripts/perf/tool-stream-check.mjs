// Live tool output against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). A turn's
// first call answers with a terminal call for a command that prints in two bursts; the page must
// receive `tool.progress` events with the output before the `tool.complete` carries the result
// (patches/0017).   node scripts/perf/tool-stream-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
const stamp = Date.now()

const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
}

// The provider: the first call of the turn answers with the terminal call; the next one ends it.
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
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Tool stream check' } }] }))
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    const end = reason => { res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: reason }] })}\n\n`); res.end('data: [DONE]\n\n') }
    if (request.messages.at(-1)?.role === 'user') {
      chunk({ tool_calls: [{ index: 0, id: `call_${stamp}`, type: 'function', function: { name: 'terminal', arguments: JSON.stringify({ command: 'echo one; sleep 1; echo two' }) } }] })
      return end('tool_calls')
    }
    chunk({ role: 'assistant', content: 'Done.' })
    end('stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.tool-stream'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.get(m.id) && pending.delete(m.id) } else if (m.method === 'event') events.push(m.params)
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(new Error(`${method}: ${m.error.message}`)) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})

const { session_id: sid } = await rpc('session.create', {})
const from = events.length
await rpc('prompt.submit', { session_id: sid, text: 'run the command' })
const deadline = Date.now() + 60000
while (
  !events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') &&
  Date.now() < deadline
) await new Promise(r => setTimeout(r, 100))

const progress = events.slice(from).filter(e => e.type === 'tool.progress' && e.payload.tool_id === `call_${stamp}`)
const complete = events.slice(from).find(e => e.type === 'tool.complete' && e.payload.tool_id === `call_${stamp}`)
check('tool.progress events arrived before the result', progress.length > 0 && !!complete, progress.map(p => p.payload.tool_id))
check('the relay carries the output', progress.some(p => String(p.payload.message).includes('one')), progress.map(p => p.payload.message))
check('the finished result carries the full output', !!complete && String(complete.payload.result_text).includes('one') && String(complete.payload.result_text).includes('two'), complete?.payload?.result_text?.slice(0, 120))
ws.close()
provider.close()
