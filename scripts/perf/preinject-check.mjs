// Per-turn pre-injection (Phase 5, CP5.2) against a running hermes-desktop (the perf instance,
// provider http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first).
// Under `requests`, a message a bundled skill covers reaches the model with the skill's body in
// a <context> block in front of it (and no skill_view round trip: one request, not two); the
// page's transcript (session.resume) and the rewind box show only what was typed; the same skill
// is pointed at, not repeated, on the next message; a short generic message gets no block; the
// system prompt lists categories and is the same bytes on every request; under `balanced` the
// message goes as typed. Leaves the perf instance on `requests` (the default).
//   node scripts/perf/preinject-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

// The provider: a short text for everything; every main request is kept.
const mains = []
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
    if (req.headers['x-hermes-rs-purpose'] === 'main') mains.push(request)
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Done.' }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.preinject'])
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
const sessionOf = async () => {
  const { session_id } = await rpc('session.create', {})
  await rpc('session.resume', { session_id })
  return session_id
}
const userTexts = request => (request.messages ?? [])
  .filter(m => m.role === 'user')
  .map(m => (typeof m.content === 'string' ? m.content : (m.content ?? []).map(p => p.text ?? '').join('')))
const toolCalls = request => (request.messages ?? []).flatMap(m => (m.tool_calls ?? []).map(c => c.function?.name))

const ASK = 'search arxiv for papers on diffusion models'
const BODY = '# arXiv Research'

// 1. requests: the skill rides on the message, one request, no skill_view.
await rpc('config.set', { key: 'budget.profile', value: 'requests' })
const first = await sessionOf()
const before = mains.length
await turn(first, ASK)
const sent = mains.slice(before)
check('the turn made exactly one main request (no skill_view round trip)', sent.length === 1, sent.length)
const user = userTexts(sent[0]).at(-1) ?? ''
check('the model got a <context> block with the arxiv skill body in front of the message', user.startsWith('<context>') && user.includes('<skill name="arxiv"') && user.includes(BODY) && user.endsWith(`</context>\n\n${ASK}`), user.slice(0, 160))
check('nobody asked for skill_view', !toolCalls(sent[0]).includes('skill_view'))
const system = sent[0].messages[0].content
check('the system prompt lists categories, not every skill, and is free of the skill body', system.includes('<skill_categories>') && !system.includes('<available_skills>') && !system.includes('Err on the side of loading') && !system.includes(BODY))

// 2. The page's transcript shows what was typed, not the block.
const resumed = await rpc('session.resume', { session_id: first })
const rows = (resumed.messages ?? []).filter(m => m.role === 'user')
check("session.resume's user row is exactly what was typed", rows.length === 1 && rows[0].text === ASK, rows.map(r => r.text?.slice(0, 80)))
check('no row carries the block', !JSON.stringify(resumed.messages ?? []).includes('<context>'))
// The sidebar's search indexes what was typed: a word of the skill's body ("curl") doesn't find
// the session, a typed word ("diffusion") does.
const search = async q => ((await (await fetch(`http://127.0.0.1:${port}/api/sessions/search?q=${q}`)).json()).results ?? [])
  .filter(r => r.session_id === first)
check("session search doesn't find the session by the attached skill's text", (await search('curl')).length === 0)
check('session search finds it by what was typed', (await search('diffusion')).length === 1)

// 3. The next message on the same topic: pointed at, not repeated; same system prompt; same first message.
await turn(first, 'now search arxiv for papers on transformers')
const second = mains.at(-1)
const users = userTexts(second)
check('the first message is replayed byte for byte (the prompt cache holds)', users[0] === user)
check('the second message points at the skill instead of repeating it', users[1]?.includes('already provided earlier') && JSON.stringify(second).split(BODY).length === 2, users[1]?.slice(0, 200))
check('the system prompt is the same bytes on every request', second.messages[0].content === system)

// 4. A short generic message gets no block.
await turn(first, 'hello')
check('"hello" goes as typed', userTexts(mains.at(-1)).at(-1) === 'hello')

// 5. Rewind: the box gets what was typed, and asking again attaches the skill again.
const rows2 = (await rpc('session.resume', { session_id: first })).messages ?? []
const target = rows2.find(m => m.role === 'user')
const rewound = await rpc('session.rewind', { session_id: first, target_message_id: target.row_id })
check('rewind puts back the typed text, not the block', rewound.target_message?.content === ASK, rewound.target_message?.content?.slice(0, 80))
await turn(first, ASK)
check('asking again after the rewind attaches the skill again', userTexts(mains.at(-1)).at(-1) === user)

// 6. balanced: the message goes as typed and the system prompt lists every skill.
await rpc('config.set', { key: 'budget.profile', value: 'balanced' })
const third = await sessionOf()
await turn(third, ASK)
const plain = mains.at(-1)
check('balanced sends the message exactly as typed', userTexts(plain).at(-1) === ASK)
check('and keeps the full skills list in the prompt', plain.messages[0].content.includes('<available_skills>') && plain.messages[0].content.includes('Err on the side of loading'))

await rpc('config.set', { key: 'budget.profile', value: 'requests' })
ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
