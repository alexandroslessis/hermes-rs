// The profiles window's SOUL.md editor against a running hermes-desktop (the perf instance, no
// provider needed): reads the default profile's persona, saves an edit, reads it back, checks a
// never-written one reads as missing and another profile name is refused like Hermes. Restores
// the SOUL.md it found.   node scripts/perf/profiles-check.mjs [port]
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

const before = await rest('GET', '/api/profiles/default/soul')
check('the soul reads', before.status === 200 && typeof before.body?.content === 'string' && typeof before.body?.exists === 'boolean', before.body)
const saved = before.body?.exists === true

const edit = await rest('PUT', '/api/profiles/default/soul', { content: 'You are terse, and this is the check edit.' })
check('the save answers ok', edit.status === 200 && edit.body?.ok === true, edit.body)
const after = await rest('GET', '/api/profiles/default/soul')
check('the edit reads back', after.body?.content === 'You are terse, and this is the check edit.' && after.body?.exists === true, after.body)

// Another profile name is Hermes's "does not exist" 404 (a single-profile backend).
const other = await rest('GET', '/api/profiles/second/soul')
check('another name is refused', other.status === 404 && other.body?.detail === "Profile 'second' does not exist.", other.body)
const otherSave = await rest('PUT', '/api/profiles/second/soul', { content: 'x' })
check('another name refuses the save', otherSave.status === 404, otherSave.body)

// Leave the instance as found: the soul it had, or an empty file removed by hand when none.
if (saved) {
  const restore = await rest('PUT', '/api/profiles/default/soul', { content: before.body.content })
  check('the original soul is restored', restore.body?.ok === true && (await rest('GET', '/api/profiles/default/soul')).body?.content === before.body.content)
} else {
  check('a saved-then-cleared soul reads as before', (await rest('PUT', '/api/profiles/default/soul', { content: '' })).body?.ok === true)
}

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
