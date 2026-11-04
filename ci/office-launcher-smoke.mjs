// Exercise inherited stdin/stdout through the actual release GUI executable.
// --standalone allows the same check against the GUI-independent development bin.
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const executable = process.argv[2]
assert.ok(executable, 'Usage: node ci/office-launcher-smoke.mjs EXECUTABLE [--standalone]')
const standalone = process.argv[3] === '--standalone'
const root = mkdtempSync(join(tmpdir(), 'dsh-office-launcher-'))
try {
  const messages = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'launcher-smoke', version: '1' } } },
    { jsonrpc: '2.0', method: 'notifications/initialized' },
    { jsonrpc: '2.0', id: 2, method: 'tools/list' },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'docx_create', arguments: { path: 'smoke.docx', paragraphs: ['Hello 世界'] } } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'docx_read', arguments: { path: 'smoke.docx' } } },
  ]
  const result = spawnSync(resolve(executable), [...(standalone ? [] : ['--office-mcp']), '--workspace', root], {
    input: messages.map(message => JSON.stringify(message)).join('\n') + '\n',
    encoding: 'utf8', timeout: 30_000, maxBuffer: 2 * 1024 * 1024,
    windowsHide: true,
  })
  assert.ifError(result.error)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stderr, '', 'MCP startup must not initialize GUI/logging')
  const replies = result.stdout.trim().split('\n').map(line => JSON.parse(line))
  assert.deepEqual(replies.map(reply => reply.id), [1, 2, 3, 4])
  assert.equal(replies[0].result.serverInfo.name, 'dsh-office')
  assert.ok(replies[1].result.tools.some(tool => tool.name === 'xlsx_create'))
  assert.equal(replies[2].result.isError, false)
  assert.equal(replies[3].result.isError, false)
  assert.deepEqual(JSON.parse(replies[3].result.content[0].text).paragraphs, ['Hello 世界'])
  assert.ok(statSync(join(root, 'smoke.docx')).size > 0)
  console.log(`Office ${standalone ? 'standalone' : 'launcher release'} stdio smoke passed`)
} finally {
  rmSync(root, { recursive: true, force: true })
}
