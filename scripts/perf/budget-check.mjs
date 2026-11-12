// Budget profiles against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
// Checks that `budget.profile` is offered by the Settings schema, that config.set switches it
// (and refuses an unknown profile and a 0 for the caps that would break a turn), that
// `delegation.status` reports the effective width, and that a session's system prompt takes its
// budget wording when the session starts and keeps it when the profile changes later (prompt
// caching): a session made after the switch gets the new wording. Leaves the perf instance on
// `requests` (the default).
//   node scripts/perf/budget-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: a short text for everything; the system prompt and tools of each main request.
const systems = []
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
    if (req.headers['x-hermes-rs-purpose'] === 'main') {
      systems.push({ system: request.messages?.[0]?.content ?? '', tools: JSON.stringify(request.tools ?? []) })
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Done.' }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.budget'])
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
const turn = async (sid, text) => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text })
  const deadline = Date.now() + 20000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  await sleep(300)
}
const setProfile = value => rpc('config.set', { key: 'budget.profile', value })
const refused = async (key, value) => {
  try {
    await rpc('config.set', { key, value })
    return undefined
  } catch (error) {
    return error.rpc
  }
}
const REQUESTS = "Budget: the user's model provider limits how many requests"
const TOKENS = 'Budget: the user is limited by tokens'
const sessionOf = async () => {
  const { session_id } = await rpc('session.create', {})
  await rpc('session.resume', { session_id })
  return session_id
}

// 1. What the Settings page is offered, and the default.
const schema = await (await fetch(`http://127.0.0.1:${port}/api/config/schema`)).json().catch(() => ({}))
const field = schema.fields?.['budget.profile']
check('the Settings schema offers budget.profile as a select of the three profiles', field?.type === 'select' && JSON.stringify(field.options) === '["requests","balanced","tokens"]', field)
await setProfile('requests')
check('config.get answers requests', (await rpc('config.get', { key: 'budget.profile' })).value === 'requests')
check('delegation.status reports the requests width (3)', (await rpc('delegation.status', {})).max_concurrent_children === 3)

// 2. A session made under requests carries the requests wording; the same session after a switch
//    keeps its prompt byte for byte, and its tool schemas state no profile-dependent number.
const first = await sessionOf()
await turn(first, 'one')
await setProfile('tokens')
check('delegation.status follows the profile (10)', (await rpc('delegation.status', {})).max_concurrent_children === 10)
await turn(first, 'two')
const mine = systems.slice(-2)
check("the requests session's prompt has the requests wording", mine[0]?.system.includes(REQUESTS) && !mine[0].system.includes(TOKENS), mine[0]?.system.slice(0, 80))
check('and is the same prompt after the profile changed (prompt caching)', mine.length === 2 && mine[0].system === mine[1].system)
check('read_file and delegate_task schemas state no profile-dependent number', mine.length === 2 && !/default: 2000|max: 2000|up to 10 in parallel/.test(mine[0].tools))

// 3. A session made after the switch gets the new wording; balanced adds none.
const second = await sessionOf()
await turn(second, 'three')
const tokensSystem = systems.at(-1)?.system ?? ''
check('a session started after the switch has the tokens wording', tokensSystem.includes(TOKENS) && !tokensSystem.includes(REQUESTS))
check('the tokens prompt lists skill names only', tokensSystem.includes('Only the names are listed') && !tokensSystem.includes('Err on the side of loading'))
await setProfile('balanced')
check("delegation.status reports balanced's 10", (await rpc('delegation.status', {})).max_concurrent_children === 10)
const third = await sessionOf()
await turn(third, 'four')
const balancedSystem = systems.at(-1)?.system ?? ''
check('balanced adds no budget wording (the prompt from before profiles)', !balancedSystem.includes('Budget: the user'))

// 4. Bad values are refused with an invalid-params error and change nothing.
for (const [key, value] of [['budget.profile', 'fast'], ['budget.result_size_pct', 0], ['budget.length_continuations', 0], ['budget.invalid_tool_retries', 0], ['budget.bogus', 1]]) {
  const error = await refused(key, value)
  check(`config.set ${key} = ${JSON.stringify(value)} is refused`, error?.code === -32602, error)
}
check('and the profile is still balanced', (await rpc('config.get', { key: 'budget.profile' })).value === 'balanced')
const override = await rpc('config.set', { key: 'budget.max_iterations', value: 100 })
check('a valid per-knob override is accepted', override.ok === true, override)
await rpc('config.set', { key: 'budget.max_iterations', value: null })

await setProfile('requests')
ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
