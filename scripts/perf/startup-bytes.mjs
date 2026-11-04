// What the page loads before its first paint, attributed by source map to packages and app
// folders: the launcher's static imports plus the main chunk it imports at once, and theirs.
//   node scripts/perf/startup-bytes.mjs <source-map build dir> [group|file] [rows]
import { readFileSync, readdirSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
const D = process.argv[2], depth = process.argv[3] ?? 'group'
// startup graph: index.html module + static imports + main entry (dynamic import from index)
const html = readFileSync(join(D, 'index.html'), 'utf8')
const queue = [...html.matchAll(/(?:src|href)="\.\/(assets\/[^"]+\.js)"/g)].map(m => resolve(D, m[1]))
const idx = readFileSync(queue.find(f => /index-/.test(f)), 'utf8')
for (const m of idx.matchAll(/import\(`\.\/(main-[^`]+\.js)`\)/g)) queue.push(resolve(D, 'assets', m[1]))
const seen = new Set()
while (queue.length) { const f = queue.pop(); if (seen.has(f)) continue; seen.add(f)
  const src = readFileSync(f, 'utf8')
  for (const m of src.matchAll(/(?:^|[;}\s])import\s*(?:[^"'()]*?from\s*)?["'](\.\.?\/[^"']+\.js)["']/g)) queue.push(resolve(dirname(f), m[1])) }
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
const dec = s => { const out = []; let v = 0, shift = 0; for (const c of s) { let d = B64.indexOf(c); v += (d & 31) << shift; if (d & 32) shift += 5; else { out.push(v & 1 ? -(v >>> 1) : v >>> 1); v = 0; shift = 0 } } return out }
const bySrc = new Map(); const byChunk = new Map()
const key = s => { s = s.split(String.fromCharCode(92)).join('/')
  const nm = s.lastIndexOf('node_modules/'); if (nm >= 0) { const r = s.slice(nm + 13).split('/'); return 'npm:' + (r[0].startsWith('@') ? r[0] + '/' + r[1] : r[0]) }
  const i = s.indexOf('/src/'); const parts = s.slice(i + 5).split('/'); const app = s.includes('apps/shared') ? 'shared' : 'desktop'
  return `${app}:` + parts.slice(0, depth === 'file' ? 99 : Math.min(parts.length - 1, 2)).join('/') }
for (const f of seen) {
  const code = readFileSync(f, 'utf8'); let map; try { map = JSON.parse(readFileSync(f + '.map', 'utf8')) } catch { continue }
  const lines = code.split('\n'); let srcIdx = 0
  map.mappings.split(';').forEach((line, li) => { const segs = line ? line.split(',').map(dec) : []
    const L = lines[li] ?? ''; let col = 0
    const starts = []; for (const seg of segs) { col += seg[0]; starts.push(col) }
    segs.forEach((seg, si) => { if (seg.length > 1) srcIdx += seg[1]
      const end = si + 1 < segs.length ? starts[si + 1] : L.length
      const k = seg.length > 1 ? key(map.sources[srcIdx] ?? '?') : '(unmapped)'
      bySrc.set(k, (bySrc.get(k) ?? 0) + (end - starts[si])) }) })
  byChunk.set(f.split(/[\/]/).pop(), code.length) }
const total = [...bySrc.values()].reduce((a, b) => a + b, 0)
console.log(`startup: ${seen.size} chunks, ${(total / 1048576).toFixed(2)} MB attributed`)
for (const [k, v] of [...bySrc].sort((a, b) => b[1] - a[1]).slice(0, +(process.argv[4] ?? 45))) console.log(`${(v / 1024).toFixed(0).padStart(6)} KB  ${k}`)
