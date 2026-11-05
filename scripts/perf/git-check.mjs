// The review pane's git routes against a running hermes-desktop (the perf instance), in a
// throwaway repository: list, diff (worktree, staged, untracked), stage / unstage, commit
// context, commit, branches, worktree add / remove, revert, and a refused credential diff.
//   node scripts/perf/git-check.mjs [port] [empty folder]
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const port = process.argv[2] ?? '47810'
const repo = process.argv[3] ?? fs.mkdtempSync(path.join(os.tmpdir(), 'git-check-'))
const base = `http://127.0.0.1:${port}`
fs.mkdirSync(repo, { recursive: true })
const git = (...args) => execFileSync('git', args, { cwd: repo, encoding: 'utf8' })
git('init', '-q', '-b', 'main')
git('config', 'user.email', 'check@example.com')
git('config', 'user.name', 'Check')
fs.writeFileSync(path.join(repo, 'a.txt'), 'one\n')
fs.writeFileSync(path.join(repo, '.env'), 'KEY=1\n')
git('add', 'a.txt')
git('commit', '-q', '-m', 'first')
fs.writeFileSync(path.join(repo, 'a.txt'), 'one\ntwo\n')
fs.writeFileSync(path.join(repo, 'b.txt'), 'new\n')

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const get = async (route, params) => {
  const r = await fetch(`${base}/api/git/${route}?${new URLSearchParams({ path: repo, ...params })}`)
  return { status: r.status, body: await r.json().catch(() => null) }
}
const post = async (route, body) => {
  const r = await fetch(`${base}/api/git/${route}`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ path: repo, ...body }) })
  return { status: r.status, body: await r.json().catch(() => null) }
}

const list = await get('review/list', { scope: 'uncommitted' })
const rows = list.body?.files ?? []
check('list', rows.map(f => `${f.status}${f.path}`).join(',') === 'M a.txt,? .env,? b.txt'.replace(/ /g, '') || rows.length === 3, rows)
const diff = await get('review/diff', { file: 'a.txt', scope: 'uncommitted' })
check('diff', diff.status === 200 && diff.body.diff.includes('+two'), diff.body)
const added = await get('review/diff', { file: 'b.txt', scope: 'uncommitted' })
check('untracked diff (all added)', added.body?.diff?.includes('+new'), added.body)
const secret = await get('review/diff', { file: '.env', scope: 'uncommitted' })
check('credential diff refused', secret.status === 403, secret.body)
const preview = await get('file-diff', { file: path.join(repo, 'a.txt') })
check('file-diff (preview)', preview.body?.diff?.includes('+two'), preview.body)

check('stage', (await post('review/stage', { file: 'a.txt' })).status === 200)
const staged = await get('review/diff', { file: 'a.txt', scope: 'uncommitted', staged: 'true' })
check('staged diff', staged.body?.diff?.includes('+two'), staged.body)
const context = await get('review/commit-context', {})
check('commit context (staged only, no credentials)', context.body.diff.includes('+two') && !context.body.diff.includes('KEY=1') && context.body.recent === 'first', context.body)
check('unstage', (await post('review/unstage', { file: 'a.txt' })).status === 200)

fs.rmSync(path.join(repo, '.env'))
const committed = await post('review/commit', { message: 'second', push: false })
check('commit (stages everything)', committed.status === 200 && git('log', '-1', '--pretty=%s').trim() === 'second', committed.body)
const failedCommit = await post('review/commit', { message: 'nothing', push: false })
check('commit with nothing to commit fails with git\'s message', failedCommit.status === 400 && /nothing to commit|nothing added/i.test(failedCommit.body.detail), failedCommit.body)

const branches = await get('branches', {})
check('branches', branches.body.branches.some(b => b.name === 'main' && b.isDefault && b.checkedOut), branches.body)
const tree = await post('worktree/add', { name: 'Try It' })
check('worktree add', tree.status === 200 && tree.body.branch === 'hermes/try-it' && fs.existsSync(tree.body.path), tree.body)
const trees = await get('worktrees', {})
check('worktrees', trees.body.worktrees.length === 2 && trees.body.worktrees[0].isMain, trees.body)
check('worktree remove', (await post('worktree/remove', { worktreePath: tree.body.path, force: true })).status === 200)

fs.writeFileSync(path.join(repo, 'a.txt'), 'broken\n')
check('revert', (await post('review/revert', { file: 'a.txt' })).status === 200 && fs.readFileSync(path.join(repo, 'a.txt'), 'utf8').replace(/\r\n/g, '\n') === 'one\ntwo\n')
const ship = await get('review/ship-info', {})
check('ship-info answers (no GitHub remote here)', ship.status === 200 && ship.body.pr === null, ship.body)

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
