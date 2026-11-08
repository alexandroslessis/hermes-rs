// The file tree's "Open in default app" (patches/0015) against a running hermes-desktop (the
// perf instance): the menu item asks POST /api/hermes-rs/open-file with a plain absolute path.
// The happy path opens a window on the machine, so this checks the shapes the page relies on
// without opening anything: a missing file, a relative path, an empty path and the file:// form
// (the artifacts gallery's) all answer 400 with a readable detail — a 404 would mean the route
// isn't served. The open itself is exercised by artifacts-check.mjs and the open_file.rs unit
// tests.   node scripts/perf/file-tree-open-check.mjs [port]

const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const rest = async (method, path, body) => {
  const r = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' }, body: body && JSON.stringify(body) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

const open = url => rest('POST', '/api/hermes-rs/open-file', { url })

const missing = await open('C:\\definitely\\not\\here.md')
check('a missing file is a readable 400, not a 404', missing.status === 400 && missing.body.detail.endsWith("doesn't exist"), missing.body)
const relative = await open('relative/path.md')
check('a relative path is refused', relative.status === 400 && relative.body.detail.startsWith('Not a local file'), relative.body)
const empty = await open('')
check('an empty path is refused', empty.status === 400 && empty.body.detail.startsWith('Not a local file'), empty.body)
const link = await open('file:///C:/definitely/not/here.md')
check("the file:// form (the artifacts gallery's) is still served", link.status === 400 && link.body.detail.endsWith("doesn't exist"), link.body)

// The reveal route (patches/0016): same shapes; the happy path opens a window, so
// refusal paths only (the open itself is covered by the run() normalization test).
const reveal = url => rest('POST', '/api/hermes-rs/reveal', { url })
const rMissing = await reveal('C:\\definitely\\not\\here.md')
check('a missing file is a readable 400, not a 404', rMissing.status === 400 && rMissing.body.detail.endsWith("doesn't exist"), rMissing.body)
const rRelative = await reveal('relative/path.md')
check('a relative path is refused', rRelative.status === 400 && rRelative.body.detail.startsWith('Not a local file'), rRelative.body)

console.log(failed ? `${failed} FAILED` : 'all ok')
process.exit(failed ? 1 : 0)
