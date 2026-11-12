// The background review's scheduling (Phase 7, CP7.3) against a running hermes-desktop (the perf
// instance, provider http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py
// first). Under `requests` (`budget.review_mode` = the profile's `idle_once`) with a 3-second idle
// time (`budget.review_idle_minutes: 0.05`):
//   - a session where the user corrects the agent twice gets exactly one review request once it
//     goes quiet (the evidence review: its system prompt, no tools, `response_format`), and the
//     reply's memory entry lands in USER.md;
//   - nothing more for it at the next quiet period (its signals are reviewed);
//   - a smooth session gets no review request at all, idle or switched away from.
// Puts back USER.md and the review knobs it changed.
//   node scripts/perf/review-check.mjs [port] [data folder]
import fs from 'node:fs'
import http from 'node:http'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const home = process.argv[3] ?? path.resolve('target/perf/home')
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 300)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

const ENTRY = 'Uses pnpm, never npm (review-check).'
const userFile = path.join(home, 'memories', 'USER.md')
const userBefore = fs.existsSync(userFile) ? fs.readFileSync(userFile, 'utf8') : null

// The provider: "Done." for turns; the review gets one memory add.
const reviews = []
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    const purpose = req.headers['x-hermes-rs-purpose'] ?? 'main'
    let text = 'Done.'
    if (purpose === 'review') {
      reviews.push(request)
      text = JSON.stringify({ memory: [{ op: 'add', target: 'user', old: null, text: ENTRY, why: 'signals 1 and 2' }], skills: [] })
    }
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

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.review'])
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
  const deadline = Date.now() + 20000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
}
const sessionOf = async () => {
  const { session_id } = await rpc('session.create', {})
  await rpc('session.resume', { session_id })
  return session_id
}
const waitFor = async (ms, done) => {
  const deadline = Date.now() + ms
  while (!done() && Date.now() < deadline) await sleep(200)
  return done()
}

try {
  await rpc('config.set', { key: 'budget.profile', value: 'requests' })
  await rpc('config.set', { key: 'budget.review_mode', value: '' })
  await rpc('config.set', { key: 'budget.review_idle_minutes', value: 0.05 })
  let refused = false
  try { await rpc('config.set', { key: 'budget.review_mode', value: 'sometimes' }) } catch { refused = true }
  check('an unknown review mode is refused', refused)

  // 1. Two corrections, then quiet: one review.
  const corrected = await sessionOf()
  await turn(corrected, 'Install the deps for this project.')
  await turn(corrected, "No, that's wrong.")
  check('no review while the turns run', reviews.length === 0, reviews.length)
  await turn(corrected, 'No, use pnpm instead.')
  const reviewed = await waitFor(30000, () => reviews.length >= 1)
  check('the quiet session got a review request', reviewed)
  await sleep(2000)
  check('exactly one review request', reviews.length === 1, reviews.length)
  const review = reviews[0] ?? {}
  const system = review.messages?.[0]?.content ?? ''
  check('it is the evidence review (its prompt, no tools, the reply schema asked for)',
    system.startsWith('You review one session of a coding agent') && !(review.tools?.length) && review.response_format?.type === 'json_schema',
    { system: system.slice(0, 60), tools: review.tools?.length, format: review.response_format?.type })
  const packet = review.messages?.[1]?.content ?? ''
  check('the packet carries both corrections', packet.includes("No, that's wrong") && packet.includes('use pnpm instead'))
  const landed = await waitFor(10000, () => fs.existsSync(userFile) && fs.readFileSync(userFile, 'utf8').includes(ENTRY))
  check("the review's memory entry is in USER.md", landed)

  // 2. Quiet again (the page opens it again, then leaves it): nothing more.
  await rpc('session.resume', { session_id: corrected })
  await sleep(6000)
  check('no second review for the same signals', reviews.length === 1, reviews.length)

  // 3. A smooth session: no review, idle or switched away from.
  const smooth = await sessionOf()
  for (const q of ['What is a closure?', 'And an iterator?', 'Thanks, that helps.']) await turn(smooth, q)
  await sleep(6000)
  await sessionOf() // switched away
  await sleep(5000)
  check('a smooth session makes no review request', reviews.length === 1, reviews.length)
} finally {
  await rpc('config.set', { key: 'budget.review_idle_minutes', value: null }).catch(() => {})
  if (userBefore === null) fs.rmSync(userFile, { force: true })
  else fs.writeFileSync(userFile, userBefore)
  ws.close()
  provider.close()
}
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
