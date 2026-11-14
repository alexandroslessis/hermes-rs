// The provider fallback chain against a running hermes-desktop (the perf instance, provider "mock"
// at http://127.0.0.1:47900/v1): this script is that provider AND a second one ("mock b", port
// 47901, registered as a custom endpoint and removed again), so stop mock_llm.py first.
// Scenarios, each counted on both mocks:
//   - the main provider answers 401 / 400 (a wrong key, a bad request): the turn ends with the
//     error and NOTHING reaches the fallback (no silent switch across an auth error);
//   - an invalid chain (unknown provider, the main model itself, a repeat) is ignored: the main
//     provider still serves the turn;
//   - the daily limit used up on our side (the limit set to what is already used; the user's version: a limit of 3): the turn goes to
//     the fallback with the page's "fallback" status line and the main provider sees nothing;
//   - the main provider answers the daily-quota 429: the turn completes on the fallback, with a
//     notice that says which provider has the conversation and that its cache starts empty; the
//     next turn goes straight to the fallback (the quota is known to be gone);
//   - the fallback removed (config key emptied): the refusal the governor always gave.
// Leaves the instance's config record with an empty `fallback_providers` and no limit on "mock".
//   node scripts/perf/fallback-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 400)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// Provider A's mode decides what it answers; B always answers.
let modeA = 'ok'
const served = { a: [], b: [] }
const mockServer = (name, answer) => http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: name, object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    const purpose = req.headers['x-hermes-rs-purpose'] ?? 'unlabelled'
    served[name === 'mock' ? 'a' : 'b'].push({ purpose, model: request.model, stream: !!request.stream })
    const reply = answer(request)
    if (reply) return reply(res)
    if (!request.stream) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: `Answered by ${name}.` }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
const json = (status, body, headers = {}) => res => {
  res.writeHead(status, { 'Content-Type': 'application/json', ...headers })
  res.end(JSON.stringify(body))
}
const providerA = mockServer('mock', request => {
  if (!request.stream || modeA === 'ok') return null
  if (modeA === '401') return json(401, { error: { message: 'Invalid API key' } })
  if (modeA === '400') return json(400, { error: { message: 'messages: field required' } })
  if (modeA === 'daily429') {
    return json(429, { error: { message: 'Rate limit exceeded: user daily request limit reached (800 of 800). Retry after 19919 seconds.', code: 429, metadata: { retry_after_seconds: 19919 } } }, { 'Retry-After': '19919' })
  }
  return null
})
const providerB = mockServer('mock-b', () => null)
await new Promise((ok, fail) => providerA.listen(47900, '127.0.0.1', ok).on('error', fail))
await new Promise((ok, fail) => providerB.listen(47901, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.fallback'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method === 'event') events.push(m.params)
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(Object.assign(new Error(`${method}: ${m.error.message}`), { rpc: m.error })) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})
const restCall = async (method, path, body) => {
  const r = await fetch(`http://127.0.0.1:${port}${path}`, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
const setConfig = (key, value) => rpc('config.set', { key, value })
const status = () => rpc('requests.status', {})

// A turn: what the page would show (status lines, the final text) and what each mock served.
const turn = async (sid, text) => {
  const from = events.length
  const mark = { a: served.a.length, b: served.b.length }
  let refused
  try {
    await rpc('prompt.submit', { session_id: sid, text })
  } catch (error) {
    refused = error.message
  }
  const deadline = Date.now() + 30000
  while (!refused && !events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  await sleep(400)
  const mine = events.slice(from).filter(e => e.session_id === sid)
  const main = side => served[side].slice(mark[side]).filter(r => r.stream && r.purpose === 'main')
  return {
    refused,
    notices: mine.filter(e => e.type === 'status.update' && /switched to/.test(e.payload?.text ?? '')).map(e => ({ kind: e.payload.kind, text: e.payload.text })),
    text: mine.filter(e => e.type === 'message.complete').map(e => e.payload?.text ?? '').join(''),
    allText: mine.map(e => JSON.stringify(e.payload ?? {})).join(' '),
    a: main('a').length,
    b: main('b').length,
  }
}
const newSession = async () => {
  const { session_id } = await rpc('session.create', {})
  await rpc('session.resume', { session_id })
  return session_id
}

// B is a configured provider (Settings ▸ Providers); the chain only names it.
const added = await restCall('POST', '/api/providers/custom-endpoints', {
  name: 'Mock B', base_url: 'http://127.0.0.1:47901/v1', model: 'mock-b', models: ['mock-b'], discover_models: false,
})
const bId = added.body?.id
check('provider B is configured', added.status === 200 && !!bId, added.body?.detail ?? bId)
const aId = (await status()).provider
await setConfig('requests.limits', { [aId]: { daily_limit: null } })
await setConfig('fallback_providers', [])
const chain = [{ provider: bId, model: 'mock-b' }]

// 1. Without a chain nothing changes: the 401 is the user's to see.
modeA = '401'
let sid = await newSession()
let t = await turn(sid, 'no chain, wrong key')
check('without a chain the 401 ends the turn and only A is asked', t.a === 1 && t.b === 0 && t.notices.length === 0, t)

// 2. With a chain, auth and ordinary 4xx still never leave for the fallback.
await setConfig('fallback_providers', chain)
for (const mode of ['401', '400']) {
  modeA = mode
  sid = await newSession()
  t = await turn(sid, `chain set, A answers ${mode}`)
  check(`A answers ${mode}: the fallback is not asked and nothing is switched`, t.a >= 1 && t.b === 0 && t.notices.length === 0, t)
  check(`and the page shows A's error`, /(Authentication failed|Invalid API key|messages: field required|Bad request)/.test(t.allText), t.allText.slice(0, 200))
}

// 3. An invalid chain is ignored (logged), the main provider serves.
modeA = 'ok'
await setConfig('fallback_providers', [{ provider: 'nobody', model: 'x' }, { provider: aId, model: 'mock' }, { provider: bId }, 7])
sid = await newSession()
t = await turn(sid, 'invalid chain')
check('an invalid chain changes nothing: A answers', t.a === 1 && t.b === 0 && /Answered by mock\./.test(t.text), t)

// 4. Our own daily limit used up (the limit set to what is already used): straight to the
// fallback, no request to A.
await setConfig('fallback_providers', chain)
await sleep(500)
const used = (await status()).used
await setConfig('requests.limits', { [aId]: { daily_limit: used } })
const afterLimit = await status()
check('the limit is used up on our side', afterLimit.limit === used && afterLimit.used >= afterLimit.limit, afterLimit)
sid = await newSession()
t = await turn(sid, 'limit used up')
check('the turn goes to the fallback; A is not asked', t.a === 0 && t.b === 1 && /Answered by mock-b\./.test(t.text), t)
check('the page shows a "fallback" status line naming B and the cache', t.notices.length === 1 && t.notices[0].kind === 'fallback' && t.notices[0].text.includes('switched to') && t.notices[0].text.includes(bId) && /cache/.test(t.notices[0].text), t.notices)
await setConfig('requests.limits', { [aId]: { daily_limit: null } })

// 5. The daily-quota 429: the turn completes on the fallback; the next goes straight there.
modeA = 'daily429'
sid = await newSession()
t = await turn(sid, 'quota 429')
check('daily 429: A asked once, B once, the turn completes with B\'s answer', t.a === 1 && t.b === 1 && /Answered by mock-b\./.test(t.text), t)
check('the notice says A used up its quota and B has the conversation', t.notices.length === 1 && /mock has used up its request quota/.test(t.notices[0].text) && t.notices[0].text.includes(`switched to ${bId}`), t.notices)
t = await turn(sid, 'second message')
check('the next turn does not ask A again (its quota is known gone)', t.a === 0 && t.b === 1 && /Answered by mock-b\./.test(t.text), t)

// 6. The fallback removed: the governor's own refusal, no request to B.
await setConfig('fallback_providers', [])
t = await turn(sid, 'without the fallback')
check('without the fallback the turn is refused (used up) and B sees nothing', t.b === 0 && t.a === 0 && /used up/.test(t.refused ?? t.allText), t.refused ?? t.allText.slice(0, 200))

if (bId) await restCall('DELETE', `/api/providers/custom-endpoints/${bId}`)
ws.close()
providerA.close()
providerB.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
