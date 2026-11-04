// Real published DSH MCP bridge + native ToolRuntime, with no model or API key.
// This is deliberately separate from the launcher's production dependencies.
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import * as McpClient from '@deepseek-ai/dsh-mcp-client'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import * as NativeToolExecutor from '@deepseek-ai/dsh-tool-executor-native'
import ToolRuntime from '@deepseek-ai/dsh-tools'

const [executable, mode, ...extra] = process.argv.slice(2)
assert.ok(executable, 'Usage: node ci/office-dsh/smoke.mjs EXECUTABLE [--standalone]')
assert.ok((mode === undefined || mode === '--standalone') && extra.length === 0, 'Only --standalone is supported')
const command = resolve(executable)
assert.ok((await stat(command)).isFile(), `Missing compiled Office executable: ${command}`)

const root = await mkdtemp(join(tmpdir(), 'dsh-office-official-client-'))
const workspace = join(root, 'workspace')
await mkdir(workspace)
const ctx = new Context()
let sequence = 0
let checks = 0

function passed(description) {
  checks += 1
  console.log(`ok ${checks} - ${description}`)
}

// Use the public batch API used by the native executor for model tool calls.
// JSON argument parsing, tool selection, execution, and result projection all
// run inside DSH. There is intentionally no LLM runtime or provider mounted.
async function call(rawName, args) {
  const id = `office-smoke-${++sequence}`
  const batch = await ctx.tools.execute({
    calls: [{ type: 'tool-call', id, name: `mcp__office-rust__${rawName}`, arguments: JSON.stringify(args) }],
    signal: AbortSignal.timeout(20_000),
  })
  assert.equal(batch.results.length, 1, 'DSH must settle exactly one result per call')
  assert.equal(batch.results[0].callId, id, 'DSH must preserve the model tool-call ID')
  assert.equal(batch.concluded, false)
  return batch.results[0].result
}

function text(result) {
  assert.ok(Array.isArray(result.content) && result.content.length > 0, 'A model-visible result is required')
  for (const block of result.content) assert.equal(block.type, 'text')
  return result.content.map(block => block.text).join('\n')
}

async function success(name, args) {
  const result = await call(name, args)
  assert.equal(result.isError, false, `${name}: ${text(result)}`)
  // Canonical MCP value and model projection must preserve the same payload.
  assert.deepEqual(result.value.content, result.content)
  return JSON.parse(text(result))
}

async function failure(name, args, pattern) {
  const result = await call(name, args)
  assert.equal(result.isError, true, `${name} should fail`)
  assert.equal(result.value, undefined, 'Errors must not become successful canonical values')
  assert.match(text(result), pattern)
}

try {
  await ctx.plugin(SystemPrompt)
  await ctx.plugin(ToolRuntime)
  await ctx.plugin(NativeToolExecutor)
  await ctx.plugin(McpClient, McpClient.Config({
    transport: 'stdio',
    serverName: 'office-rust',
    command,
    args: [...(mode === '--standalone' ? [] : ['--office-mcp']), '--workspace', workspace],
    env: {},
    cwd: workspace,
    startupTimeoutMs: 30_000,
    toolCallTimeoutMs: 15_000,
    closeTimeoutMs: 5_000,
    failOnStartupError: true,
    reconnect: { enabled: false },
  }))

  const expectedNames = ['docx_create', 'docx_read', 'office_info', 'office_preview', 'xlsx_create', 'xlsx_read']
    .map(name => `mcp__office-rust__${name}`).sort()
  assert.deepEqual(ctx.tools.schemas().map(tool => tool.name).sort(), expectedNames)
  const assembly = await ctx.systemPrompt.assemble()
  assert.deepEqual(assembly.tools.map(tool => tool.name).sort(), expectedNames)
  for (const schema of assembly.tools) {
    assert.equal(schema.parameters.type, 'object')
    assert.ok(schema.description.length > 0)
    assert.equal(schema.execute, undefined, 'Executable callbacks must not leak into model schemas')
  }
  const modelSchema = name => assembly.tools.find(tool => tool.name === `mcp__office-rust__${name}`).parameters
  assert.deepEqual(modelSchema('office_preview').required, ['path', 'output'])
  assert.deepEqual(modelSchema('docx_create').required, ['path', 'paragraphs'])
  assert.equal(modelSchema('docx_create').properties.paragraphs.items.type, 'string')
  assert.deepEqual(modelSchema('xlsx_create').properties.sheets.items.properties.rows.items.items.type,
    ['string', 'number', 'boolean', 'null'])
  passed('official DSH bridge registers all six namespaced tools in the model prompt')

  const info = await success('office_info', {})
  assert.equal(info.runtime, 'rust')
  assert.deepEqual(info.formats, ['docx', 'xlsx'])
  assert.equal(info.preview.mode, 'semantic')
  assert.equal(info.preview.scripts, false)
  assert.equal(info.preview.external_assets, false)
  passed('office_info executes through the DSH native executor')

  const hostileText = '<script>alert(1)</script><img src=x onerror=alert(2)>'
  const paragraphs = [hostileText, 'DSH → Rust Office 中文 😀', '  Spaces, & <tags>, "quotes"\nline\ttab  ', '']
  await success('docx_create', { path: 'report.docx', paragraphs })
  const docxBytes = await readFile(join(workspace, 'report.docx'))
  assert.equal(docxBytes.subarray(0, 2).toString(), 'PK', 'DOCX must exist on disk as a ZIP package')
  assert.deepEqual((await success('docx_read', { path: 'report.docx' })).paragraphs, paragraphs)
  passed('DOCX create/read preserves Unicode, XML characters, whitespace, and empty paragraphs')

  const row = ['中文 & <>', 42, -1.5, true, false, null, '=SUM(A1:A2)', '_x0041_', '😀\r\n', hostileText]
  await success('xlsx_create', {
    path: 'book.xlsx',
    sheets: [{ name: 'Data', rows: [row] }, { name: 'Empty', rows: [] }],
  })
  const xlsxBytes = await readFile(join(workspace, 'book.xlsx'))
  assert.equal(xlsxBytes.subarray(0, 2).toString(), 'PK', 'XLSX must exist on disk as a ZIP package')
  const book = await success('xlsx_read', { path: 'book.xlsx' })
  assert.deepEqual(book.sheets.map(sheet => sheet.name), ['Data', 'Empty'])
  assert.deepEqual(book.sheets[0].cells.map(cell => cell.value), row.filter(value => value !== null))
  assert.deepEqual(book.sheets[1].cells, [])
  assert.equal(book.formula_cells, 0)
  assert.equal(book.sheets[0].cells[5].cell, 'G1')
  assert.equal(book.sheets[0].cells[5].type, 'string')
  passed('XLSX create/read preserves types, sparse nulls, empty sheets, and literal formula text')

  for (const [path, output, sourceFormat] of [
    ['report.docx', 'report.html', 'docx'], ['book.xlsx', 'book.html', 'xlsx'],
  ]) {
    const preview = await success('office_preview', { path, output })
    assert.equal(preview.path, output)
    assert.equal(preview.source, path)
    assert.equal(preview.source_format, sourceFormat)
    assert.equal(preview.format, 'html')
    assert.equal(preview.mode, 'semantic')
    assert.ok(preview.warnings.some(warning => warning.includes('not full-fidelity Office rendering')))
    const html = await readFile(join(workspace, output), 'utf8')
    assert.equal(preview.bytes, Buffer.byteLength(html))
    assert.match(html, /<!doctype html>/i)
    assert.match(html, /Preview limitations/)
    assert.match(html, /not full-fidelity Office rendering/)
    assert.match(html, /default-src 'none'/)
    assert.ok(html.includes('&lt;script&gt;alert(1)&lt;/script&gt;&lt;img src=x onerror=alert(2)&gt;'))
    assert.doesNotMatch(html, /<(?:script|img|iframe|object|embed|link)\b/i)
    if (sourceFormat === 'docx') {
      assert.equal(preview.paragraphs, paragraphs.length)
      assert.match(html, /中文 😀/)
    } else {
      assert.equal(preview.sheets, 2)
      assert.equal(preview.cells, row.filter(value => value !== null).length)
      assert.equal(preview.formula_cells, 0)
      assert.match(html, /Sparse typed cells/)
      assert.match(html, /cached values that may be absent or stale/)
      assert.match(html, /=SUM\(A1:A2\)/)
    }
  }
  assert.deepEqual(await readFile(join(workspace, 'report.docx')), docxBytes)
  assert.deepEqual(await readFile(join(workspace, 'book.xlsx')), xlsxBytes)
  passed('DOCX/XLSX semantic HTML previews escape active markup and disclose fidelity/formula limits')

  const htmlBefore = await readFile(join(workspace, 'report.html'))
  await failure('office_preview', { path: 'report.docx', output: 'report.html' }, /overwriting/i)
  assert.deepEqual(await readFile(join(workspace, 'report.html')), htmlBefore)
  await writeFile(join(root, 'outside.html'), 'outside preview sentinel')
  await failure('office_preview', { path: 'report.docx', output: '../outside.html' }, /traversal/i)
  assert.equal(await readFile(join(root, 'outside.html'), 'utf8'), 'outside preview sentinel')
  passed('semantic previews cannot overwrite files or escape the workspace')

  await failure('docx_create', { path: 'report.docx', paragraphs: ['replacement'] }, /overwriting/i)
  assert.deepEqual(await readFile(join(workspace, 'report.docx')), docxBytes)
  await failure('xlsx_create', { path: 'book.xlsx', sheets: [{ name: 'Replacement', rows: [] }] }, /overwriting/i)
  assert.deepEqual(await readFile(join(workspace, 'book.xlsx')), xlsxBytes)
  passed('Rust overwrite errors reach DSH as model-visible failures without changing either file')

  await writeFile(join(root, 'outside.docx'), 'outside workspace sentinel')
  await failure('docx_create', { path: '../outside.docx', paragraphs: ['escape'] }, /traversal/i)
  await failure('docx_read', { path: '../outside.docx' }, /traversal/i)
  assert.equal(await readFile(join(root, 'outside.docx'), 'utf8'), 'outside workspace sentinel')
  passed('workspace escape is rejected on both create and read through the official bridge')

  await writeFile(join(workspace, 'invalid.docx'), 'not an Office ZIP')
  await failure('docx_read', { path: 'invalid.docx' }, /zip|archive/i)
  await failure('xlsx_read', { path: 'missing.xlsx' }, /inspect workspace-relative path/i)
  await failure('office_preview', { path: 'invalid.docx', output: 'invalid.html' }, /zip|archive/i)
  await assert.rejects(stat(join(workspace, 'invalid.html')), { code: 'ENOENT' })
  passed('malformed and missing files produce visible tool errors instead of successful content')

  await failure('docx_create', { path: 'bad.docx', paragraphs: 42 }, /invalid arguments.*expected a sequence/i)
  await failure('not_a_tool', {}, /not found|unknown tool/i)
  await assert.rejects(stat(join(workspace, 'bad.docx')), { code: 'ENOENT' })
  passed('invalid model arguments and unknown public tool names return DSH failures without creating files')

  assert.deepEqual((await success('docx_read', { path: 'report.docx' })).paragraphs, paragraphs)
  passed('the same DSH connection remains usable after failed calls')

  const registry = ctx.tools
  await ctx.fiber.dispose()
  assert.deepEqual(registry.schemas(), [], 'Disposal must unregister the MCP tools')
  passed('Cordis disposal closes the bridge and unregisters its tools')
  console.log(`Office official DSH client smoke passed (${checks} checks, ${sequence} tool calls; DSH 0.2.2-rc.1 / Cordis 4.1.0)`)
} finally {
  try {
    await ctx.fiber.dispose()
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}
