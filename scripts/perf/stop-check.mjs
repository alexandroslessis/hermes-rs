// Stop while a turn is stuck, as the page's Stop button does (`session.interrupt`): the turn must
// end (`message.complete`, then `session.info` running=false) and stay ended (no running=true
// afterwards). Cases: the provider stalls mid-reply ("please hang"), a terminal command runs for
// ten minutes ("please run slow"). Needs mock_llm.py.   node scripts/perf/stop-check.mjs <port> <folder>
const [port, folder] = process.argv.slice(2)
const waitMs = Number(process.env.STOP_AFTER_MS ?? 4000)
const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.stop'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
let id = 0
const pending = new Map()
const events = []
ws.onmessage = e => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id) } else if (m.method === 'event') events.push({ at: Date.now(), ...m.params })
}
const rpc = (method, params = {}) => new Promise((ok, fail) => {
  pending.set(++id, m => (m.error ? fail(new Error(`${method}: ${m.error.message}`)) : ok(m.result)))
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
})
const sleep = ms => new Promise(r => setTimeout(r, ms))
let failed = 0

const prompts = process.argv.slice(4).length ? process.argv.slice(4) : ['please hang', 'please run slow']
for (const prompt of prompts) {
  const { session_id: sid } = await rpc('session.create', { cwd: folder })
  await rpc('session.resume', { session_id: sid })
  await rpc('prompt.submit', { session_id: sid, text: prompt, admission_id: `adm-${Date.now()}` })
  await sleep(waitMs)
  const stopAt = Date.now()
  const answer = await rpc('session.interrupt', { session_id: sid })
  await sleep(8000)
  const after = events.filter(e => e.session_id === sid && e.at >= stopAt)
  const complete = after.find(e => e.type === 'message.complete')
  const lastInfo = after.filter(e => e.type === 'session.info').at(-1)
  const ended = Boolean(complete) && lastInfo?.payload?.running === false
  console.log(`${ended ? 'ok  ' : 'FAIL'} "${prompt}": interrupt=${JSON.stringify(answer)} ` +
    `complete=${complete ? `${complete.at - stopAt}ms ${complete.payload?.status ?? ''}` : 'none'} ` +
    `last running=${lastInfo?.payload?.running}`)
  if (!ended) failed++
}
ws.close()
process.exit(failed ? 1 : 0)
