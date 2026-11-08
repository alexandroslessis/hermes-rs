// Arrow-key navigation (patches/0018, 0019) against a running hermes-desktop (the perf
// instance, DevTools on 9223): the file tree's ↑/↓ move the visible selection (arborist's
// own arrows moved only its focus cursor, which no style followed — the keys looked dead)
// and ←/→ fold/unfold the selected folder; the Settings rail's rows walk with ↑/↓ (roving
// focus, wrap-around; the disclosure carets and the icon-only footer are skipped, and
// arrows only move focus — Enter still activates, the hash never changes). Radix menus
// arrow-navigate natively; not checked here.
//   node scripts/perf/keyboard-nav-check.mjs

const devtools = 9223
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const wait = ms => new Promise(ok => setTimeout(ok, ms))

const targets = await (await fetch(`http://127.0.0.1:${devtools}/json`)).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
if (!page) throw new Error('perf page not found on DevTools ' + devtools)
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
ws.onmessage = event => {
  const msg = JSON.parse(event.data)
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id) }
}
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })
const evalJs = async expression => {
  const { result } = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error('page eval failed: ' + JSON.stringify(result.exceptionDetails).slice(0, 300))
  return result.result?.value
}
/** Poll until the expression is truthy (the SPA renders async after a hash change). */
const until = async (expression, timeoutMs = 5000) => {
  const end = Date.now() + timeoutMs
  while (Date.now() < end) {
    const value = await evalJs(expression)
    if (value) return value
    await wait(150)
  }
  return null
}


// ── Settings rail (patch 0019): ↑/↓ walk the nav rows ───────────────────────────
await evalJs(`location.hash = '#/settings'`)
const railRows = await until(
  `[...document.querySelectorAll('[data-tour=overlay-nav] button')].filter(b => b.offsetParent !== null && !b.dataset.tour?.startsWith('nav-toggle-') && !b.closest('[data-rail-footer]')).length >= 2`
)
check('the settings rail has walkable nav rows', railRows === true)
if (railRows) {
  const walk = await evalJs(`(() => {
    const rows = [...document.querySelectorAll('[data-tour=overlay-nav] button')].filter(b => b.offsetParent !== null && !b.dataset.tour?.startsWith('nav-toggle-') && !b.closest('[data-rail-footer]'))
    const labels = () => rows.map(r => r.textContent.trim() || r.getAttribute('aria-label'))
    const focused = () => labels()[rows.indexOf(document.activeElement)]
    rows[0].focus()
    const seq = [focused()]
    let el = rows[0]
    for (let i = 0; i < 3; i++) {
      el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: 'ArrowDown' }))
      el = document.activeElement
      seq.push(focused())
    }
    const hashAfterDown = location.hash
    rows[0].focus()
    rows[0].dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: 'ArrowUp' }))
    return { first: seq[0], second: seq[1], walkMoved: seq[1] !== seq[0] && seq[2] !== seq[1] && seq[3] !== seq[2], hashAfterDown, wrappedTo: focused() }
  })()`)
  check('ArrowDown walks the rail row by row', walk.walkMoved, walk)
  check('arrows only move focus (no navigation)', walk.hashAfterDown.startsWith('#/settings'), walk.hashAfterDown)
  check('ArrowUp wraps from the first row to the last', walk.wrappedTo !== walk.first, walk.wrappedTo)
}

// ── File tree (patch 0018): ↑/↓ move the selection, ←/→ fold ────────────────────
// A reload first: the tree's arborist state (its focus cursor) drifts when the
// page is driven by synthetic keydowns with no real DOM focus — real usage keeps
// focus in the tree (clicks focus it, arrows keep it), and a fresh page is what
// the user's first interaction sees.
await evalJs(`location.hash = '#/'; location.reload()`)
const treeRows = await until(
  `document.querySelectorAll('[data-project-tree] [role=treeitem]').length >= 2`,
  15000
)
check('a file tree is visible (the default Files pane)', treeRows === true)
if (treeRows) {
  // One pass, no clicks: keydowns are dispatched on the selected row (a
  // descendant, so arborist's own container handler sees ←/→), and every step
  // polls for its effect instead of sleeping — the tree re-renders and even
  // remounts under the page's own traffic, so a step can race a re-render.
  // The pass retries: the feature is the walk, not the page's traffic.
  const treePassJs = `(async () => {
    const q = () => [...document.querySelectorAll('[data-project-tree] [role=treeitem]')]
    const wait = ms => new Promise(ok => setTimeout(ok, ms))
    const until = async (fn, ms = 2500) => {
      const end = Date.now() + ms
      let value
      while (Date.now() < end) { value = fn(); if (value !== null && value !== undefined) return value; await wait(100) }
      return null
    }
    const selected = () => q().find(r => r.getAttribute('aria-selected') === 'true')
    const indexOfSel = () => { const el = selected(); return el ? q().indexOf(el) : null }
    // Keydowns on the selected row; when nothing is selected yet, on the first row.
    const press = keyName => {
      const el = selected() ?? q()[0]
      el?.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: keyName }))
    }
    press('ArrowDown')
    // Each poll waits for the index to CHANGE: the pre-press value is non-null
    // the moment the poll starts, so a plain truthiness wait reads it stale.
    const preFirst = indexOfSel()
    const first = await until(() => { const i = indexOfSel(); return i !== null && i !== preFirst ? i : null })
    press('ArrowDown')
    const second = await until(() => { const i = indexOfSel(); return i !== null && i !== first ? i : null })
    press('ArrowUp')
    const back = await until(() => { const i = indexOfSel(); return i !== null && i !== second ? i : null })
    const walkDown = first !== null && second !== null
    const walkUp = back !== null
    // Fold/unfold on the selected row. A file row has no aria-expanded — skip.
    const row = selected()
    const expanded = row?.getAttribute('aria-expanded')
    let opened = null
    let closed = null
    if (expanded !== null && expanded !== undefined) {
      if (expanded === 'true') {
        press('ArrowLeft')
        await until(() => { const el = selected(); return el && el.getAttribute('aria-expanded') === 'false' ? true : null })
      }
      press('ArrowRight')
      opened = await until(() => { const el = selected(); return el && el.getAttribute('aria-expanded') === 'true' ? true : null })
      press('ArrowLeft')
      closed = await until(() => { const el = selected(); return el && el.getAttribute('aria-expanded') === 'false' ? true : null })
    }
    return { first, second, back, walkDown, walkUp, isFolder: expanded !== null && expanded !== undefined, opened: opened === true, closed: closed === true }
  })()`
  const tree = await evalJs(treePassJs)
  check('ArrowDown moves the selection down the tree', tree.walkDown, tree)
  check('ArrowUp moves it back', tree.walkUp, tree)
  if (tree.isFolder) {
    check('ArrowRight opens the selected folder', tree.opened, tree)
    check('ArrowLeft closes it again', tree.closed, tree)
  } else {
    console.log('skip folder fold/unfold (the selected row is a file)')
  }
}

ws.close()
console.log(failed ? `${failed} FAILED` : 'all ok')
process.exit(failed ? 1 : 0)
