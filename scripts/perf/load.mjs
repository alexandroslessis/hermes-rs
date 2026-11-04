// Page load timing of MY perf instance: reloads the page (cold: browser cache and code cache
// cleared first) with an observer injected before the page's scripts, and reports when the
// composer and the session list appear, first paint, and long tasks during load.
//   node load.mjs [cold|warm] [runs]
const mode = process.argv[2] ?? 'warm'
const runs = +(process.argv[3] ?? 3)
const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method) events.push(m)
}
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })
const sleep = ms => new Promise(r => setTimeout(r, ms))

await call('Page.enable')
await call('Network.enable')
await call('Page.addScriptToEvaluateOnNewDocument', { source: `
  window.__perf = { longTasks: [], marks: {} };
  try { new PerformanceObserver(l => { for (const e of l.getEntries()) window.__perf.longTasks.push([Math.round(e.startTime), Math.round(e.duration)]) }).observe({ type: 'longtask', buffered: true }) } catch {}
  const mark = (name, test) => { if (!window.__perf.marks[name] && test()) window.__perf.marks[name] = Math.round(performance.now()) };
  const check = () => {
    mark('composer', () => document.querySelector('[contenteditable="true"], textarea'));
    mark('sessionList', () => document.querySelector('[data-tour="sidebar"] a, nav a[href*="session"], [data-session-id]'));
  };
  new MutationObserver(check).observe(document, { subtree: true, childList: true });
` })

const results = []
for (let i = 0; i < runs; i++) {
  if (mode === 'cold') {
    await call('Network.clearBrowserCache')
    await call('Storage.clearDataForOrigin', { origin: 'http://127.0.0.1:47810', storageTypes: 'cache_storage,service_workers' })
  }
  await call('Page.reload', { ignoreCache: mode === 'cold' })
  await sleep(6000)
  const { result } = await call('Runtime.evaluate', { returnByValue: true, expression: `(() => {
    const nav = performance.getEntriesByType('navigation')[0];
    const paint = Object.fromEntries(performance.getEntriesByType('paint').map(p => [p.name, Math.round(p.startTime)]));
    const res = performance.getEntriesByType('resource');
    const js = res.filter(r => r.name.endsWith('.js'));
    const lt = window.__perf.longTasks;
    return {
      domContentLoaded: Math.round(nav.domContentLoadedEventEnd), load: Math.round(nav.loadEventEnd),
      firstPaint: paint['first-paint'], firstContentfulPaint: paint['first-contentful-paint'],
      composer: window.__perf.marks.composer, sessionList: window.__perf.marks.sessionList,
      jsFiles: js.length, jsFromNetwork: js.filter(r => r.transferSize > 0).length,
      jsTransferKB: Math.round(js.reduce((a, r) => a + r.transferSize, 0) / 1024),
      longTasks: lt.length, longTaskMs: lt.reduce((a, t) => a + t[1], 0), longestTask: Math.max(0, ...lt.map(t => t[1])),
      firstSecondsLongTaskMs: lt.filter(t => t[0] < 3000).reduce((a, t) => a + t[1], 0)
    } })()` })
  results.push(result.result.value)
}
console.log(mode, JSON.stringify(results, null, 0).replaceAll('},{', '},\n{'))
ws.close()
