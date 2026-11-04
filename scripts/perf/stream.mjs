// Sends one prompt in MY perf instance's page and measures the page while the reply streams:
// main-thread work (Performance metrics delta), long tasks, frames, DOM size. Prints JSON.
//   node stream.mjs [seconds=24] [prompt]
const seconds = +(process.argv[2] ?? 24)
const prompt = process.argv[3] ?? 'Write a long performance report.'
const targets = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:47810'))
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } }
const call = (method, params = {}) => new Promise(ok => { pending.set(++id, ok); ws.send(JSON.stringify({ id, method, params })) })
const evaluate = async expression => (await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result.result?.value
const metrics = async () => Object.fromEntries((await call('Performance.getMetrics')).result.metrics.map(m => [m.name, m.value]))
const sleep = ms => new Promise(r => setTimeout(r, ms))

await call('Performance.enable')
await evaluate(`(() => {
  window.__stream = { longTasks: [], frames: 0, gaps: [] };
  new PerformanceObserver(l => { for (const e of l.getEntries()) window.__stream.longTasks.push(Math.round(e.duration)) }).observe({ type: 'longtask' });
  let last = performance.now();
  const tick = now => { window.__stream.frames++; window.__stream.gaps.push(now - last); last = now; if (!window.__stream.stop) requestAnimationFrame(tick) };
  requestAnimationFrame(tick);
})()`)
const ok = await evaluate(`(() => {
  const box = document.querySelector('[contenteditable="true"], textarea');
  if (!box) return false;
  box.focus();
  return true;
})()`)
if (!ok) throw new Error('no composer')
await call('Input.insertText', { text: prompt })
await sleep(300)
await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 })
await call('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 })
const before = await metrics()
const t0 = Date.now()
await sleep(seconds * 1000)
const after = await metrics()
const page2 = await evaluate(`(() => {
  const s = window.__stream; s.stop = true;
  const gaps = s.gaps.slice(5).sort((a, b) => a - b);
  const p = q => Math.round(gaps[Math.floor(gaps.length * q)] ?? 0);
  return { frames: s.frames, fps: +(s.frames / ${seconds}).toFixed(1), frameGapP50: p(0.5), frameGapP95: p(0.95), frameGapMax: Math.round(gaps.at(-1) ?? 0),
    longTasks: s.longTasks.length, longTaskMs: s.longTasks.reduce((a, b) => a + b, 0), longestTask: Math.max(0, ...s.longTasks),
    domNodes: document.getElementsByTagName('*').length }
})()`)
const d = k => +(after[k] - before[k]).toFixed(2)
console.log(JSON.stringify({
  seconds, mainThread: { scriptS: d('ScriptDuration'), layoutS: d('LayoutDuration'), styleS: d('RecalcStyleDuration'), taskS: d('TaskDuration'),
    layouts: d('LayoutCount'), styleRecalcs: d('RecalcStyleCount') },
  jsHeapMB: +(after.JSHeapUsedSize / 1048576).toFixed(1), ...page2 }, null, 1))
ws.close()
