// Move to project against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). Creates a
// session, moves it to a git folder and to a plain folder (the receipt, the stored cwd and the
// git identity the page's cache reads back), checks the unknown-folder refusal, and the busy
// refusal while a turn runs.   node scripts/perf/workspace-check.mjs [port]
import http from 'node:http'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, realpathSync } from 'node:fs'
const real = p => realpathSync.native(p)
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

// The provider: a plain text reply, delayed when a prompt asks the turn to stay running (the
// busy check needs a window where the session's turn is live).
let slowMs = 0
const provider = http.createServer((req, res) => {
  let body = ''
  req.on('data', c => (body += c))
  req.on('end', async () => {
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock', object: 'model' }] }))
    }
    const request = JSON.parse(body || '{}')
    if (!request.stream) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      return res.end(JSON.stringify({ choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'Mock title' } }] }))
    }
    if (slowMs > 0) await sleep(slowMs)
    res.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const chunk = delta => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    chunk({ role: 'assistant', content: 'Done.' })
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`)
    res.end('data: [DONE]\n\n')
  })
})
await new Promise((ok, fail) => provider.listen(47900, '127.0.0.1', ok).on('error', fail))

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.workspace'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method === 'event') events.push(m.params)
}
// The page's client keeps the code and the typed reason on the error (JsonRpcGatewayError).
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(Object.assign(new Error(m.error.message), { code: m.error.code, data: m.error.data })) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})

// The page's canonical protocol maps `session.workspace.move` to this mutation
// (canonical-protocol.ts), as `session.mutate` with `session_id`, `operation` and `payload` —
// the same call the composer's folder picker on an open chat makes.
const move = (sid, cwd) => rpc('session.mutate', { session_id: sid, operation: 'workspace', payload: { cwd } })

// The backend publishes a `session.info` event on a move; the page repaints its workspace from
// it. The latest one for `sid` is the current state.
const latestInfo = sid => [...events].reverse().find(e => e.type === 'session.info' && e.session_id === sid)?.payload

// The sidebar's list (PaginatedSessions) — the rows the page's cache is rebuilt from.
const listed = async sid => (await rest('GET', '/api/profiles/sessions')).body?.sessions?.find(s => s.id === sid)

// A git folder and a plain folder, made fresh (no machine assumptions). The backend stores the
// real location, so `tmpdir()`'s 8.3 short spelling is resolved to the long one.
const gitDir = mkdtempSync(join(tmpdir(), 'hermes-ws-git-'))
execFileSync('git', ['init', '-b', 'feature', gitDir], { stdio: 'pipe' })
const plainDir = mkdtempSync(join(tmpdir(), 'hermes-ws-plain-'))
const gitReal = real(gitDir)
const plainReal = real(plainDir)

const { session_id: sid } = await rpc('session.create', {})
await rpc('session.resume', { session_id: sid })
check('session created', Boolean(sid))

const toGit = await move(sid, gitDir).catch(e => { check('move to a git folder', false, e.message); return null })
check('move to a git folder', toGit !== null)
// The stored cwd keeps Windows' native separators and the real folder spelling (Hermes's
// `abspath`); the repo root is normalized to git's forward slashes so it compares equal to the
// page's cache.
check('the receipt names the new cwd', toGit?.cwd === gitReal, toGit?.cwd)
check('the receipt carries the branch', toGit?.branch === 'feature', toGit)
check('the receipt carries the repo root', typeof toGit?.git_repo_root === 'string' && toGit.git_repo_root.includes('hermes-ws-git-'), toGit)
check('the receipt is a mutation receipt', Boolean(toGit?.session_id) && typeof toGit?.revision === 'number' && toGit?.execution_generation === 0, toGit)

const gitInfo = latestInfo(sid)
check('session.info shows the cwd', gitInfo?.cwd === gitReal, gitInfo?.cwd)
check('session.info shows the git identity', gitInfo?.git_branch === 'feature' && (gitInfo?.git_repo_root ?? '').includes('hermes-ws-git-'), { branch: gitInfo?.git_branch, root: gitInfo?.git_repo_root })
const gitRow = await listed(sid)
check('the list keeps the identity', gitRow?.cwd === gitReal && gitRow?.git_branch === 'feature', gitRow && { cwd: gitRow.cwd, branch: gitRow.git_branch })

const toPlain = await move(sid, plainDir).catch(e => { check('move to a plain folder', false, e.message); return null })
check('move to a plain folder', toPlain !== null)
check('a non-repo folder answers a null identity', toPlain?.branch === null && toPlain?.git_repo_root === null, toPlain)
const plainInfo = latestInfo(sid)
check('the identity is replaced, not merged', plainInfo?.cwd === plainReal && (plainInfo?.git_branch ?? null) === null, { cwd: plainInfo?.cwd, branch: plainInfo?.git_branch })

const missing = await move(sid, join(plainDir, 'no-such-folder')).then(() => null, e => e)
check('an unknown folder is refused', missing?.code === -32602, missing?.message)

// The busy refusal: the page refuses a move while the session's turn runs.
slowMs = 4000
const submitted = rpc('prompt.submit', { session_id: sid, text: 'keep the turn running for a moment' }).catch(() => null)
await sleep(900)
const busy = await move(sid, gitDir).then(() => null, e => e)
check('a move while the turn runs is refused', busy !== null && busy?.data?.reason === 'session_busy', busy && { message: busy.message, data: busy.data })
slowMs = 0
await submitted
const deadline = Date.now() + 15000
while (!events.some(e => e.session_id === sid && (e.type === 'message.complete' || e.type === 'turn.complete')) && Date.now() < deadline) await sleep(100)
const after = await move(sid, gitDir).catch(e => { check('the move runs once the turn is done', false, e.message); return null })
check('the move runs once the turn is done', after?.branch === 'feature', after)

ws.close()
provider.close()
console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
