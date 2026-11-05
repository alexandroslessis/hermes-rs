// The side panel's terminal against a running hermes-desktop (the perf instance): open a shell,
// run a command, drop the socket and reattach (the output replays), resize, close it, and check a
// closed shell can't be reattached.   node scripts/perf/terminal-check.mjs [port]
const port = process.argv[2] ?? '47810'
const META = '\u0000HERMES_TERMINAL_META:'

let failed = 0
const check = (label, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 240)}`}`)
  if (!ok) failed++
}
const sleep = ms => new Promise(r => setTimeout(r, ms))

/** One socket: its metadata, everything it printed, and how it closed. */
function dial(params) {
  const url = new URL(`ws://127.0.0.1:${port}/api/host-terminal`)
  Object.entries(params).forEach(([k, v]) => url.searchParams.set(k, v))
  const ws = new WebSocket(url)
  ws.binaryType = 'arraybuffer'
  const decoder = new TextDecoder()
  // (A reattach's replay holds the old cursor query: the page doesn't answer a replay.)
  const session = { ws, meta: null, frames: 0, text: '', closed: null, answered: Boolean(params.attach) }
  session.opened = new Promise(resolve => {
    ws.onmessage = event => {
      if (typeof event.data === 'string' && event.data.startsWith(META)) {
        session.meta = JSON.parse(event.data.slice(META.length))
        resolve()
        return
      }
      session.frames++
      session.text += typeof event.data === 'string' ? event.data : decoder.decode(event.data, { stream: true })
      // Answer the pseudo-console's cursor query as xterm.js does.
      if (session.text.includes('\u001b[6n') && !session.answered) {
        session.answered = true
        ws.send('\u001b[1;1R')
      }
    }
    ws.onclose = event => { session.closed = { code: event.code, reason: event.reason }; resolve() }
  })
  session.until = async (test, ms = 20000) => {
    const deadline = Date.now() + ms
    while (!test() && Date.now() < deadline) await sleep(100)
    return test()
  }
  return session
}

const first = dial({ cols: '100', rows: '30' })
await first.opened
check('opens a shell', first.meta?.terminalId && /pwsh|powershell|cmd/.test(first.meta.shell), first.meta)
first.ws.send('echo terminal-check-one\r')
check('runs a command', await first.until(() => first.text.includes('terminal-check-one\r\n') || first.text.split('terminal-check-one').length > 2), first.text.slice(-200))
first.ws.send('\u001b[RESIZE:120;40]')
first.ws.close()
await first.until(() => first.closed)

const again = dial({ attach: first.meta.terminalId, cols: '120', rows: '40' })
await again.opened
check('reattaches to the same shell', again.meta?.terminalId === first.meta.terminalId && again.meta.reconnected === true, again.meta)
check('its output replays', await again.until(() => again.text.includes('terminal-check-one')), again.text.slice(-200))
again.ws.send('echo terminal-check-two\r')
check('and it still runs commands', await again.until(() => again.text.includes('terminal-check-two\r\n')) && !again.text.includes('CommandNotFound'), again.text.slice(-200))
again.ws.close()
await again.until(() => again.closed)

const closer = dial({ attach: first.meta.terminalId, action: 'close' })
await closer.opened
check('close ends it', closer.meta?.closed === true && closer.meta.terminalId === first.meta.terminalId, closer.meta)
await closer.until(() => closer.closed)
const gone = dial({ attach: first.meta.terminalId })
await gone.opened
check('a closed shell is gone (4410)', gone.closed?.code === 4410 && !gone.meta, gone.closed)

const exiting = dial({})
await exiting.opened
await exiting.until(() => exiting.answered, 10000)
exiting.ws.send('exit\r')
check('exit closes the tab (4410 "Process exited")', await exiting.until(() => exiting.closed) && exiting.closed.code === 4410 && exiting.closed.reason === 'Process exited', exiting.closed)

console.log(failed ? `\n${failed} check(s) failed` : '\nall checks passed')
process.exit(failed ? 1 : 0)
