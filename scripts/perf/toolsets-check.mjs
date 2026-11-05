// Capabilities ▸ Tools against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). Lists the
// toolsets, switches Terminal off (the next turn isn't offered its tools; a call is refused),
// switches it back on, and checks the setting is kept.   node scripts/perf/toolsets-check.mjs [port]
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

// The provider: the first call of a turn asks for the terminal; the next one answers.
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
      chunk({ tool_calls: [{ index: 0, id: `call_${Date.now()}`, type: 'function', function: { name: 'terminal', arguments: '{"command":"echo hi"}' } }] })
      return end('tool_calls')
    }
    chunk({ role: 'assistant', content: 'Done.' })
    end('stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.toolsets'])
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
  const before = requests.length
  await rpc('prompt.submit', { session_id: sid, text })
  const deadline = Date.now() + 30000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  return requests.slice(before)
}
const offered = request => (request.tools ?? []).map(t => t.function.name)

const listed = await rest('GET', '/api/tools/toolsets')
check('toolsets listed, all on', listed.status === 200 && listed.body.length === 10 && listed.body.every(t => t.enabled), listed.body.map(t => t.name))
check('terminal backends', (await rest('GET', '/api/tools/terminal/backends')).body?.active === 'local')
check('a toolset\'s config (no providers)', (await rest('GET', '/api/tools/toolsets/web/config')).body?.has_category === false)

const off = await rest('PUT', '/api/tools/toolsets/terminal', { enabled: false })
check('switch Terminal off', off.status === 200 && off.body.enabled === false, off.body)
check('listed as off', (await rest('GET', '/api/tools/toolsets')).body.find(t => t.name === 'terminal').enabled === false)
const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
const offTurn = await turn(sid, 'run echo hi')
check('the next turn isn\'t offered its tools', !offered(offTurn[0]).includes('terminal') && !offered(offTurn[0]).includes('process_manage') && offered(offTurn[0]).includes('read_file'), offered(offTurn[0]))
check('a call is refused, saying why', JSON.stringify(offTurn[1]?.messages ?? []).includes('turned off by the user'))

const on = await rest('PUT', '/api/tools/toolsets/terminal', { enabled: true })
check('switch it back on', on.body?.enabled === true && (await rest('GET', '/api/tools/toolsets')).body.every(t => t.enabled))
const onTurn = await turn(sid, 'run echo hi again')
check('offered again', offered(onTurn[0]).includes('terminal'))
check('unknown toolset', (await rest('PUT', '/api/tools/toolsets/browser', { enabled: false })).status === 404)

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
