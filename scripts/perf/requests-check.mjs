// The request ledger and quota governor against a running hermes-desktop (the perf instance,
// provider http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
// Runs two turns and compares `requests.status` with the requests this mock really served
// (total and per purpose), then sets a daily limit through the config record and checks the
// status bar's allowance gauge (`plan_gauge` in `session.usage`), the refusal at zero (no request
// reaches the mock), that clearing the limit lifts it, and that a limit saved on the custom
// endpoint (the Settings field) reaches the ledger. Leaves a null limit in the instance's config
// record (it can't be deleted, only nulled) and none on the endpoint.
//   node scripts/perf/requests-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: answers every chat request with a short text, counts them by the purpose header.
const served = { total: 0, byPurpose: {} }
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    served.total++
    const purpose = req.headers['x-hermes-rs-purpose'] ?? 'unlabelled'
    served.byPurpose[purpose] = (served.byPurpose[purpose] ?? 0) + 1
    if (!request.stream) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Done.' }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.requests'])
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
const status = () => rpc('requests.status', {})
const turn = async (sid, text) => {
  const from = events.length
  let refused
  try {
    await rpc('prompt.submit', { session_id: sid, text })
  } catch (error) {
    refused = error.message
  }
  const deadline = Date.now() + 20000
  while (!refused && !events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  await sleep(300) // the title, and the ledger thread, catch up
  return { refused, events: events.slice(from) }
}
const setLimits = value => rpc('config.set', { key: 'requests.limits', value })

const first = await status()
const providerId = first.provider
check('requests.status answers for the active provider', typeof providerId === 'string' && providerId !== '' && typeof first.used === 'number', first)
check('it has the day\'s reset time and a purpose split', first.resets_at > Date.now() / 1000 && typeof first.by_purpose === 'object', first)

// Start without a limit left over from an earlier run.
await setLimits({ [providerId]: { daily_limit: null } })

// Two turns: what the ledger counts is what the mock served.
const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
const before = await status()
const servedBefore = { total: served.total, byPurpose: { ...served.byPurpose } }
await turn(sid, 'first question')
await turn(sid, 'second question')
const after = await status()
const servedDelta = served.total - servedBefore.total
check('two turns made requests', servedDelta >= 2, servedDelta)
check('the ledger counted exactly the requests the mock served', after.used - before.used === servedDelta, { ledger: after.used - before.used, mock: servedDelta })
const delta = purpose => (after.by_purpose[purpose] ?? 0) - (before.by_purpose[purpose] ?? 0)
const mockDelta = purpose => (served.byPurpose[purpose] ?? 0) - (servedBefore.byPurpose[purpose] ?? 0)
for (const purpose of new Set([...Object.keys(served.byPurpose), ...Object.keys(after.by_purpose)])) {
  check(`purpose "${purpose}" agrees (ledger ${delta(purpose)}, mock ${mockDelta(purpose)})`, delta(purpose) === mockDelta(purpose))
}
check('no request reached the provider unlabelled', !served.byPurpose.unlabelled, served.byPurpose)
check('the main turns are "main" requests', delta('main') === 2, after.by_purpose)

// A limit well above the use: the gauge shows it.
const limit = after.used + 100
await setLimits({ [providerId]: { daily_limit: limit } })
const limited = await status()
check('the limit set in the config record shows in requests.status', limited.limit === limit, limited)
const usageFrom = events => events.filter(e => e.session_id === sid && e.type === 'session.usage').map(e => e.payload?.usage).filter(Boolean)
const third = await turn(sid, 'third question')
const gauge = usageFrom(third.events).map(u => u.plan_gauge).filter(Boolean).at(-1)
check('session.usage carries the allowance gauge', gauge && gauge.id === 'requests' && /^Requests \d+\/\d+$/.test(gauge.label) && gauge.used_bp > 0 && gauge.used_bp < 10000 && gauge.resets_at_ms > Date.now(), gauge)
check('the gauge\'s label says used/limit', gauge?.label.endsWith(`/${limit}`), gauge?.label)

// Used up: the turn is refused before any request (the 5% warning is covered by the runtime's
// `turns` tests: it needs a limit this instance can't get close to in a few turns).
const current = await status()
await setLimits({ [providerId]: { daily_limit: current.used } })
const servedBeforeRefusal = served.total
const refusedTurn = await turn(sid, 'one more')
const refusal = refusedTurn.refused ?? refusedTurn.events.map(e => JSON.stringify(e.payload ?? {})).join(' ')
check('used up: the turn is refused, saying so and when it resets', /used up/.test(refusal) && /resets at/.test(refusal), refusal.slice(0, 200))
check('and no request reached the provider', served.total === servedBeforeRefusal, served.total - servedBeforeRefusal)

// Cleared (the config record merges what is set into what is there, so a limit is removed by
// setting it to null, not by sending an empty object), it works again.
await setLimits({ [providerId]: { daily_limit: null } })
const cleared = await status()
check('clearing the limit removes it', cleared.limit === null || cleared.limit === undefined, cleared.limit)
const again = await turn(sid, 'back to normal')
check('and turns run again', !again.refused && served.total > servedBeforeRefusal)

// The Settings field (patch 0021) saves `daily_request_limit` through the custom-endpoint route,
// which hands it to the ledger; re-saved with its own fields, the endpoint is otherwise unchanged.
const restCall = async (method, path, body) => {
  const r = await fetch(`http://127.0.0.1:${port}${path}`, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
const listed = (await restCall('GET', '/api/providers/custom-endpoints')).body?.endpoints?.find(e => e.id === providerId)
if (!listed) {
  check('the active provider is a custom endpoint (to try the Settings field)', false, providerId)
} else {
  const saveEndpoint = daily_request_limit => restCall('POST', '/api/providers/custom-endpoints', {
    id: listed.id, name: listed.name, base_url: listed.base_url, model: listed.model, models: listed.models,
    discover_models: listed.discover_models, context_length: listed.context_length ?? undefined, daily_request_limit
  })
  const savedLimit = (await status()).used + 50
  const saved = await saveEndpoint(savedLimit)
  check('the endpoint saves a daily request limit and lists it', saved.status === 200 && saved.body.endpoints.find(e => e.id === providerId)?.daily_request_limit === savedLimit, saved.body?.detail)
  check('and the ledger has it at once', (await status()).limit === savedLimit)
  const clearedEndpoint = await saveEndpoint(null)
  check('saving it empty clears it', clearedEndpoint.status === 200 && (await status()).limit == null)
}

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
