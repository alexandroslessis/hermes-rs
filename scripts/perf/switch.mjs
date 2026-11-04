// Session switch timing in MY perf instance: from a new chat to the given session, until its last
// reply's text is in the page, with main-thread work and long tasks. node switch.mjs <hash> [runs]
const hash = process.argv[2]
const runs = +(process.argv[3] ?? 3)
const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } }
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })
const evaluate = async expression => (await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result.result?.value
const sleep = ms => new Promise(r => setTimeout(r, ms))
await call('Performance.enable')
const metric = async k => (await call('Performance.getMetrics')).result.metrics.find(m => m.name === k).value

for (let i = 0; i < runs; i++) {
  await evaluate(`location.hash = '#/'`)
  await sleep(2500)
  const before = await metric('TaskDuration')
  const r = await evaluate(`new Promise(done => {
    const lt = []; const obs = new PerformanceObserver(l => { for (const e of l.getEntries()) lt.push(Math.round(e.duration)) }); obs.observe({ type: 'longtask' });
    const t0 = performance.now();
    location.hash = '${hash}';
    const has = () => document.body.innerText.includes('Reply number 10, please.') && document.body.innerText.split('Section').length > 20;
    const poll = () => {
      if (has()) {
        const shown = performance.now() - t0;
        requestAnimationFrame(() => setTimeout(() => { obs.disconnect(); done({ shownMs: Math.round(shown), longTasks: lt.length, longTaskMs: lt.reduce((a, b) => a + b, 0), longest: Math.max(0, ...lt), nodes: document.getElementsByTagName('*').length }) }, 500));
      } else if (performance.now() - t0 > 15000) { obs.disconnect(); done({ timeout: true }) } else setTimeout(poll, 10);
    };
    poll();
  })`)
  r.mainThreadS = +((await metric('TaskDuration')) - before).toFixed(2)
  console.log(JSON.stringify(r))
}
ws.close()
