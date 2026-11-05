// The Hermes importer against a running hermes-desktop started with HERMES_HOME set to a home
// built by make-hermes-db.py (the perf instance; provider http://127.0.0.1:47900/v1, which this
// script is: stop mock_llm.py first).   node scripts/perf/import-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 260)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

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
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Continuing.' }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.import'])
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

const list = await rpc('session.foreign.list', { source: null, offset: 0 })
const titles = list.sessions.map(s => `${s.label}: ${s.title}`)
check('lists the Hermes sessions of both profiles', list.sessions.length === 3 && titles.includes('Hermes: Read the notes') && titles.includes('Hermes (work): Work chat'), titles)
check('Claude Code / Codex: none', (await rpc('session.foreign.list', { source: 'claude', offset: 0 })).sessions.length === 0)
const notes = list.sessions.find(s => s.title === 'Read the notes')
check('turn count and folder', notes.turn_count === 1 && /import-folder/.test(notes.cwd ?? ''), notes)
const preview = await rpc('session.foreign.preview', { id: notes.id })
check('preview', preview.messages.map(m => m.content).join(' | ') === 'what do my notes say? | Your notes say: buy milk.' && preview.already_imported == null, preview.messages)

const first = await rpc('session.foreign.import', { id: notes.id })
check('import one', first.already_imported === false && first.session_id, first)
const again = await rpc('session.foreign.import', { id: notes.id })
check('importing it again gives the same session', again.already_imported === true && again.session_id === first.session_id, again)
const all = await rpc('slash.exec', { command: 'import-hermes' })
check('/import-hermes imports the rest', /Imported 2 sessions from Hermes \(2 databases\); 1 already here/.test(all.output), all.output)
check('the list marks them imported', (await rpc('session.foreign.list', { source: null, offset: 0 })).sessions.every(s => s.already_imported))

const resumed = await rpc('session.resume', { session_id: first.session_id })
const shown = resumed.messages.map(m => `${m.role}:${m.text ?? ''}`)
check('the transcript', shown[0] === 'user:what do my notes say?' && shown.at(-1) === 'assistant:Your notes say: buy milk.', shown)
const sessions = await (await fetch(`http://127.0.0.1:${port}/api/profiles/sessions?limit=100`)).json()
const row = sessions.sessions.find(s => s.id === first.session_id)
check('listed with its title and folder', row?.title === 'Read the notes' && /import-folder/.test(row.cwd), row)

// It goes on: the next request carries the imported history, tool call and result paired.
const from = events.length
await rpc('prompt.submit', { session_id: first.session_id, text: 'thanks, anything else?' })
const deadline = Date.now() + 30000
while (!events.slice(from).some(e => e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
const sent = requests.at(-1)?.messages ?? []
const call = sent.find(m => m.tool_calls?.length)
const result = sent.find(m => m.role === 'tool')
check('continues with the imported history', call?.tool_calls[0].id === result?.tool_call_id && result?.content.includes('Buy milk.') && sent.at(-1).content === 'thanks, anything else?', sent.map(m => m.role))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
