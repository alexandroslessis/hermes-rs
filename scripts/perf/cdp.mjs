// Runs JS in MY perf instance's page through WebView2's DevTools protocol (port 9223) and prints
// the result as JSON.   node cdp.mjs '<expression>'     (async expressions are awaited)
//                       node cdp.mjs --metrics          (Performance.getMetrics)
const port = 9223
const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
if (!page) throw new Error('page not found: ' + JSON.stringify(targets.map(t => t.url)))
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
ws.onmessage = event => {
  const msg = JSON.parse(event.data)
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id) }
}
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })

const arg = process.argv[2]
if (arg === '--metrics') {
  await call('Performance.enable')
  const { result } = await call('Performance.getMetrics')
  const pick = Object.fromEntries(result.metrics.map(m => [m.name, m.value]))
  console.log(JSON.stringify({
    jsHeapUsedMB: +(pick.JSHeapUsedSize / 1048576).toFixed(1),
    jsHeapTotalMB: +(pick.JSHeapTotalSize / 1048576).toFixed(1),
    nodes: pick.Nodes, documents: pick.Documents, listeners: pick.JSEventListeners,
    layoutCount: pick.LayoutCount, recalcStyleCount: pick.RecalcStyleCount,
    scriptDurationS: +pick.ScriptDuration.toFixed(2), layoutDurationS: +pick.LayoutDuration.toFixed(2),
    recalcStyleDurationS: +pick.RecalcStyleDuration.toFixed(2), taskDurationS: +pick.TaskDuration.toFixed(2)
  }))
} else {
  const { result } = await call('Runtime.evaluate', { expression: arg, awaitPromise: true, returnByValue: true })
  console.log(JSON.stringify(result.result?.value ?? result.exceptionDetails ?? result, null, 1))
}
ws.close()
