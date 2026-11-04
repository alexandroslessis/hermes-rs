// Settings ▸ Providers ▸ Custom endpoints against a running hermes-desktop (the perf instance,
// with mock_llm.py on :47900): validate (the /v1 retry), save with a key and as the default, the
// model picker, delete.   node scripts/perf/endpoints-check.mjs [port]
const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}

const before = await rest('GET', '/api/providers/custom-endpoints')
check('list', before.status === 200 && Array.isArray(before.body.endpoints), before.body.current)

const probe = await rest('POST', '/api/providers/custom-endpoints/validate', { name: 'x', base_url: 'http://127.0.0.1:47900', model: '' })
check('validate', probe.body.ok && Boolean(probe.body.resolved_base_url) && probe.body.models.includes('mock'), probe.body)
const bad = await rest('POST', '/api/providers/custom-endpoints/validate', { name: 'x', base_url: 'http://127.0.0.1:1', model: '' })
check('validate an unreachable URL', bad.body.ok === false && bad.body.reachable === false, bad.body.message)
const refused = await rest('POST', '/api/providers/custom-endpoints', { name: 'x', base_url: 'http://x', model: 'm', api_mode: 'codex_responses' })
check('refuse another API', refused.status === 400, refused.body.detail)

const saved = await rest('POST', '/api/providers/custom-endpoints', {
  name: 'Smoke Endpoint', base_url: probe.body.resolved_base_url, model: 'mock', api_key: 'sk-smoke-test-key-1234567890', make_default: true
})
check('save as default', saved.status === 200 && saved.body.current.provider === saved.body.id, saved.body.current)
const id = saved.body.id
const listed = saved.body.endpoints.find(e => e.id === id)
check('key stored, shown masked', listed?.has_api_key === true && !String(listed.api_key_preview).includes('test-key-12'), listed?.api_key_preview)

const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws`, ['hermes-gateway-ticket.endpoints'])
await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail })
const options = await new Promise(ok => {
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id === 1) ok(m.result) }
  ws.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'model.options', params: {} }))
})
ws.close()
const picked = options.providers.find(p => p.slug === id)
check('model picker lists it', picked?.models.includes('mock') && options.provider === id, { provider: options.provider, models: picked?.models })

const activated = await rest('POST', `/api/providers/custom-endpoints/${encodeURIComponent(before.body.current.provider || id)}/activate`)
check('activate the previous default (keeps a model)', activated.status === 200 && activated.body.model !== '', activated.body)
const deleted = await rest('DELETE', `/api/providers/custom-endpoints/${encodeURIComponent(id)}`)
check('delete', deleted.status === 200 && !deleted.body.endpoints.some(e => e.id === id), deleted.body.current)

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
