// Deferred tool schemas (Phase 11, CP11.2) against a running hermes-desktop (the perf instance,
// provider http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
// Under `tokens` the request's tool list is the core tools plus `tool_search`, and the system
// prompt names what is deferred; the model calls tool_search, the next request has the loaded tool
// (after the loader, the old list unchanged before it), the model calls it and it runs (three main
// requests, one more than a profile that has the tool from the start); the page sees the
// tool_search call like any tool; a deferred tool called before it was loaded is not run and
// answers with its schema; the main agent has no skill_manage; a session keeps its list when the
// profile changes, and a new session under `requests` (or `balanced`) gets all 18 tools,
// byte-identical across requests, with no tool_search; budget.tool_schemas overrides the profile.
// Leaves the perf instance on `requests` (the default).
//   node scripts/perf/deferred-tools-check.mjs [port]
import http from 'node:http'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'hermes-deferred-'))

// The provider. `script` decides what a main request gets from the tool results it already holds:
//   'search'  -> tool_search(session_search), then session_search, then a text answer
//   'direct'  -> session_search called directly, again, then a text answer
//   'plain'   -> a text answer
let script = 'plain'
const mains = []
const sse = (res, chunks) => {
  res.writeHead(200, { 'Content-Type': 'text/event-stream' })
  for (const chunk of chunks) res.write(`data: ${JSON.stringify(chunk)}\n\n`)
  res.end('data: [DONE]\n\n')
}
const call = (id, name, args) => [
  { choices: [{ index: 0, delta: { role: 'assistant', tool_calls: [{ index: 0, id, type: 'function', function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] },
  { choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] },
]
const text = content => [
  { choices: [{ index: 0, delta: { role: 'assistant', content }, finish_reason: null }] },
  { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] },
]
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
    if (req.headers['x-hermes-rs-purpose'] !== 'main') return sse(res, text('ok'))
    mains.push(request)
    const results = (request.messages ?? []).filter(m => m.role === 'tool').length
    if (script === 'search' && results === 0) return sse(res, call('s1', 'tool_search', { names: ['session_search'] }))
    if (script === 'search' && results === 1) return sse(res, call('c1', 'session_search', { query: 'deferred' }))
    if (script === 'direct' && results < 2) return sse(res, call(`d${results}`, 'session_search', { query: 'deferred' }))
    return sse(res, text('Done.'))
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.deferred'])
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
const turn = async (sid, textToSend) => {
  const from = events.length
  await rpc('prompt.submit', { session_id: sid, text: textToSend })
  const deadline = Date.now() + 30000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  await sleep(300)
}
const newSession = async () => {
  const { session_id } = await rpc('session.create', { cwd: work })
  await rpc('session.resume', { session_id })
  return session_id
}
const names = request => (request.tools ?? []).map(t => t.function?.name)
const size = request => JSON.stringify(request.tools ?? []).length
// A deferred-tool answer is an error with a schema; a real result can mention the words (the store holds earlier runs).
const isDeferredAnswer = text => { try { const v = JSON.parse(text); return v.success === false && v.schema !== undefined } catch { return false } }
const toolTexts = request => (request.messages ?? []).filter(m => m.role === 'tool').map(m => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content)))

// 1. requests: every tool, no tool_search, identical across requests.
await rpc('config.set', { key: 'budget.profile', value: 'requests' })
script = 'plain'
let before = mains.length
const plain = await newSession()
await turn(plain, 'hello')
await turn(plain, 'hello again')
const plainRequests = mains.slice(before)
check('requests: two turns, two main requests', plainRequests.length === 2, plainRequests.length)
check('requests: all 18 tools, skill_manage among them, no tool_search', names(plainRequests[0]).length === 18 && names(plainRequests[0]).includes('skill_manage') && !names(plainRequests[0]).includes('tool_search'), names(plainRequests[0]))
check('requests: the tool list is identical in both requests', JSON.stringify(plainRequests[0].tools) === JSON.stringify(plainRequests[1].tools))
check('requests: the system prompt has no deferred block', !plainRequests[0].messages[0].content.includes('# Deferred tools'))
const fullBytes = size(plainRequests[0])

// 2. tokens: core + tool_search; the saving; the prompt.
await rpc('config.set', { key: 'budget.profile', value: 'tokens' })
before = mains.length
script = 'search'
const tokens = await newSession()
const from = events.length
await turn(tokens, 'What did we do about deferred tools last week?')
const flow = mains.slice(before)
check('tokens: three main requests (tool_search adds one round trip)', flow.length === 3, flow.length)
const first = names(flow[0])
check('tokens: the first list is the core tools and tool_search', first.length === 11 && first.at(-1) === 'tool_search' && !first.includes('session_search') && !first.includes('skill_manage'), first)
check('tokens: the tools block is less than 60% of the full one', size(flow[0]) * 10 < fullBytes * 6, [size(flow[0]), fullBytes])
const system = flow[0].messages[0].content
check('tokens: the prompt names the deferred tools and says there is no skill_manage', system.includes('# Deferred tools') && system.includes('- session_search: ') && system.includes('- delegate_task: ') && system.includes('no skill_manage tool'))
check('tokens: nothing in the prompt tells the model to call skill_manage', !/skill_manage\(|\(skill_manage\)|with skill_manage|via skill_manage/.test(system))
const second = names(flow[1])
check('tokens: the loaded tool joined the list after tool_search, the old list unchanged before it', second.length === first.length + 1 && second.at(-1) === 'session_search' && JSON.stringify(second.slice(0, first.length)) === JSON.stringify(first), second)
check('tokens: the system prompt is byte-identical across the loading', flow[0].messages[0].content === flow[1].messages[0].content)
const searchResult = JSON.parse(toolTexts(flow[1]).at(-1) ?? '{}')
check('tokens: tool_search returned the schema of session_search', searchResult.loaded?.[0] === 'session_search' && searchResult.schemas?.[0]?.parameters?.properties?.query !== undefined, searchResult)
check('tokens: the loaded tool ran (no deferred answer)', !isDeferredAnswer(toolTexts(flow[2]).at(-1) ?? ''), (toolTexts(flow[2]).at(-1) ?? '').slice(0, 120))
check('tokens: the page saw the tool_search call like any tool', events.slice(from).some(e => e.type === 'tool.start' && JSON.stringify(e.payload).includes('tool_search')), events.slice(from).filter(e => String(e.type).startsWith('tool.')).map(e => e.type))

// 3. A deferred tool called directly is not run.
before = mains.length
script = 'direct'
const direct = await newSession()
await turn(direct, 'search the old sessions')
const directFlow = mains.slice(before)
const answer = toolTexts(directFlow[1]).at(-1) ?? ''
check('a deferred tool called before loading answers with its schema, not run', answer.includes('"success":false') && answer.includes('not run') && answer.includes('"schema"'), answer.slice(0, 200))
check('...and is in the list from the next request, where the retry runs', directFlow.length === 3 && names(directFlow[1]).includes('session_search') && !isDeferredAnswer(toolTexts(directFlow[2]).at(-1) ?? ''), { requests: directFlow.length, listed: names(directFlow[1]).includes('session_search'), last: (toolTexts(directFlow.at(-1)).at(-1) ?? '').slice(0, 160) })

// 4. A session keeps its list when the profile changes; a new one follows the profile.
await rpc('config.set', { key: 'budget.profile', value: 'balanced' })
script = 'plain'
before = mains.length
await turn(tokens, 'one more question')
check('a session begun under tokens stays deferred after switching to balanced', names(mains.at(-1)).at(-1) === 'session_search' && names(mains.at(-1)).includes('tool_search'), names(mains.at(-1)))
const balanced = await newSession()
await turn(balanced, 'hello')
check('a new session under balanced has all 18 tools, no tool_search', names(mains.at(-1)).length === 18 && !names(mains.at(-1)).includes('tool_search'))
check('balanced and requests send the same bytes of tools', size(mains.at(-1)) === fullBytes, [size(mains.at(-1)), fullBytes])
// The context gauge counts what each session sends (CP11.2 review): the deferred session's tool block is smaller.
const gaugeTools = async sid => {
  const breakdown = await rpc('session.context_breakdown', { session_id: sid })
  return (breakdown.categories ?? []).find(c => c.id === 'tool_definitions')?.tokens ?? -1
}
const [gaugeTokens, gaugeBalanced] = [await gaugeTools(tokens), await gaugeTools(balanced)]
check('the context breakdown counts fewer tool tokens for the deferred session', gaugeTokens > 0 && gaugeTokens * 10 < gaugeBalanced * 8, [gaugeTokens, gaugeBalanced])

// 5. The override: budget.tool_schemas on another profile, validated.
await rpc('config.set', { key: 'budget.profile', value: 'requests' })
check('budget.tool_schemas rejects a bad value', await rpc('config.set', { key: 'budget.tool_schemas', value: 'some' }).then(() => false, () => true))
await rpc('config.set', { key: 'budget.tool_schemas', value: 'deferred' })
await turn(await newSession(), 'hello')
check('requests + budget.tool_schemas deferred: a deferred session', names(mains.at(-1)).includes('tool_search') && !names(mains.at(-1)).includes('skill_manage'), names(mains.at(-1)))
await rpc('config.set', { key: 'budget.tool_schemas', value: null })
await turn(await newSession(), 'hello')
check('the override cleared: all tools again', names(mains.at(-1)).length === 18)

await rpc('config.set', { key: 'budget.profile', value: 'requests' })
ws.close()
provider.close()
fs.rmSync(work, { recursive: true, force: true })
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
