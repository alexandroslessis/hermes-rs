// Moves the mouse onto an element of MY perf instance's page (CDP Input, port 9223) and reports the
// floating elements (popovers, tooltips, menus) that appear, with their boxes: for hover and
// placement bugs without screenshots.   node hover.mjs '<css selector or aria-label or text>' [click]
const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise(ok => (ws.onopen = ok))
let id = 0; const pending = new Map()
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } }
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })
const ev = async expr => (await call('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true })).result.result.value
const [sel, action] = process.argv.slice(2)
const rect = await ev(`(() => { const s = ${JSON.stringify(sel)}; let el; try { el = document.querySelector(s) } catch {} ; if (!el) el = [...document.querySelectorAll('button,[role=button],a,div,span')].find(e => e.getAttribute('aria-label') === s || e.textContent.trim() === s); if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.x + r.width/2, y: r.y + r.height/2, w: r.width, h: r.height, tag: el.outerHTML.slice(0, 200) } })()`)
console.log('target', JSON.stringify(rect))
if (!rect) process.exit(1)
await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x, y: rect.y })
if (action === 'click') {
  await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button: 'left', clickCount: 1 })
  await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button: 'left', clickCount: 1 })
}
await new Promise(r => setTimeout(r, 1200))
console.log(JSON.stringify(await ev(`[...document.querySelectorAll('[data-radix-popper-content-wrapper],[data-floating-ui-portal] > *,[role=tooltip],[role=dialog],[role=menu],[data-side]')].map(e => { const r = e.getBoundingClientRect(); return { tag: e.tagName, role: e.getAttribute('role'), side: e.getAttribute('data-side'), style: (e.getAttribute('style')||'').slice(0, 300), rect: [r.x, r.y, r.width, r.height].map(Math.round), text: e.textContent.slice(0, 60) } })`), null, 1))
ws.close()
