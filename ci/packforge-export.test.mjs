import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { createInterface } from 'node:readline'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { unzipSync } from 'fflate'

const worker = fileURLToPath(new URL('../src-tauri/packforge/generated/worker.cjs', import.meta.url))

async function invoke(operation, options) {
  const child = spawn(process.execPath, [worker], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
    signal: AbortSignal.timeout(45_000),
  })
  const lines = createInterface({ input: child.stdout })
  const stderr = []
  child.stderr.on('data', data => stderr.push(data))
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject)
    child.once('close', resolve)
  })
  child.stdin.write(JSON.stringify({ operation, options }) + '\n')
  let result
  let error
  for await (const line of lines) {
    const frame = JSON.parse(line)
    if (frame.kind === 'download') {
      child.stdin.write(JSON.stringify({ id: frame.id, error: '本测试使用离线样本' }) + '\n')
    } else if (frame.kind === 'result') {
      result = frame.value
      child.stdin.end()
    } else if (frame.kind === 'error') {
      error = frame.message
      child.stdin.end()
    }
  }
  const status = await exited
  assert.ifError(error ? new Error(error) : null)
  assert.equal(status, 0, Buffer.concat(stderr).toString())
  assert.ok(result)
  return result
}

async function fixture(t) {
  const workspace = await mkdtemp(join(tmpdir(), 'packforge-export-test-'))
  t.after(() => rm(workspace, { recursive: true, force: true }))
  const home = { name: 'example', dir: join(workspace, 'home') }
  const marker = 'synthetic-secret-test-value'
  async function write(relative, content) {
    const target = join(home.dir, relative)
    await mkdir(dirname(target), { recursive: true })
    await writeFile(target, content)
  }

  for (const name of ['alpha', 'beta', 'web']) {
    await write(`profiles/${name}/package.json`, JSON.stringify({ name, dsh: { profile: { bundles: [] } } }))
    await write(`profiles/${name}/cordis.patch.yml`, [
      '- id: llm-pi-ai',
      '  config:',
      '    providers:',
      `      ${name}:`,
      '        baseURL: https://models.example/v1',
      `        apiKeyEnv: ${name.toUpperCase()}_MODEL_KEY`,
      '- id: mcp-manager',
      '  config:',
      '    servers:',
      `      ${name}:`,
      '        transport: streamable-http',
      '        url: https://mcp.example/mcp',
      '        headers:',
      `          Authorization: Bearer ${marker}`,
      '- id: ordinary-row',
      '  config: { interval: 3 }',
      '',
    ].join('\n'))
    await write(`profiles/${name}/.npmrc`, `//example.test/:_authToken=${marker}`)
    await write(`profiles/${name}/.env`, `MODEL_KEY=${marker}`)
    await write(`profiles/${name}/notes.txt`, `notes for ${name}\n`)
  }
  await write('skills/review/SKILL.md', '---\nname: review\ndescription: Review a change\n---\nReview the selected change.\n')
  await write('skills/review/.dsh-skill.json', JSON.stringify({ repo: `https://${marker}@example.com/demo/skill` }))
  await write('cordis.patch.yml', [
    '- id: mcp-manager',
    '  config:',
    '    servers:',
    '      home-only:',
    '        transport: streamable-http',
    '        url: https://mcp.example/mcp',
    '        headers:',
    `          Authorization: Bearer ${marker}`,
    '- id: ordinary-home-row',
    '  config: { interval: 15 }',
    '',
  ].join('\n'))
  await write('AGENTS.md', 'Use the selected configuration.\n')
  await write('.credentials.yaml', `version: 1\nrefs:\n  MODEL_KEY: ${marker}\n`)
  await write('.dsh-pack/updates/baselines/profile.json', JSON.stringify({ marker }))

  return { workspace, home, marker, write }
}

function archiveJSON(entries, name) {
  assert.ok(entries[name], `缺少归档条目 ${name}`)
  return JSON.parse(Buffer.from(entries[name]).toString('utf8'))
}

async function archive(result, output) {
  const bytes = await readFile(output)
  assert.equal(result.size, bytes.length)
  assert.equal(result.sha256, createHash('sha256').update(bytes).digest('hex'))
  const entries = unzipSync(bytes)
  assert.deepEqual(archiveJSON(entries, 'dspack.json'), { format: 'dspack', version: 4 })
  const manifest = archiveJSON(entries, 'manifest.json')
  assert.equal(manifest.manifestVersion, 6)
  return { entries, manifest }
}

test('全局目录导出仅包含选中的配置档与资源，并剥离凭据与安装基线', async t => {
  const { workspace, home, marker } = await fixture(t)
  const options = {
    workspace,
    home,
    kind: 'dshhome',
    profiles: { alpha: { patch: true, lockfile: false, workspace: false, extraFiles: true } },
    homeContents: { agentsMd: true, presets: false, extraFiles: true },
    exportOptions: {
      defaultProfile: 'alpha',
      dshVersion: '0.2.3-alpha.2',
      dshVersions: ['0.2.3-alpha.2', '0.2.4'],
    },
  }
  const shown = await invoke('inspectExport', options)
  assert.deepEqual(Object.keys(shown.units), ['alpha'])
  assert.equal(shown.candidates.providers.length, 1)
  assert.equal(shown.candidates.providers[0].scope, 'profile:alpha')
  assert.doesNotMatch(JSON.stringify(shown), new RegExp(marker))
  const output = join(workspace, 'whole.dspack')
  const result = await invoke('export', {
    ...options,
    output,
    exportOptions: {
      ...options.exportOptions,
      selection: {
        providers: [shown.candidates.providers[0].key],
        skills: [{ key: 'skill:home:review', mode: 'embedded' }],
      },
    },
  })
  const { entries, manifest } = await archive(result, output)
  const readback = await invoke('inspect', {
    workspace,
    source: output,
    expectedSha256: result.sha256,
    expectedSize: result.size,
  })
  assert.ok(readback.valid)
  assert.equal(readback.manifest.defaultProfile, 'alpha')
  assert.equal(manifest.type, 'dshhome')
  assert.equal(manifest.defaultProfile, 'alpha')
  assert.deepEqual(manifest.dshVersions, options.exportOptions.dshVersions)
  assert.deepEqual(Object.keys(manifest.profiles), ['alpha'])
  assert.ok(entries['overrides/profiles/alpha/package.json'])
  assert.ok(entries['overrides/profiles/alpha/notes.txt'])
  assert.ok(entries['overrides/AGENTS.md'])
  assert.match(Buffer.from(entries['overrides/cordis.patch.yml']).toString('utf8'), /ordinary-home-row/)
  assert.deepEqual(archiveJSON(entries, 'provider.json').items.map(item => item.credentialRefs), [['ALPHA_MODEL_KEY']])
  assert.equal(archiveJSON(entries, 'skill.json').items[0].mode, 'embedded')
  assert.ok(Object.keys(entries).some(name => name.startsWith('extension-data/skill/')))
  assert.ok(!entries['mcp.json'])
  assert.ok(!Object.keys(entries).some(name => /(?:\b(?:beta|web)\b|\.npmrc|\.env|credentials|\.dsh-pack\/updates)/.test(name)))
  for (const bytes of Object.values(entries)) {
    assert.doesNotMatch(Buffer.from(bytes).toString('utf8'), new RegExp(marker))
  }
  assert.match(await readFile(join(home.dir, 'profiles', 'alpha', '.npmrc'), 'utf8'), new RegExp(marker))
})

test('单配置档关闭基础补丁后仍可导出所选资源', async t => {
  const { workspace, home, marker, write } = await fixture(t)
  const output = join(workspace, 'single.dspack')
  const profile = { name: 'web', dir: join(home.dir, 'profiles', 'web') }
  const icon = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jY4sAAAAASUVORK5CYII=', 'base64')
  await write('icons/example.png', icon)
  const options = {
    workspace,
    home,
    profile,
    kind: 'profile',
    iconPath: 'icons/example.png',
    contents: { patch: false, lockfile: false, workspace: false, extraFiles: false },
    exportOptions: { profileName: 'web', selection: { providers: ['provider:profile:web:web'] } },
    output,
  }
  const result = await invoke('export', options)
  const { entries, manifest } = await archive(result, output)
  assert.equal(manifest.type, 'profile')
  assert.equal(manifest.profileName, 'web')
  assert.equal(manifest.icon, 'icon.png')
  assert.deepEqual(Buffer.from(entries['icon.png']), icon)
  assert.ok(!manifest.patch)
  assert.ok(!entries['overrides/cordis.patch.yml'])
  assert.ok(!entries['overrides/notes.txt'])
  assert.ok(entries['provider.json'])
  assert.doesNotMatch(JSON.stringify(manifest), new RegExp(marker))
})

for (const kind of ['profile', 'dshhome']) {
  test(`私有路径排除规则约束 ${kind} 导出的额外文件与已选路径资源`, async t => {
    const { workspace, home, write } = await fixture(t)
    const plugin = '@example/data'
    for (const name of ['alpha', 'beta']) {
      await write(`profiles/${name}/package.json`, JSON.stringify({ name, dependencies: { [plugin]: '1.0.0' } }))
    }
    await write(`profiles/alpha/node_modules/${plugin}/dsh_pack/paths.json`, JSON.stringify([
      { name: '共享数据', paths: ['shared'] },
      { name: '本机历史', paths: ['shared/history'] },
    ]))
    await write(`profiles/beta/node_modules/${plugin}/dsh_pack/exclude.json`, JSON.stringify([
      'data/local.txt', 'shared/history/', 'icons/example.png', 'profiles/alpha/data/state.txt',
    ]))
    await write(`profiles/alpha/node_modules/${plugin}/dsh_pack/exclude.json`, JSON.stringify([
      './<profile>/data/private/',
    ]))
    await write('profiles/alpha/data/private/local.txt', 'local-only')
    await write('profiles/alpha/data/state.txt', 'local-only')
    await write('profiles/alpha/data/public.txt', 'shareable profile file')
    await write('icons/example.png', 'local-only')
    await write('data/local.txt', 'local-only')
    await write('data/common.txt', 'shareable')
    await write('shared/history/old.txt', 'local-only')
    await write('shared/public.txt', 'shareable resource')

    const options = kind === 'profile'
      ? { profile: { name: 'alpha', dir: join(home.dir, 'profiles', 'alpha') }, contents: { extraFiles: true, homeExtraFiles: true } }
      : { profiles: { alpha: { extraFiles: true } }, homeContents: { extraFiles: true } }
    const input = { workspace, home, kind, ...options }
    const { candidates } = await invoke('inspectExport', input)
    const output = join(workspace, kind + '.dspack')
    const result = await invoke('export', {
      ...input, output, iconPath: 'icons/example.png',
      exportOptions: { dshVersion: '0.2.3-alpha.2', selection: { paths: [candidates.paths[0].key] } },
    })
    const { entries } = await archive(result, output)
    const prefix = kind === 'profile' ? 'home' : 'overrides'
    assert.ok(entries[`${prefix}/data/common.txt`])
    assert.equal(entries[`${prefix}/data/local.txt`], undefined)
    const profilePrefix = kind === 'profile' ? 'overrides' : 'overrides/profiles/alpha'
    assert.ok(entries[`${profilePrefix}/data/public.txt`])
    assert.equal(entries[`${profilePrefix}/data/private/local.txt`], undefined)
    assert.equal(entries['icon.png'], undefined)
    assert.deepEqual(candidates.paths.map(item => item.name), ['共享数据'])
    const [resource] = archiveJSON(entries, 'path.json').plugins[plugin]
    assert.deepEqual(resource.files.map(file => file.path), ['shared/public.txt'])
    assert.equal(Buffer.from(entries[resource.files[0].entry]).toString(), 'shareable resource')
    for (const bytes of Object.values(entries)) {
      assert.ok(!Buffer.from(bytes).includes('local-only'))
    }
    assert.equal(await readFile(join(home.dir, 'data', 'local.txt'), 'utf8'), 'local-only')
  })
}

test('排除声明拒绝空数组和配置档保留路径', async t => {
  const { workspace, home, write } = await fixture(t)
  const plugin = '@example/data'
  const profile = { name: 'alpha', dir: join(home.dir, 'profiles', 'alpha') }
  const output = join(workspace, 'invalid.dspack')
  await write('profiles/alpha/package.json', JSON.stringify({ name: 'alpha', dependencies: { [plugin]: '1.0.0' } }))
  const declaration = `profiles/alpha/node_modules/${plugin}/dsh_pack/exclude.json`
  for (const [rules, error] of [
    [[], /排除声明须为非空路径数组/],
    [['<profile>/package.json'], /排除声明占用了保留路径/],
    [['<profile>/skills/review'], /排除声明占用了保留路径/],
  ]) {
    await write(declaration, JSON.stringify(rules))
    await assert.rejects(invoke('export', { workspace, home, kind: 'profile', profile, output }), error)
    await assert.rejects(readFile(output), { code: 'ENOENT' })
  }
})
