// Composer attachments against a running hermes-desktop (the perf instance, port 47810, whose
// provider is http://127.0.0.1:47900/v1): this script is that provider itself (stop mock_llm.py
// first), so it sees what reaches the model.
//   node scripts/perf/attach-check.mjs [port] [session folder]
// Checks: image upload (data URL) and read-back, an image sent with a prompt (`attachments`), an
// image held by `image.attach_bytes` / dropped by `image.detach`, a multipart file upload and
// `file.attach`, whose `@file:` reference must bring the file's text to the model.
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'attach-check-'))
const base = `http://127.0.0.1:${port}`
// A 1×1 PNG.
const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=='

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}

// The provider: lists one model, answers every chat with a few words, keeps what it was sent.
const seen = []
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model', architecture: { input_modalities: ['text', 'image'] } }] }))
    }
    const request = JSON.parse(body || '{}')
    if (!request.stream) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    seen.push(request)
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = (delta, finish = null) => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`)
    chunk({ role: 'assistant', content: 'Got it.' })
    chunk({}, 'stop')
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const rest = async (method, url, body) => {
  const r = await fetch(base + url, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.attach'])
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
const sleep = ms => new Promise(r => setTimeout(r, ms))
// Sends a prompt and waits for its answer: what the model was sent last.
const turn = async (sid, params) => {
  const from = events.length
  const asked = seen.length
  await rpc('prompt.submit', { session_id: sid, ...params })
  const deadline = Date.now() + 30000
  while (!events.slice(from).some(e => e.session_id === sid && e.type === 'message.complete') && Date.now() < deadline) await sleep(100)
  return seen[asked] ?? null
}
const lastUser = request => [...(request?.messages ?? [])].reverse().find(m => m.role === 'user')?.content
const imageParts = content => (Array.isArray(content) ? content.filter(p => p.type === 'image_url') : [])
const text = content => (Array.isArray(content) ? content.filter(p => p.type === 'text').map(p => p.text).join('\n') : String(content ?? ''))

const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })

// 1. An image, uploaded then sent with the prompt.
const up = await rest('POST', '/api/chat/image-upload', { data_url: `data:image/png;base64,${PNG}`, filename: 'shot.png' })
check('image upload', up.status === 200 && up.body.mime_type === 'image/png' && fs.existsSync(up.body.path), up.body)
const back = await rest('GET', `/api/fs/read-data-url?path=${encodeURIComponent(up.body.path)}`)
check('read it back as a data URL', back.body?.dataUrl === `data:image/png;base64,${PNG}`, back.body?.dataUrl?.slice(0, 40))
const refused = await rest('POST', '/api/chat/image-upload', { data_url: 'data:text/plain;base64,aGk=' })
check('refuse a non-image', refused.status === 400, refused.body)
let sent = await turn(sid, { text: 'what is in this picture?', attachments: [{ path: up.body.path, mime: 'image/png' }] })
check('the model gets the image', imageParts(lastUser(sent)).length === 1 && text(lastUser(sent)).includes('what is in this picture'), lastUser(sent))

// 2. Held images (`image.attach_bytes`), one dropped again (`image.detach`).
const held = await rpc('image.attach_bytes', { session_id: sid, content_base64: PNG, filename: 'held.png' })
const dropped = await rpc('image.attach_bytes', { session_id: sid, content_base64: PNG, filename: 'dropped.png' })
const detached = await rpc('image.detach', { session_id: sid, path: dropped.path })
check('attach_bytes, detach', held.attached && held.count === 1 && dropped.count === 2 && detached.detached && detached.count === 1, { held, detached })
sent = await turn(sid, { text: 'and this one?' })
check('the held image goes with the next prompt', imageParts(lastUser(sent)).length === 1, lastUser(sent))
sent = await turn(sid, { text: 'no picture now' })
check('and only that one', imageParts(lastUser(sent)).length === 0, lastUser(sent))

// 3. A file: multipart upload, `file.attach`, the `@file:` reference in the prompt.
const form = new FormData()
form.append('file', new Blob(['hello from the attachment\n'], { type: 'text/plain' }), 'my notes.txt')
const staged = await (await fetch(`${base}/api/chat/file-upload`, { method: 'POST', body: form })).json()
check('file upload', staged.ok && staged.staged_upload?.path === staged.path, staged)
const attached = await rpc('file.attach', { session_id: sid, name: 'my notes.txt', staged_upload: staged.staged_upload })
check('file.attach', attached.attached && attached.ref_text === '@file:`.hermes/attachments/my notes.txt`', attached)
check('copied into the session folder', fs.existsSync(path.join(folder, '.hermes', 'attachments', 'my notes.txt')))
sent = await turn(sid, { text: `summarize ${attached.ref_text}` })
check('the model gets the file text', text(lastUser(sent)).includes('hello from the attachment'), text(lastUser(sent)).slice(0, 300))

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
