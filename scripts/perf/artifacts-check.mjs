// The Artifacts gallery against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1, DevTools on 9223): this script is that provider (stop mock_llm.py
// first). A turn writes a file (write_file) and replies naming its full path and a link; the
// page's Artifacts view (#/artifacts) must list both, and opening the file must reach the backend
// (patches/0013; the file is removed first, so nothing opens on screen). As in Hermes, a
// write_file result itself carries no path the gallery reads: files come from replies, terminal
// output and producer tools.   node scripts/perf/artifacts-check.mjs [port]
import fs from 'node:fs'
import http from 'node:http'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

const stamp = Date.now()
const work = path.resolve('target/perf/artifacts-work')
fs.mkdirSync(work, { recursive: true })
const file = `report-${stamp}.md`
const link = `https://example.com/artifact-${stamp}`

// The provider: the turn's first call writes the file, the next one answers with the link.
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
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Artifacts check' } }] }))
    }
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    const end = reason => { res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: reason }] })}\n\n`); res.end('data: [DONE]\n\n') }
    if (request.messages.at(-1)?.role === 'user') {
      chunk({ tool_calls: [{ index: 0, id: `call_${stamp}`, type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: file, content: '# Report\n' }) } }] })
      return end('tool_calls')
    }
    chunk({ role: 'assistant', content: `Wrote the report to ${path.join(work, file)}. The source is at [the spec](${link}).` })
    end('stop')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.artifacts'])
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

const { session_id: sid } = await rpc('session.create', { cwd: work })
await rpc('session.resume', { session_id: sid })
await rpc('prompt.submit', { session_id: sid, text: `write the report ${stamp}` })
const deadline = Date.now() + 30000
while (!events.some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
check('the turn wrote the file', fs.existsSync(path.join(work, file)))

// The page, through DevTools: open the Artifacts view and read it.
const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith(`http://127.0.0.1:${port}`))
const cdp = new WebSocket(page.webSocketDebuggerUrl)
await new Promise(ok => (cdp.onopen = ok))
let cid = 0
const waiting = new Map()
cdp.onmessage = e => { const m = JSON.parse(e.data); if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id) } }
const evaluate = expression => new Promise(ok => {
  waiting.set(++cid, m => ok(m.result?.result?.value))
  cdp.send(JSON.stringify({ id: cid, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise: true } }))
})
// Leave first: the view loads once when it opens, so it must open after the turn.
await evaluate(`location.hash = '#/'`)
await sleep(500)
await evaluate(`location.hash = '#/artifacts'`)
let text = ''
for (let i = 0; i < 40 && !(text.includes(file) && text.includes(`artifact-${stamp}`)); i++) {
  await sleep(500)
  text = (await evaluate('document.body.innerText')) ?? ''
}
check('the Artifacts view opens', text.length > 0 && !/No such API endpoint/.test(text))
check('it lists the written file', text.includes(file))
check('it lists the link', text.includes(`artifact-${stamp}`))

// Opening a file: the page sends `file:` links to the backend (patches/0013). The file is removed
// first so nothing opens on the desktop: the route answers that it doesn't exist.
fs.rmSync(path.join(work, file), { force: true })
await evaluate(`(() => {
  window.__openFile = []
  const original = window.fetch
  window.fetch = async (input, init) => {
    const url = String(input?.url ?? input)
    const response = await original(input, init)
    if (url.includes('/api/hermes-rs/open-file')) window.__openFile.push({ body: init?.body, status: response.status })
    return response
  }
  const row = [...document.querySelectorAll('button,[role=button],a')].find(e => e.innerText.includes(${JSON.stringify(file)}))
  row?.click()
  return !!row
})()`)
await sleep(1500)
const opened = (await evaluate('JSON.stringify(window.__openFile)')) ?? '[]'
const calls = JSON.parse(opened)
check('opening the file asks the backend', calls.length === 1 && calls[0].body.includes(file), calls)
check('a missing file is refused, nothing opens', calls[0]?.status === 400)
await evaluate(`location.hash = '#/'`)

cdp.close()
ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
