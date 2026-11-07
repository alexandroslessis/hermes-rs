// The provider-error card against a running hermes-desktop (the perf instance, port 47810):
// this script is that provider (stop mock_llm.py first) and answers every chat-completions
// request with a 429 daily-limit body, so the turn ends without a reply. Checks the
// message.complete payload the page's error card keys off: status "error", the provider's text,
// and the structured descriptor (layer/code/retryable — a quota limit is not retryable).
//   node scripts/perf/provider-error-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: a daily-limit 429 for every streaming request, one non-stream title reply.
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
    res.writeHead(429, { 'Content-Type': 'application/json', 'Retry-After': '19919' })
    res.end(JSON.stringify({
      error: {
        message: 'Rate limit exceeded: user daily request limit reached (800 of 800). Retry after 19919 seconds.',
        metadata: { retry_after_seconds: 19919 }
      }
    }))
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.provider-error'])
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
check('session created', Boolean(sid))

await rpc('prompt.submit', { session_id: sid, text: 'say hi' })
// The quota 429 is not retried, so the turn ends at once; the deadline is for a cold instance.
const deadline = Date.now() + 30000
while (!events.some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)

const complete = [...events].reverse().find(e => e.session_id === sid && e.type === 'message.complete')?.payload
check('the turn ends with a message.complete', Boolean(complete))
check('the failure status', complete?.status === 'error', complete?.status)
check('the provider error text', typeof complete?.error === 'string' && complete.error.includes('daily request limit reached'), complete?.error)
check('the descriptor names the layer and code', complete?.error_surface?.layer === 'provider' && complete.error_surface?.code === 'rate_limit', complete?.error_surface)
// A quota limit reproduces on retry: the card's Retry action hides.
check('a quota limit is not retryable', complete?.error_surface?.retryable === false, complete?.error_surface)
check('the descriptor names the session that failed', complete?.error_surface?.provider === 'mock', complete?.error_surface)

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
