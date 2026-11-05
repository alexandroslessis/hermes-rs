// Looks up methods in Hermes's gateway contract (the JSON-RPC methods the page may call) and prints
// their params and result schemas with every $ref resolved, plus whether gateway.rs answers them.
//   tools/node/node.exe scripts/contract.mjs session.branch        one method
//   tools/node/node.exe scripts/contract.mjs mcp.                  every method starting with "mcp."
//   tools/node/node.exe scripts/contract.mjs --missing [prefix]     methods gateway.rs doesn't answer
//   tools/node/node.exe scripts/contract.mjs --events [prefix]      notifications (event payloads)
// Reads target/hermes-src (run scripts/build-hermes-webapp.ps1 first).
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const contract = JSON.parse(fs.readFileSync(path.join(root, 'target/hermes-src/apps/shared/src/gateway-contract.openrpc.json'), 'utf8'))
const gateway = fs.readFileSync(path.join(root, 'crates/desktop/src/gateway.rs'), 'utf8')
const served = name => gateway.includes(`"${name}"`)

const schemas = contract.components?.schemas ?? {}
const resolve = (node, depth = 0) => {
  if (depth > 8 || node === null || typeof node !== 'object') return node
  if (Array.isArray(node)) return node.map(n => resolve(n, depth + 1))
  if (node.$ref) return resolve(schemas[node.$ref.split('/').at(-1)] ?? node, depth + 1)
  return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, resolve(v, depth + 1)]))
}

const args = process.argv.slice(2)
if (args[0] === '--missing') {
  const prefix = args[1] ?? ''
  const missing = contract.methods.filter(m => m.name.startsWith(prefix) && !served(m.name))
  for (const m of missing) console.log(`${m.name.padEnd(40)} ${m.summary ?? ''}`)
  console.log(`\n${missing.length} of ${contract.methods.filter(m => m.name.startsWith(prefix)).length} not answered`)
} else if (args[0] === '--events') {
  const notes = contract['x-notifications'] ?? []
  const list = Array.isArray(notes) ? notes : Object.entries(notes).map(([name, v]) => ({ name, ...v }))
  for (const n of list.filter(n => (n.name ?? '').startsWith(args[1] ?? ''))) console.log(JSON.stringify(resolve(n), null, 1))
} else if (args[0]) {
  const found = contract.methods.filter(m => m.name === args[0] || (args[0].endsWith('.') && m.name.startsWith(args[0])))
  if (!found.length) { console.log('no such method'); process.exit(1) }
  for (const m of found) {
    console.log(`## ${m.name}  (${served(m.name) ? 'answered by gateway.rs' : 'NOT answered'})\n${m.summary ?? ''}`)
    console.log('params:', JSON.stringify(resolve(m.params?.[0]?.schema), null, 1))
    console.log('result:', JSON.stringify(resolve(m.result?.schema), null, 1), '\n')
  }
} else {
  console.log('usage: contract.mjs <method | prefix.> | --missing [prefix] | --events [prefix]')
}
