import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { zipSync } from 'fflate';
import { parseDocument, stringify } from 'yaml';
import { NodeHost } from './vendor/host-node/src/index.js';
import { checkMcpInstall } from './vendor/core/src/mcp-install.js';

const worker = fileURLToPath(new URL('./generated/worker.cjs', import.meta.url));
const buffer = text => Buffer.from(text, 'utf8');
const json = value => buffer(JSON.stringify(value));
const digest = value => createHash('sha256').update(value).digest('hex');

async function workspace(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'launcher-packforge-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  return root;
}

async function run(operation, options, handle = () => ({})) {
  const proc = spawn(process.execPath, [worker], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  const exit = once(proc, 'close');
  let stderr = '', result, error;
  proc.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const send = value => proc.stdin.write(JSON.stringify(value) + '\n');
  send({ operation, options });
  for await (const line of createInterface({ input: proc.stdout })) {
    const message = JSON.parse(line);
    if (message.kind === 'result') result = message.value;
    if (message.kind === 'error') error = message.message;
    if (message.id) send({ id: message.id, ...await handle(message) });
  }
  const [status] = await exit;
  if (error) throw new Error(error);
  if (status || !result) throw new Error(stderr || 'worker exited without result');
  return result;
}

async function archive(root, version, files = {}, fields = {}) {
  const source = path.join(root, `pack-${version}.dspack`);
  const manifest = { manifestVersion: 6, type: 'profile', name: 'example-pack', version,
    profileName: 'web', bundles: [], dependencies: {}, ...fields };
  const bytes = zipSync({ 'manifest.json': json(manifest), 'dspack.json': json({ format: 'dspack', version: 4 }), ...files });
  await fs.writeFile(source, bytes);
  return { source, expectedSha256: digest(bytes), expectedSize: bytes.length };
}

function npmTarball(name, version) {
  const body = json({ name, version });
  const header = Buffer.alloc(512);
  header.write('package/package.json');
  header.write('0000644\0', 100);
  header.write(body.length.toString(8).padStart(11, '0') + '\0', 124);
  header[156] = 48;
  header.write('ustar\0', 257);
  header.fill(32, 148, 156);
  header.write([...header].reduce((a,b) => a + b, 0).toString(8).padStart(6, '0') + '\0 ', 148);
  return gzipSync(Buffer.concat([header, body, Buffer.alloc(1536 - body.length)]));
}

test('在写入前拒绝错误摘要、路径冲突和错误的清单容器配对', async t => {
  const root = await workspace(t);
  const source = await archive(root, '1.0.0');
  const loaded = await run('inspect', { workspace: root, ...source });
  assert.equal(loaded.valid, true);
  assert.equal(loaded.containerVersion, 4);
  await assert.rejects(run('inspect', { workspace: root, ...source, expectedSha256: '0'.repeat(64) }), /校验失败/);

  const badPaths = await archive(root, '1.0.1', { 'overrides/Notes/main.txt': buffer('a'), 'overrides/notes/other.txt': buffer('b') });
  await assert.rejects(run('inspect', { workspace: root, ...badPaths }), /重复或重叠/);
  const token = await archive(root, '1.0.2', { '.npmrc': buffer('//example.invalid/:_authToken=fixture') });
  await assert.rejects(run('inspect', { workspace: root, ...token }), /受保护/);
  const badMarker = await archive(root, '1.0.3', { 'dspack.json': json({ format: 'dspack', version: 3 }) });
  await assert.rejects(run('inspect', { workspace: root, ...badMarker }), /配套/);
});

test('安装写入来源基线，发布失败恢复原文件，并拒绝过期预览和其他来源', async t => {
  const root = await workspace(t);
  const home = path.join(root, 'home'), profilesRoot = path.join(home, 'profiles');
  const profile = path.join(profilesRoot, '中文档');
  const pnpm = path.join(root, 'pnpm.cjs');
  await fs.writeFile(pnpm, 'process.exitCode = 0;\n');
  const options = { workspace: root, home, profilesRoot, name: '中文档', pnpm, sourceIdentity: 'market:author.example' };
  const initial = await archive(root, '1.0.0', { 'overrides/readme.md': buffer('first') });
  const create = await run('preview', { ...initial, ...options });
  let publication;
  await run('apply', { ...initial, ...options, previewToken: create.previewToken }, message => { publication = message; return {}; });
  assert.equal(await fs.readFile(path.join(profile, 'readme.md'), 'utf8'), 'first');
  const baseline = JSON.parse(await fs.readFile(publication.baseline, 'utf8'));
  assert.equal(baseline.archiveSha256, initial.expectedSha256);
  assert.equal(baseline.transaction, publication.transaction);
  assert.equal(baseline.sourceIdentity, options.sourceIdentity);
  assert.equal(baseline.profileName, options.name);
  assert.equal(baseline.packType, 'profile');

  const next = await archive(root, '1.1.0', { 'overrides/readme.md': buffer('next') });
  const update = { ...next, ...options, update: true };
  await assert.rejects(run('preview', { ...update, sourceIdentity: 'market:other.example' }), /来源/);
  const plan = await run('preview', update);
  await assert.rejects(run('apply', { ...update, previewToken: plan.previewToken }, () => ({ error: 'refused' })), /请求/);
  assert.equal(await fs.readFile(path.join(profile, 'readme.md'), 'utf8'), 'first');
  assert.deepEqual(JSON.parse(await fs.readFile(publication.baseline, 'utf8')), baseline);
  await assert.rejects(run('apply', { ...update, previewToken: plan.previewToken }, () => ({ kind: 'cancel' })), /取消/);
  assert.equal(await fs.readFile(path.join(profile, 'readme.md'), 'utf8'), 'first');
  await fs.writeFile(path.join(profile, 'readme.md'), 'locally modified');
  await assert.rejects(run('apply', { ...update, previewToken: plan.previewToken }), /重新预览/);
});

test('核验未指定专用源的内嵌依赖并将已验证副本固定到配置档中', async t => {
  const root = await workspace(t);
  const home = path.join(root, 'home'), profilesRoot = path.join(home, 'profiles');
  const pnpm = path.join(root, 'pnpm.cjs');
  await fs.writeFile(pnpm, 'process.exitCode = 0;\n');
  const bytes = npmTarball('example-lib', '1.2.3');
  const sha256 = digest(bytes), entry = 'vendor/lib.tgz';
  const metadata = { dependencies: { 'example-lib': '1.2.3' },
    vendored: { 'example-lib': { path: entry, size: bytes.length, version: '1.2.3', sha256 } } };
  const source = await archive(root, '1.0.0', { [entry]: bytes }, metadata);
  const options = { ...source, workspace: root, home, profilesRoot, name: 'pack', pnpm, sourceIdentity: 'local:example' };
  const plan = await run('preview', options);
  await run('apply', { ...options, previewToken: plan.previewToken });
  const profile = path.join(profilesRoot, 'pack');
  const pkg = JSON.parse(await fs.readFile(path.join(profile, 'package.json'), 'utf8'));
  assert.equal(pkg.dependencies['example-lib'], `file:.dspack-sources/${sha256}.tgz`);
  assert.deepEqual(await fs.readFile(path.join(profile, '.dspack-sources', sha256 + '.tgz')), bytes);

  const wrong = npmTarball('wrong-lib', '1.2.3');
  const forged = await archive(root, '1.1.0', { [entry]: wrong }, { ...metadata,
    vendored: { 'example-lib': { ...metadata.vendored['example-lib'], size: wrong.length, sha256: digest(wrong) } } });
  await assert.rejects(run('inspect', { workspace: root, ...forged }), /身份不符/);
});

test('专用包源使用目标凭据，重定向后仅对授权地址发送认证头', async t => {
  const root = await workspace(t);
  const home = path.join(root, 'home'), profilesRoot = path.join(home, 'profiles'), profile = path.join(profilesRoot, 'pack');
  await fs.mkdir(profile, { recursive: true });
  await fs.writeFile(path.join(profile, '.npmrc'), '//registry.example/private/:_authToken=fixture\n');
  const name = 'example-lib', version = '1.2.3';
  const bytes = npmTarball(name, version);
  const metadata = { dependencies: { [name]: version }, plugins: {
    [name]: { name: 'Example', source: { type: 'npm', registry: 'https://registry.example/private/' } },
  } };
  const source = await archive(root, '1.0.0', {}, metadata);
  const seen = [];
  const ready = await run('preview', { ...source, workspace: root, home, profilesRoot, name: 'pack', update: true,
    sourceIdentity: 'local:example' }, async frame => {
    assert.equal(frame.kind, 'download');
    seen.push([frame.url, frame.headers.authorization ?? null]);
    if (frame.url.endsWith('/example-lib/1.2.3')) {
      const dist = { tarball: 'https://registry.example/private/package.tgz',
        integrity: 'sha512-' + createHash('sha512').update(bytes).digest('base64') };
      await fs.writeFile(frame.path, json({ name, version, dist }));
    } else if (frame.url.startsWith('https://registry.example/')) {
      return { status: 302, location: 'https://cdn.example/package.tgz' };
    } else await fs.writeFile(frame.path, bytes);
    return { status: 200 };
  });
  assert.equal(ready.unknownBase, true);
  assert.deepEqual(seen, [
    ['https://registry.example/private/example-lib/1.2.3', 'Bearer fixture'],
    ['https://registry.example/private/package.tgz', 'Bearer fixture'],
    ['https://cdn.example/package.tgz', null],
  ]);
});

test('导出按配置档、作用域和精确版本预填专用源，允许修改或清空', async t => {
  const root = await workspace(t);
  const home = { name: 'example', dir: path.join(root, 'home') };
  const dependencies = { '@org/plugin': '1.2.3', plain: '2.0.0', '@else/plugin': '3.0.0',
    '@env/plugin': '1.0.0', '@auth/plugin': '1.0.0', '@query/plugin': '1.0.0',
    range: '^1.0.0', local: 'file:../local' };
  async function profile(name, registries) {
    const dir = path.join(home.dir, 'profiles', name);
    await fs.mkdir(dir, { recursive: true });
    await fs.writeFile(path.join(dir, 'package.json'), json({ name, dependencies }));
    if (registries) await fs.writeFile(path.join(dir, 'pnpm-workspace.yaml'), stringify({ registries }));
    return { name, dir };
  }
  const first = await profile('alpha', { default: 'https://default.example/npm', '@org': 'https://alpha.example',
    '@env': 'https://env.example/${TOKEN}/', '@auth': 'https://user:pass@auth.example/',
    '@query': 'https://query.example/?token=fixture' });
  await profile('beta', { 'https://beta.example/npm': { scopes: ['@org'] },
    'https://fallback.example': { scopes: ['@'] } });
  await profile('gamma');
  const multiple = { workspace: root, home, kind: 'dshhome',
    profiles: { alpha: { workspace: false }, beta: {}, gamma: {} } };
  const { units } = await run('inspectExport', multiple);
  const registry = (unit, id) => unit.plugins?.[id]?.source?.registry;
  assert.equal(registry(units.alpha, '@org/plugin'), 'https://alpha.example/');
  assert.equal(registry(units.beta, '@org/plugin'), 'https://beta.example/npm/');
  assert.equal(registry(units.alpha, 'plain'), 'https://default.example/npm/');
  assert.equal(registry(units.alpha, '@else/plugin'), 'https://default.example/npm/');
  assert.equal(registry(units.beta, 'plain'), 'https://fallback.example/');
  assert.equal(registry(units.gamma, '@org/plugin'), undefined);
  for (const id of ['@env/plugin', '@auth/plugin', '@query/plugin', 'range', 'local']) {
    assert.equal(registry(units.alpha, id), undefined);
  }
  const single = { workspace: root, kind: 'profile', home, profile: first, contents: { workspace: false } };
  const inspected = await run('inspectExport', single);
  assert.deepEqual(inspected.units.alpha, units.alpha);
  const profilePlugins = { alpha: {
    '@org/plugin': { version: '1.2.3', source: { type: 'npm', registry: 'https://manual.example/' } },
    plain: { version: '2.0.0', source: null },
  } };
  const output = path.join(root, 'with-registries.dspack');
  await run('export', { ...multiple, output, exportOptions: { profilePlugins } });
  const { manifest } = await run('inspect', { workspace: root, source: output });
  assert.equal(registry(manifest.profiles.alpha, '@org/plugin'), 'https://manual.example/');
  assert.equal(registry(manifest.profiles.alpha, 'plain'), undefined);
  assert.equal(registry(manifest.profiles.alpha, '@else/plugin'), 'https://default.example/npm/');
  assert.equal(registry(manifest.profiles.beta, '@org/plugin'), 'https://beta.example/npm/');
  await fs.writeFile(path.join(first.dir, 'pnpm-workspace.yaml'), 'registries: { default: *missing }');
  const malformed = await run('inspectExport', single);
  assert.equal(registry(malformed.units.alpha, 'plain'), undefined);
});

test('整机导出保留共享内嵌副本，已验证的安装来源优先于工作区配置', async t => {
  const root = await workspace(t);
  const home = { name: 'example', dir: path.join(root, 'home') };
  const version = '1.2.3', embedded = 'embedded-lib', pinned = 'pinned-lib';
  const savedDir = path.join(home.dir, 'profiles', 'saved');
  const derivedDir = path.join(home.dir, 'profiles', 'derived');
  const dependencies = {}, pluginSources = {};
  await fs.mkdir(path.join(savedDir, '.dspack-sources'), { recursive: true });
  await fs.mkdir(derivedDir, { recursive: true });
  for (const [name, registry] of [[embedded, null], [pinned, 'https://saved.example/']]) {
    const bytes = npmTarball(name, version), sha256 = digest(bytes);
    dependencies[name] = 'file:.dspack-sources/' + sha256 + '.tgz';
    pluginSources[name] = { version, registry, sha256 };
    await fs.writeFile(path.join(savedDir, '.dspack-sources', sha256 + '.tgz'), bytes);
  }
  await fs.writeFile(path.join(savedDir, 'package.json'), json({ dependencies, dsh: { packforge: { pluginSources } } }));
  await fs.writeFile(path.join(derivedDir, 'package.json'), json({ dependencies: { [embedded]: version } }));
  for (const dir of [savedDir, derivedDir]) {
    await fs.writeFile(path.join(dir, 'pnpm-workspace.yaml'), 'registries: { default: https://workspace.example/ }');
  }
  const single = await run('inspectExport', { kind: 'profile', workspace: root, home, profile: { name: 'derived', dir: derivedDir } });
  assert.equal(single.units.derived.plugins[embedded].source.registry, 'https://workspace.example/');
  const options = { kind: 'dshhome', workspace: root, home, profiles: { derived: {}, saved: {} } };
  const shown = await run('inspectExport', options);
  const output = path.join(root, 'shared.dspack');
  await run('export', { ...options, output });
  const { manifest } = await run('inspect', { workspace: root, source: output });
  assert.equal(manifest.vendored[embedded].sha256, pluginSources[embedded].sha256);
  for (const units of [shown.units, manifest.profiles]) {
    assert.equal(units.saved.plugins[pinned].source.registry, 'https://saved.example/');
    assert.equal(units.saved.plugins[embedded].source, undefined);
    assert.equal(units.derived.plugins?.[embedded]?.source, undefined);
  }
});

test('配置档切换到公共源后，预填地址仍保留私有安装包的本地图标', async t => {
  const root = await workspace(t);
  const home = { name: 'example', dir: path.join(root, 'home') };
  const profile = { name: 'example', dir: path.join(home.dir, 'profiles', 'example') };
  const id = '@org/plugin', version = '1.0.0';
  const installed = path.join(profile.dir, 'node_modules', id);
  await fs.mkdir(installed, { recursive: true });
  await fs.writeFile(path.join(profile.dir, 'package.json'), json({ dependencies: { [id]: version } }));
  await fs.writeFile(path.join(profile.dir, 'pnpm-workspace.yaml'), 'registries: { default: https://registry.npmjs.org/ }');
  await fs.writeFile(path.join(installed, 'package.json'), json({
    name: id, version, icon: 'icon.svg', _resolved: 'https://private.example/plugin.tgz',
  }));
  await fs.writeFile(path.join(installed, 'icon.svg'), '<svg xmlns="http://www.w3.org/2000/svg"></svg>');
  const requests = [];
  const shown = await run('inspectExport', { workspace: root, kind: 'profile', home, profile }, message => {
    requests.push(message.url);
    return { error: '没有公共包源记录' };
  });
  const plugin = shown.units.example.plugins[id];
  assert.equal(plugin.source.registry, 'https://registry.npmjs.org/');
  assert.match(plugin.icon, /^assets\/plugins\/[a-f0-9]{64}\.svg$/);
  assert.deepEqual(requests, []);
});

test('配置档级技能添加独立提供方，保留原技能目录且导出时移除本机注册', async t => {
  const root = await workspace(t);
  const home = path.join(root, 'home');
  const pnpm = path.join(root, 'pnpm.cjs');
  await fs.writeFile(pnpm, 'process.exitCode = 0;\n');
  const bytes = buffer('---\nname: example\ndescription: Example\n---\n# Example\n');
  const sha256 = digest(bytes), entry = 'extension-data/skill/' + sha256;
  const record = { kind: 'skill', id: 'example', key: 'skill:profile:pack:example', scope: 'profile:pack', name: 'Example',
    mode: 'embedded', digest: digest(json([['SKILL.md', sha256]])), files: [{ path: 'SKILL.md', entry, sha256 }] };
  const patch = '- id: skill-filesystem\n  config:\n    customSkillDirs:\n      - local-custom\n';
  const fields = { type: 'dshhome', dshVersion: '0.2.3', dshVersions: ['0.2.3', '0.2.2'], defaultProfile: 'pack',
    profiles: { pack: { bundles: [], dependencies: {}, patch } } };
  const source = await archive(root, '1.0.0', { 'skill.json': json({ items: [record] }), [entry]: bytes }, fields);
  const opts = { ...source, workspace: root, home, pnpm, sourceIdentity: 'local:skill',
    selectedDshVersion: '0.2.2', installedDshVersions: ['0.2.2'] };
  await assert.rejects(run('preview', { ...opts, selectedDshVersion: '0.2.1' }), /兼容版本/);
  const plan = await run('preview', opts);
  await run('apply', { ...opts, previewToken: plan.previewToken });
  assert.deepEqual(await fs.readFile(path.join(home, '.dsh-pack', 'skills', 'pack', 'example', 'SKILL.md')), bytes);
  const written = parseDocument(await fs.readFile(path.join(home, 'profiles', 'pack', 'cordis.patch.yml'), 'utf8'));
  assert.equal(written.getIn([0, 'config', 'customSkillDirs', 0]), 'local-custom');
  const custom = written.getIn([1, 'insert', 0, 'config', 'customSkillDirs', 0], true);
  assert.equal(custom.tag, 'tag:yaml.org,2002:js');
  assert.match(custom.value, /\.dsh-pack\/skills\/pack/);

  const output = path.join(root, 'scoped-export.dspack');
  await run('export', { workspace: root, kind: 'dshhome', home: { name: 'home', dir: home }, profiles: { pack: {} },
    exportOptions: { name: 'example', selection: { skills: [record.key] } }, output });
  const zip = await fs.readFile(output);
  const { unzipSync } = await import('fflate');
  const entries = unzipSync(zip);
  const manifest = JSON.parse(Buffer.from(entries['manifest.json']).toString());
  assert.match(manifest.profiles.pack.patch, /local-custom/);
  assert.doesNotMatch(manifest.profiles.pack.patch, /dsh-launcher-pack-skills/);
});

test('显式离线的 MCP 预览不会自动发现执行前新安装的 DSH', async t => {
  const home = await workspace(t);
  const host = new NodeHost();
  host.env = () => { throw new Error('尝试重新发现本机 DSH 安装'); };
  const options = { home, profileName: 'pack', manifest: { type: 'profile', bundles: [] } };
  const actions = [{ item: { key: 'mcp:profile:pack:test' },
    mcp: { id: 'test', scope: 'pack', server: 'test', owner: 'client:test' } }];
  assert.equal(typeof await checkMcpInstall(host, { ...options, installAnchor: null }, actions), 'function');
  await assert.rejects(checkMcpInstall(host, options, actions), /重新发现/);
});
