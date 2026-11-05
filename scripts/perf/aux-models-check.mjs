// Settings ▸ Model ▸ Auxiliary against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1 named `mock`): this script is that provider (stop mock_llm.py first).
// Lists the side jobs, refuses bad assignments, picks a title model and a compression model with
// its own effort, checks a new session's title and a /compress use them, then resets all to the
// main model.   node scripts/perf/aux-models-check.mjs [port]
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

// The provider: every request is recorded; titles (not streamed) get a title, the rest a reply.
const requests = []
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: ['mock', 'title-mock', 'summary-mock'].map(id => ({ id, object: 'model' })) }))
    }
    const request = JSON.parse(body || '{}')
    requests.push(request)
    const text = request.model === 'title-mock' ? 'Aux title' : request.model === 'summary-mock' ? '## Goal\nThe summary.' : 'Fine.'
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
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.aux'])
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
const task = (list, slot) => list.tasks.find(t => t.task === slot)
const set = body => rest('POST', '/api/model/set', { scope: 'auxiliary', ...body })

const first = (await rest('GET', '/api/model/auxiliary')).body
check('four side jobs, following the main model', first.tasks.map(t => t.task).join() === 'compression,approval,title_generation,background_review' && first.tasks.every(t => t.provider === 'auto'), first.tasks)
check('the main model', first.main.provider === 'mock' && first.main.model === 'mock', first.main)
check('the background review has its own label', task(first, 'background_review').label === 'Background review')

check('an unknown job is refused', (await set({ task: 'vision', provider: 'mock', model: 'mock' })).status === 400)
check('an unknown provider is refused', (await set({ task: 'compression', provider: 'nowhere', model: 'm' })).status === 400)
check('a bad effort is refused', (await set({ task: 'compression', provider: 'mock', model: 'm', reasoning_effort: 'max' })).status === 400)

check('pick a title model', (await set({ task: 'title_generation', provider: 'mock', model: 'title-mock' })).status === 200)
const picked = await set({ task: 'compression', provider: 'mock', model: 'summary-mock', reasoning_effort: 'low' })
check('pick a compression model with its effort', picked.status === 200 && picked.body.tasks[0] === 'compression', picked.body)
const after = (await rest('GET', '/api/model/auxiliary')).body
check('listed', task(after, 'compression').model === 'summary-mock' && task(after, 'compression').reasoning_effort === 'low' && task(after, 'compression').base_url === 'http://127.0.0.1:47900/v1' && task(after, 'compression').local_endpoint === true, task(after, 'compression'))

const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
const before = requests.length
for (const text of [`aux check ${Date.now()}`, 'second', 'third', 'fourth']) await turn(sid, text)
await sleep(1000)
check('the turns ran on the main model', requests.slice(before).some(r => r.model === 'mock' && r.stream))
check('the title came from the title model', requests.slice(before).some(r => r.model === 'title-mock'), requests.slice(before).map(r => r.model))
check('the session got that title', events.some(e => e.session_id === sid && e.type === 'session.title' && e.payload?.title === 'Aux title'))

const beforeCompress = requests.length
const compressed = await rpc('session.compress', { session_id: sid })
const summary = requests.slice(beforeCompress).find(r => r.model === 'summary-mock')
// (A few one-line turns may not get shorter: "noop" then, as smoke.mjs allows.)
check('/compress summarized with the compression model', ['compressed', 'noop'].includes(compressed.status) && summary !== undefined, compressed.status)
check('with its effort', summary?.reasoning_effort === 'low', summary && Object.keys(summary))

// No effort of its own ("inherit"): compression thinks as hard as the chat.
await set({ task: 'compression', provider: 'mock', model: 'summary-mock', reasoning_effort: null })
await rpc('config.set', { session_id: sid, key: 'reasoning', value: 'high' })
await turn(sid, 'fifth')
const beforeInherit = requests.length
await rpc('session.compress', { session_id: sid })
const inherited = requests.slice(beforeInherit).find(r => r.model === 'summary-mock')
check('an inherited effort is the chat\'s', inherited?.reasoning_effort === 'high', inherited?.reasoning_effort)

const reset = await set({ task: '__reset__', provider: 'mock', model: 'mock' })
check('reset all to the main model', reset.body?.reset === true && (await rest('GET', '/api/model/auxiliary')).body.tasks.every(t => t.provider === 'auto'))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
