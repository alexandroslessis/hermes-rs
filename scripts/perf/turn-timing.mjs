// Times one turn end to end, with its tool calls: when each tool started and completed, and when
// the turn ended.   node scripts/perf/turn-timing.mjs <port> <folder> "<prompt>"
const [port, folder, prompt] = process.argv.slice(2)
const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.timing'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const t0 = Date.now()
let done
const finished = new Promise(r => (done = r))
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); return }
  if (m.method !== 'event') return
  const { type, payload } = m.params
  if (['tool.start', 'tool.complete', 'message.complete'].includes(type)) {
    const size = payload?.result ? ` result ${String(payload.result).length} chars` : ''
    console.log(`${String(Date.now() - t0).padStart(7)} ms  ${type} ${payload?.name ?? ''}${size}`)
  }
  if (type === 'message.complete') done()
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(new Error(`${method}: ${m.error.message}`)) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})
const { session_id: sid } = await rpc('session.create', { cwd: folder })
await rpc('session.resume', { session_id: sid })
await rpc('prompt.submit', { session_id: sid, text: prompt, admission_id: `adm-${Date.now()}` })
await Promise.race([finished, new Promise(r => setTimeout(r, 300_000))])
console.log(`turn ${Date.now() - t0} ms`)
ws.close()
