// The side panel's file routes against a running hermes-desktop (the perf instance):
// read-text (relative to the session's folder), write-text, mkdir, git-root, a ticketed download
// and a ranged media stream, and the credential guard.
//   node scripts/perf/files-check.mjs [port] [empty folder]
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execSync } from 'node:child_process'

const port = process.argv[2] ?? '47810'
const folder = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'files-check-'))
const base = `http://127.0.0.1:${port}`
fs.mkdirSync(path.join(folder, 'src'), { recursive: true })
fs.writeFileSync(path.join(folder, 'src', 'main.rs'), 'fn main() {}\n')
fs.writeFileSync(path.join(folder, '.env'), 'SECRET=1\n')
fs.writeFileSync(path.join(folder, 'clip.mp3'), Buffer.alloc(4096, 7))
// The git-root check expects a repository around the folder; bare tempdirs
// answered null and failed it since the check was written. Identity set
// explicitly (the machine's global git config is nobody's business here).
execSync(`git -C "${folder}" init -q && git -C "${folder}" config user.name check && git -C "${folder}" config user.email check@local`, { stdio: 'ignore' })

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const rest = async (method, url, body) => {
  const r = await fetch(base + url, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
const q = params => new URLSearchParams(params).toString()

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.files'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
const sid = await new Promise(ok => {
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id === 1) ok(m.result.session_id) }
  ws.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'session.create', params: { cwd: folder } }))
})
ws.close()

const read = await rest('GET', `/api/fs/read-text?${q({ path: 'src/main.rs', session_id: sid })}`)
check('read-text, relative to the session', read.status === 200 && read.body.text === 'fn main() {}\n' && read.body.language === 'rust', read.body)
const saved = await rest('POST', '/api/fs/write-text', { path: 'src/main.rs', content: 'fn main() { todo!() }\n', session_id: sid })
check('write-text', saved.status === 200 && fs.readFileSync(path.join(folder, 'src', 'main.rs'), 'utf8') === 'fn main() { todo!() }\n', saved.body)
const secret = await rest('GET', `/api/fs/read-text?${q({ path: '.env', session_id: sid })}`)
check('credentials refused', secret.status === 403, secret.body)
const made = await rest('POST', '/api/files/mkdir', { path: path.join(folder, 'new', 'deeper') })
check('mkdir', made.status === 200 && fs.existsSync(path.join(folder, 'new', 'deeper')), made.body)
const root = await rest('GET', `/api/fs/git-root?${q({ path: folder })}`)
check('git-root (the repository around it)', root.status === 200 && typeof root.body.root === 'string' && folder.toLowerCase().startsWith(root.body.root.toLowerCase()), root.body)

const ticket = (await rest('POST', '/api/files/ticket', { path: 'src/main.rs', session_id: sid, route: 'download' })).body.ticket
const dl = await fetch(`${base}/api/fs/download?${q({ path: 'src/main.rs', session_id: sid, ticket })}`)
check('ticketed download', dl.status === 200 && (dl.headers.get('content-disposition') ?? '').startsWith('attachment') && (await dl.text()).includes('todo!'), dl.headers.get('content-disposition'))
const wrong = await fetch(`${base}/api/fs/download?${q({ path: '.env', session_id: sid, ticket })}`)
check('a ticket opens only its file', wrong.status === 403)
const head = await fetch(`${base}/api/fs/download?${q({ path: 'src/main.rs', session_id: sid })}`, { method: 'HEAD' })
check('HEAD (the page checks before downloading)', head.status === 200)

const media = await fetch(`${base}/api/files/stream?${q({ path: path.join(folder, 'clip.mp3') })}`, { headers: { Range: 'bytes=100-199' } })
check('ranged media stream', media.status === 206 && media.headers.get('content-type') === 'audio/mpeg' && (await media.arrayBuffer()).byteLength === 100 && (media.headers.get('content-disposition') ?? '').startsWith('inline'), [media.status, media.headers.get('content-type')])

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
