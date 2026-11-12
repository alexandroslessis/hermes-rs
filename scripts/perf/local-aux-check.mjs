// "Use a local model for background jobs" (Settings ▸ Model ▸ Auxiliary) against a running
// hermes-desktop (the perf instance, provider http://127.0.0.1:47900/v1 named `mock`): this script
// is that provider (stop mock_llm.py first) and also a second, "local" server on :47901 that it
// adds as a custom endpoint. Checks `GET/POST /api/model/auxiliary/local` (the servers it finds,
// every side job set to the chosen one, 400 and nothing changed when it is down or unknown), and
// that compression then runs on the local server (zero `compress` requests on the main provider),
// falls back to the main model when the local server is stopped, and "reset all" undoes it.
// Leaves the perf instance without the extra endpoint.   node scripts/perf/local-aux-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

// A mock OpenAI-compatible server: lists `models`, answers a (long, so that a summary shrinks the
// chat) reply to everything, and counts requests per purpose header and per model.
const serve = (listenPort, models, reply) => {
  const seen = { purposes: {}, models: [] }
  const server = http.createServer((req, res) => {
    let body = ''
    req.on('data', c => (body += c))
    req.on('end', () => {
      if (req.method === 'GET') {
        res.writeHead(200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify({ object: 'list', data: models.map(id => ({ id, object: 'model' })) }))
      }
      const request = JSON.parse(body || '{}')
      const purpose = req.headers['x-hermes-rs-purpose'] ?? 'main'
      seen.purposes[purpose] = (seen.purposes[purpose] ?? 0) + 1
      seen.models.push(request.model)
      const text = purpose === 'compress' ? `## Goal\nSummary from ${request.model}.` : reply
      if (!request.stream) {
        res.writeHead(200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: text } }] }))
      }
      res.writeHead(200, { 'Content-Type': 'text/event-stream' })
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }] })}\n\n`)
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
      res.end('data: [DONE]\n\n')
    })
  })
  return { seen, server, up: () => new Promise((ok, fail) => server.listen(listenPort, '127.0.0.1', ok).on('error', fail)) }
}
const long = `Fine. ${'x'.repeat(40000)}`
const main = serve(47900, ['mock'], long)
const local = serve(47901, ['nomic-embed-text', 'local-small', 'local-big'], 'Local reply.')
await main.up()
await local.up()

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.localaux'])
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
const turn = async (sid, text) => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text })
  const deadline = Date.now() + 30000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
}
const aux = async () => (await rest('GET', '/api/model/auxiliary')).body
const slots = ['compression', 'approval', 'title_generation', 'background_review']

// The extra endpoint, as the user would add it under Settings ▸ Providers.
const added = await rest('POST', '/api/providers/custom-endpoints', { name: 'Local mock', base_url: 'http://127.0.0.1:47901/v1', model: 'local-small', make_default: false })
check('a local endpoint is added', added.status === 200 && added.body?.id === 'local-mock', added.body?.id)

const found = (await rest('GET', '/api/model/auxiliary/local')).body
const server = found?.servers?.find(s => s.provider === 'local-mock')
check('the local server is found with its chat models (no embedding model)', server?.models?.join() === 'local-big,local-small' && server.configured === true, found)

check('an unknown server is refused', (await rest('POST', '/api/model/auxiliary/local', { provider: 'nowhere' })).status === 400)
check('a model the server lacks is refused', (await rest('POST', '/api/model/auxiliary/local', { provider: 'local-mock', model: 'nope' })).status === 400)
check('nothing changed', (await aux()).tasks.every(t => t.provider === 'auto'))

const click = await rest('POST', '/api/model/auxiliary/local', { provider: 'local-mock' })
check('one click answers ok', click.status === 200 && click.body?.provider === 'local-mock' && click.body.model === 'local-small', click.body)
const after = await aux()
check('every side job is on the local model', slots.every(s => { const t = after.tasks.find(x => x.task === s); return t?.provider === 'local-mock' && t.model === 'local-small' && t.local_endpoint === true }), after.tasks)
check('the main model is untouched', after.main.provider === 'mock' && after.main.model === 'mock', after.main)

// Compression runs on the local server.
const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
for (const text of [`local aux ${Date.now()}`, 'second', 'third', 'fourth']) await turn(sid, text)
await sleep(500)
const compressedBefore = main.seen.purposes.compress ?? 0
const first = await rpc('session.compress', { session_id: sid })
// (`status` stays "noop" for a compaction that hides messages instead of removing them: the
// token counts say whether the context shrank.)
check('/compress ran and shrank the context', first.after_tokens < first.before_tokens, [first.status, first.before_tokens, first.after_tokens])
check('zero compress requests on the main provider', (main.seen.purposes.compress ?? 0) === compressedBefore, main.seen.purposes)
check('the local server wrote the summary', local.seen.purposes.compress === 1 && local.seen.models.includes('local-small'), local.seen)

// The local server goes away: compression falls back to the main model, no error.
await new Promise(ok => { local.server.close(ok); local.server.closeAllConnections?.() })
for (const text of ['fifth', 'sixth', 'seventh', 'eighth']) await turn(sid, text)
await sleep(500)
const second = await rpc('session.compress', { session_id: sid })
check('with the local server down, /compress still works', second.status !== 'aborted' && second.after_tokens < second.before_tokens, [second.status, second.before_tokens, second.after_tokens])
check('the main model wrote that summary', (main.seen.purposes.compress ?? 0) === compressedBefore + 1, main.seen.purposes)
check('the local server was not reached again', local.seen.purposes.compress === 1)

// Down at click time: refused with a plain message, nothing changed.
await rest('POST', '/api/model/auxiliary/local', { provider: 'local-mock', model: 'local-big' }).then(r =>
  check('picking a model while the server is down is refused', r.status === 400 && /No local model server/.test(r.body?.detail ?? ''), r.body))
check('the jobs still point at the local model', (await aux()).tasks.every(t => t.model === 'local-small'))

const reset = await rest('POST', '/api/model/set', { scope: 'auxiliary', task: '__reset__', provider: 'mock', model: 'mock' })
check('reset all to the main model', reset.body?.reset === true && (await aux()).tasks.every(t => t.provider === 'auto'))
const removed = await rest('DELETE', '/api/providers/custom-endpoints/local-mock')
check('the extra endpoint is removed', removed.status === 200, removed.status)

ws.close()
main.server.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
