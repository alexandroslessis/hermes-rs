// Talks to `hermes-desktop.exe --acp` like an editor does and prints every line both ways, for
// seeing what goes over the wire and for checking a build (debug or release) by hand.
//
//   node scripts/acp/acp-probe.mjs --exe <path> --home <data folder> [--cwd <folder>]
//        [--prompt "text"] [--allow] [--forms]
//
// - `--home` is HERMES_RS_HOME: use a folder of your own (target/acp-home), never the real one
//   unless you mean to. Put a config.json in it with a provider (docs/acp/TESTING.md).
// - Without `--prompt` it only does initialize + session/new + session/list, then closes stdin.
// - Permission and question requests from the agent are answered: denied, or allowed with
//   `--allow` (the first allow option, or the first choice).
// - Exits 1 when a line on stdout isn't JSON (the stdout rule), or the agent errors out.

import { spawn } from 'node:child_process'
import { mkdirSync } from 'node:fs'

const args = process.argv.slice(2)
const flag = name => args.includes(`--${name}`)
const value = name => {
  const at = args.indexOf(`--${name}`)
  return at >= 0 ? args[at + 1] : undefined
}

const exe = value('exe')
const home = value('home')
if (!exe || !home) {
  console.error('usage: acp-probe.mjs --exe <hermes-desktop.exe> --home <data folder> [--cwd dir] [--prompt text] [--allow] [--forms]')
  process.exit(2)
}
const cwd = value('cwd') ?? process.cwd()
mkdirSync(home, { recursive: true })

const started = Date.now()
const stamp = () => `${String(Date.now() - started).padStart(6)}ms`
let failed = 0

const child = spawn(exe, ['--acp'], {
  env: { ...process.env, HERMES_RS_HOME: home },
  stdio: ['pipe', 'pipe', 'pipe']
})
child.stderr.on('data', data => {
  for (const line of String(data).split('\n').filter(Boolean)) console.log(`${stamp()} !  ${line}`)
})

const waiting = new Map()
let nextId = 0
const write = message => {
  console.log(`${stamp()} -> ${JSON.stringify(message)}`)
  child.stdin.write(`${JSON.stringify(message)}\n`)
}
const request = (method, params) =>
  new Promise(resolve => {
    const id = ++nextId
    waiting.set(id, resolve)
    write({ jsonrpc: '2.0', id, method, params })
  })

let buffer = ''
child.stdout.on('data', data => {
  buffer += data
  let newline
  while ((newline = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, newline).replace(/\r$/, '')
    buffer = buffer.slice(newline + 1)
    if (!line.trim()) continue
    let message
    try {
      message = JSON.parse(line)
    } catch {
      console.log(`${stamp()} !! stdout line that is not JSON: ${line}`)
      failed++
      continue
    }
    console.log(`${stamp()} <- ${line.length > 400 ? `${line.slice(0, 400)}…` : line}`)
    if (message.method && message.id !== undefined) {
      // A request of the agent's: answer it.
      const params = message.params ?? {}
      if (message.method === 'session/request_permission') {
        const options = params.options ?? []
        const pick = flag('allow')
          ? (options.find(o => String(o.kind).startsWith('allow')) ?? options[0])
          : (options.find(o => String(o.kind).startsWith('reject')) ?? options.at(-1))
        write({ jsonrpc: '2.0', id: message.id, result: { outcome: { outcome: 'selected', optionId: pick?.optionId } } })
      } else if (message.method === 'elicitation/create') {
        const schema = params.requestedSchema?.properties?.answer
        const answer = flag('allow') ? (schema?.enum?.[0] ?? 'yes') : undefined
        write({
          jsonrpc: '2.0',
          id: message.id,
          result: answer === undefined ? { action: 'decline' } : { action: 'accept', content: { answer } }
        })
      } else {
        write({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Method not found' } })
      }
    } else if (message.id !== undefined && waiting.has(message.id)) {
      waiting.get(message.id)(message)
      waiting.delete(message.id)
    }
  }
})

const exited = new Promise(resolve => child.on('exit', code => resolve(code)))

const timeout = setTimeout(() => {
  console.error('timed out after 120 s')
  child.kill()
  process.exit(1)
}, 120_000)

const init = await request('initialize', {
  protocolVersion: 1,
  clientCapabilities: flag('forms') ? { elicitation: { form: {} } } : {},
  clientInfo: { name: 'acp-probe', version: '0' }
})
if (init.error) failed++
const created = await request('session/new', { cwd, mcpServers: [] })
if (created.error) {
  failed++
} else {
  const session = created.result.sessionId
  const text = value('prompt')
  if (text) {
    const done = await request('session/prompt', { sessionId: session, prompt: [{ type: 'text', text }] })
    console.log(`${stamp()} == prompt answered: ${JSON.stringify(done.result ?? done.error)}`)
    if (done.error) failed++
  }
  await request('session/list', { cwd })
}

child.stdin.end()
const code = await exited
clearTimeout(timeout)
console.log(`${stamp()} == the agent exited with ${code}`)
process.exit(failed || code ? 1 : 0)
