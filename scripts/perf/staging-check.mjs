// The staging area (Phase 6) against a running hermes-desktop (the perf instance; no model calls):
// seeds items in its data folder's `staging/` the way a tainted session leaves them (a memory add,
// a skill create, a stale memory patch, a skill whose name was taken meanwhile, and a hostile
// memory entry with an ANSI escape, a right-to-left override and HTML in it), then drives the
// endpoints the Memory and Skills screens call: `GET /api/staging`, `POST /api/staging/<id>/apply`
// and `/reject`, and the same list over the gateway (`staging.list`).
// Checks: the list's shape (no payload, a reason, sources, stale + error), hidden characters shown
// as <U+XXXX> and never raw, Apply writes MEMORY.md / the skill and removes the item, a second
// Apply is a 404, a stale or refused Apply is a 409 that keeps the item with its error, Reject
// removes it, unknown ids are 404.
//   node scripts/perf/staging-check.mjs [port] [data folder] [--dom]
// `--dom` also drives the page over DevTools (9223) and needs a webapp built with patches
// 0024-0026: the "Waiting for your review" section is the first thing on Settings > Memory and at
// the top of Capabilities > Skills, its diff is text (the HTML in the hostile item is shown, not
// run), a stale item offers Reject only. The REST part runs without it.
import fs from 'node:fs'
import path from 'node:path'

const args = process.argv.slice(2).filter(a => !a.startsWith('--'))
const dom = process.argv.includes('--dom')
const port = args[0] ?? '47810'
const home = args[1] ?? path.resolve('target/perf/home')
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))
const rest = async (method, url) => {
  const r = await fetch(base + url, { method, headers: { 'Content-Type': 'application/json' }, body: method === 'POST' ? '{}' : undefined })
  return { status: r.status, body: await r.json().catch(() => null) }
}

const stagingDir = path.join(home, 'staging')
const memories = path.join(home, 'memories')
const skills = path.join(home, 'skills')
fs.mkdirSync(stagingDir, { recursive: true })
fs.mkdirSync(memories, { recursive: true })
fs.mkdirSync(skills, { recursive: true })
const memoryFile = path.join(memories, 'MEMORY.md')
const savedMemory = fs.existsSync(memoryFile) ? fs.readFileSync(memoryFile) : null
const readMemory = () => (fs.existsSync(memoryFile) ? fs.readFileSync(memoryFile, 'utf8') : '')

const ids = { memory: 'c0ffee000001', skill: 'c0ffee000002', stale: 'c0ffee000003', taken: 'c0ffee000004', hostile: 'c0ffee000005' }
const SKILL_BODY = '---\nname: staging-check-skill\ndescription: Use when checking the staging area. A seeded skill.\n---\n# Staging check\nNothing to do.\n'
const TAKEN_BODY = '---\nname: staging-check-taken\ndescription: Use when checking the staging area. Taken name.\n---\n# Taken\n'
const HOSTILE = 'looks fine ‮.exe \u001b[31m <img src=x onerror="window.__pwned=1"> <script>window.__pwned=1</script>'
const created = Date.now()
const seed = (id, item) => fs.writeFileSync(path.join(stagingDir, `${id}.json`), JSON.stringify({ v: 1, id, created, session_id: 'stagingcheck', status: 'pending', ...item }))
const WEB = ['web search results']
seed(ids.memory, {
  kind: 'memory', op: { target: 'memory', operations: [{ action: 'add', content: 'Staging check: the user likes tea.' }] },
  diff: 'MEMORY.md\n+ Staging check: the user likes tea.\n', reason: 'this turn read web search results', sources: WEB
})
seed(ids.skill, {
  kind: 'skill', op: { operations: [{ action: 'create', name: 'staging-check-skill', content: SKILL_BODY }], origin: 'foreground', bases: [{ skill: 'staging-check-skill', file: 'SKILL.md', fingerprint: null }] },
  diff: '--- /dev/null\n+++ staging-check-skill/SKILL.md\n' + SKILL_BODY.split('\n').map(l => '+' + l).join('\n'), reason: 'this turn read a web page', sources: ['a web page: https://example.com']
})
seed(ids.stale, {
  kind: 'memory', op: { target: 'memory', operations: [{ action: 'remove', old_text: 'gone entry', matched_entry: 'gone entry' }] },
  diff: 'MEMORY.md\n- gone entry\n', reason: 'this turn read web search results', sources: WEB, status: 'stale',
  error: 'Entry changed since it was staged: it no longer matches what you were shown.'
})
const takenDir = path.join(skills, 'staging-check-taken')
fs.mkdirSync(takenDir, { recursive: true })
fs.writeFileSync(path.join(takenDir, 'SKILL.md'), TAKEN_BODY)
seed(ids.taken, {
  kind: 'skill', op: { operations: [{ action: 'create', name: 'staging-check-taken', content: TAKEN_BODY.replace('Taken name.', 'Other text.') }], origin: 'foreground', bases: [{ skill: 'staging-check-taken', file: 'SKILL.md', fingerprint: null }] },
  diff: '--- /dev/null\n+++ staging-check-taken/SKILL.md\n+# Taken', reason: 'this turn read a web page', sources: ['a web page: https://example.com']
})
seed(ids.hostile, {
  kind: 'memory', op: { target: 'memory', operations: [{ action: 'add', content: HOSTILE }] },
  diff: `MEMORY.md\n+ ${HOSTILE}\n`, reason: 'this turn read results from MCP server ‮evil', sources: ['results from MCP server ‮evil']
})

const cleanup = () => {
  for (const id of Object.values(ids)) fs.rmSync(path.join(stagingDir, `${id}.json`), { force: true })
  fs.rmSync(takenDir, { recursive: true, force: true })
  for (const dir of [path.join(skills, 'staging-check-skill'), ...fs.readdirSync(skills, { withFileTypes: true }).filter(d => d.isDirectory()).map(d => path.join(skills, d.name, 'staging-check-skill'))]) fs.rmSync(dir, { recursive: true, force: true })
  if (savedMemory === null) fs.rmSync(memoryFile, { force: true })
  else fs.writeFileSync(memoryFile, savedMemory)
}

try {
  const listed = async () => (await rest('GET', '/api/staging')).body?.items ?? []
  const find = async id => (await listed()).find(i => i.id === id)

  // The list.
  const first = await rest('GET', '/api/staging')
  check('GET /api/staging answers 200 with items and a count', first.status === 200 && Array.isArray(first.body?.items) && first.body.count === first.body.items.length, first.body && { count: first.body.count })
  for (const id of Object.values(ids)) check(`seeded item ${id} is listed`, first.body?.items.some(i => i.id === id))
  const memory = await find(ids.memory)
  check('an item has the screen\'s fields, no payload', memory && ['id', 'kind', 'summary', 'diff', 'reason', 'session_id', 'sources', 'created', 'status', 'error'].every(k => k in memory) && !('op' in memory), memory)
  check('the reason and the sources are there', memory?.reason === 'this turn read web search results' && memory.sources[0] === 'web search results' && memory.kind === 'memory' && memory.status === 'pending' && memory.error === null)
  const stale = await find(ids.stale)
  check('a stale item says so and carries its error', stale?.status === 'stale' && /changed since/.test(stale.error ?? ''), stale)
  const hostile = await find(ids.hostile)
  const raw = JSON.stringify(hostile)
  check('hidden characters are shown as <U+XXXX>', hostile?.diff.includes('<U+202E>') && hostile.diff.includes('<U+001B>') && hostile.reason.includes('<U+202E>') && hostile.sources[0].includes('<U+202E>'), hostile?.diff)
  check('and none arrives raw', !/[‮\u001b]/.test(raw), raw.length)
  check('HTML in a diff arrives as text for the page to show', hostile?.diff.includes('<img src=x onerror="window.__pwned=1">') && hostile.diff.includes('<script>'))
  const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.staging'])
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
  const viaRpc = await new Promise(ok => {
    ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id === 1) ok(m) }
    ws.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'staging.list', params: {} }))
  })
  ws.close()
  check('staging.list over the gateway lists the same items', viaRpc.result?.count === (await listed()).length, viaRpc.result?.count)

  // Apply a memory add and a skill create: written, item gone, a second Apply is a 404.
  const before = readMemory()
  const applied = await rest('POST', `/api/staging/${ids.memory}/apply`)
  check('Apply of a memory item answers 200', applied.status === 200 && applied.body?.applied === true && applied.body.kind === 'memory', applied.body)
  check('MEMORY.md has the entry', readMemory().includes('Staging check: the user likes tea.') && readMemory().startsWith(before.trimEnd()), readMemory())
  check('the item is gone from the list', (await find(ids.memory)) === undefined)
  check('a second Apply is a 404 with a sentence', (await rest('POST', `/api/staging/${ids.memory}/apply`)).status === 404)
  const appliedSkill = await rest('POST', `/api/staging/${ids.skill}/apply`)
  check('Apply of a skill item answers 200', appliedSkill.status === 200 && appliedSkill.body?.kind === 'skill', appliedSkill.body)
  const skillList = await rest('GET', '/api/skills')
  check('the new skill is in the skills list', JSON.stringify(skillList.body).includes('staging-check-skill'))
  check('and its item is gone', (await find(ids.skill)) === undefined)

  // Stale: Apply is refused, the item stays with its error; Reject removes it.
  const refused = await rest('POST', `/api/staging/${ids.stale}/apply`)
  check('Apply of a stale item is a 409 with a reason', refused.status === 409 && typeof refused.body?.detail === 'string' && refused.body.detail.length > 10, refused.body)
  check('it stays listed, still stale, with an error', (await find(ids.stale))?.status === 'stale' && Boolean((await find(ids.stale)).error))
  const taken = await rest('POST', `/api/staging/${ids.taken}/apply`)
  check('a skill whose name was taken is refused (409), nothing overwritten', taken.status === 409 && fs.readFileSync(path.join(takenDir, 'SKILL.md'), 'utf8') === TAKEN_BODY, taken.body)
  check('and it is listed as stale with the error', (await find(ids.taken))?.status === 'stale' && Boolean((await find(ids.taken))?.error), await find(ids.taken))
  const hostileApply = await rest('POST', `/api/staging/${ids.hostile}/apply`)
  check('a hostile entry is refused again at Apply (threat scan), the item stays', hostileApply.status === 409 && (await find(ids.hostile)) !== undefined && !readMemory().includes('onerror'), hostileApply.body)

  if (dom) await domCheck()

  for (const id of dom ? [ids.stale, ids.hostile] : [ids.stale, ids.taken, ids.hostile]) {
    const rejected = await rest('POST', `/api/staging/${id}/reject`)
    check(`Reject ${id} answers 200`, rejected.status === 200 && rejected.body?.rejected === true, rejected.body)
    check('and removes it', (await find(id)) === undefined)
  }
  check('Reject of a gone item is a 404', (await rest('POST', `/api/staging/${ids.stale}/reject`)).status === 404)
  for (const bad of ['..', 'CON', 'x'.repeat(40), 'ZZZZZZZZ']) check(`an id that can't be an item's (${bad.slice(0, 12)}) is a 404`, (await rest('POST', `/api/staging/${encodeURIComponent(bad)}/apply`)).status === 404)
  check('MEMORY.md never got the hostile text', !readMemory().includes('onerror'))

  async function domCheck() {
    const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
    const page = targets.find(t => t.type === 'page' && t.url.startsWith(base))
    if (!page) return check('the page is open on DevTools 9223', false)
    const cdp = new WebSocket(page.webSocketDebuggerUrl)
    await new Promise(ok => (cdp.onopen = ok))
    let cid = 0
    const waiting = new Map()
    cdp.onmessage = e => { const m = JSON.parse(e.data); if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id) } }
    const evaluate = expression => new Promise(ok => {
      waiting.set(++cid, m => ok(m.result?.result?.value))
      cdp.send(JSON.stringify({ id: cid, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise: true } }))
    })
    const until = async (expression, ms = 15000) => {
      for (let t = 0; t < ms; t += 400) { const v = await evaluate(expression); if (v) return v; await sleep(400) }
      return null
    }
    // Memory: Settings > Memory & Context. The section comes before the layers panel.
    await evaluate(`location.hash = '#/'`)
    await sleep(400)
    await evaluate(`location.hash = '#/settings?tab=config:memory'`)
    const memorySection = await until(`!!document.querySelector('[data-testid="staging-pending-memory"]')`)
    check('Settings > Memory shows "Waiting for your review"', Boolean(memorySection))
    check('it comes before the memory layers panel', await evaluate(`(() => { const a = document.querySelector('[data-testid="staging-pending-memory"]'), b = document.querySelector('[data-testid="memory-layers"]'); return !!a && !!b && Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) })()`))
    check('it holds the hostile diff as text, not elements', await evaluate(`(() => { const s = document.querySelector('[data-testid="staging-pending-memory"]'); return s.textContent.includes('<img src=x') && s.textContent.includes('<U+202E>') && !s.querySelector('img,script,iframe') && window.__pwned === undefined })()`))
    check('the stale item offers Reject only', await evaluate(`(() => { const li = document.querySelector('[data-staging-id="${ids.stale}"]'); const b = [...li.querySelectorAll('button')].map(x => x.textContent.trim()); return b.length === 1 && b[0] === 'Reject' && /Out of date/.test(li.textContent) })()`))
    check('a pending item offers Apply and Reject', await evaluate(`(() => { const li = document.querySelector('[data-staging-id="${ids.hostile}"]'); return [...li.querySelectorAll('button')].map(x => x.textContent.trim()).join() === 'Apply,Reject' })()`))
    // Apply on the hostile item: refused, the message shows on the card and the card stays.
    await evaluate(`[...document.querySelector('[data-staging-id="${ids.hostile}"]').querySelectorAll('button')].find(b => b.textContent.trim() === 'Apply').click()`)
    // The click's own failure takes the card's alert over from the stored error, so once the
    // buttons are enabled again the alert must be the 409's `detail` sentence, unwrapped (a raw
    // `409: {"detail": ...}` means the page's client error wasn't parsed).
    await sleep(800)
    const alert = await until(`(() => { const li = document.querySelector('[data-staging-id="${ids.hostile}"]'); const a = li && li.querySelector('[role="alert"]'); return a && !li.querySelector('button[disabled]') && a.textContent })()`)
    check('a refused Apply shows the server\'s sentence on the card and keeps it', typeof alert === 'string' && alert.startsWith('Could not apply this change') && !alert.includes('"detail"'), alert)
    // Skills: Capabilities > Skills.
    await evaluate(`location.hash = '#/'`)
    await sleep(400)
    await evaluate(`location.hash = '#/capabilities?tab=skills'`)
    check('Capabilities > Skills shows "Waiting for your review" with the skill item', Boolean(await until(`!!document.querySelector('[data-testid="staging-pending-skill"] [data-staging-id="${ids.taken}"]')`)))
    check('and no memory item among them', await evaluate(`!document.querySelector('[data-testid="staging-pending-skill"] [data-staging-id="${ids.hostile}"]')`))
    // Reject from the page; the card goes.
    await evaluate(`[...document.querySelector('[data-staging-id="${ids.taken}"]').querySelectorAll('button')].find(b => b.textContent.trim() === 'Reject').click()`)
    check('Reject in the page removes the card', Boolean(await until(`!document.querySelector('[data-staging-id="${ids.taken}"]')`)))
    cdp.close()
  }
} finally {
  cleanup()
}
console.log(failed ? `\n${failed} failed` : '\nall passed')
process.exit(failed ? 1 : 0)
