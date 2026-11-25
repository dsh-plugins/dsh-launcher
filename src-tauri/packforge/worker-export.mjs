import path from 'node:path';
import { exportChoices } from './vendor/core/src/inspect.js';
import { packProfile, packHome } from './vendor/core/src/pack.js';
import { scanProfile } from './vendor/core/src/scan.js';
import { segment } from './vendor/core/src/extension-files.js';
import { installedPluginSource, verifyVendored } from './vendor/core/src/registry.js';
import { publicManifest, safeImage, validateArchive } from './worker-archive.mjs';

const maximumBytes = 512 * 1024 * 1024;
const machinery = { 'cordis.patch.yml': 'patch', 'pnpm-lock.yaml': 'lockfile', 'pnpm-workspace.yaml': 'workspace' };

function inProfile(name, flags = {}) {
  if (name === 'package.json') return true;
  if (Object.hasOwn(machinery, name)) return flags[machinery[name]] !== false;
  return flags.extraFiles === true && !name.startsWith('skills/');
}

function inHome(name, flags = {}, single = false) {
  if (name === 'AGENTS.md') return flags.agentsMd === true;
  if (name.startsWith('.agent-presets/')) return flags.presets === true;
  if (name.startsWith('skills/')) return false;
  return flags[single ? 'homeExtraFiles' : 'extraFiles'] === true;
}

async function validateProfile(host, profile) {
  segment(profile.name);
  if (['__proto__', 'prototype', 'constructor'].includes(profile.name)) throw new Error('所选配置档使用了保留名称');
  if (!path.isAbsolute(profile.dir) || await host.packEntryType(profile.dir) !== 'dir' ||
      await host.packEntryType(path.join(profile.dir, 'package.json')) !== 'file') {
    throw new Error('目标配置档缺少普通 package.json：' + profile.name);
  }
}

function limitSnapshot(files) {
  if (files.length > 16384 || files.reduce((bytes, file) => bytes + file.size, 0) > maximumBytes) {
    throw new Error('选择的导出文件数量或大小超过限制');
  }
  return files.map(file => file.rel);
}

function ensurePortableSnapshot(entries) {
  const decoder = new TextDecoder();
  for (const [name, data] of Object.entries(entries)) {
    if (/(?:^|\/)(?:package\.json|pnpm-(?:workspace|lock)\.yaml|cordis\.patch\.yml)$/.test(name)) {
      const config = decoder.decode(data);
      if (/(?:https?:\/\/)[^\s@/]+@|[?&](?:token|password|signature|auth|x-amz-signature|api_?key)=|-----BEGIN [A-Z ]*PRIVATE KEY-----/i.test(config)) {
        if (name.endsWith('pnpm-lock.yaml')) delete entries[name];
        else throw new Error('配置文件含认证地址或凭据，请清除后重新导出：' + name);
      }
    }
  }
}

async function restoreVendored(host, dir, unit, manifest, entries) {
  const packagePath = path.join(dir, 'package.json');
  const raw = await host.readTextFile(packagePath);
  const pkg = raw ? JSON.parse(raw) : {};
  for (const id of Object.keys(pkg.dsh?.packforge?.pluginSources ?? {})) {
    const installed = installedPluginSource(pkg, id);
    if (!installed || installed.registry || installed.version !== unit.dependencies[id]) continue;
    const { sha256, version } = pkg.dsh.packforge.pluginSources[id];
    const entry = 'vendor/' + sha256 + '.tgz';
    const bytes = await host.readFile(path.join(dir, '.dspack-sources', sha256 + '.tgz'));
    const copy = { path: entry, sha256, version, size: bytes?.length ?? 0 };
    await verifyVendored(host, id, version, copy, { [entry]: bytes });
    manifest.vendored ??= Object.create(null);
    const existing = manifest.vendored[id];
    if (existing && existing.sha256 !== sha256) throw new Error('多个配置档的内嵌依赖不一致：' + id);
    manifest.vendored[id] = copy;
    entries[entry] = bytes;
  }
}

export async function runExport(host, options, inspect) {
  const single = options.kind === 'profile';
  const { profile, home, contents } = options;
  if (!['profile', 'dshhome'].includes(options.kind) || !home || !path.isAbsolute(home.dir) ||
      await host.packEntryType(home.dir) !== 'dir') throw new Error('整合包导出目录无效');
  if (single && !profile) throw new Error('请选择要导出的配置档');
  const selected = new Map(Object.entries(options.profiles ?? {}));
  if (!single && !selected.size) throw new Error('请至少选择一个配置档');
  const sources = single ? [profile] : [...selected.keys()].map(name => ({ name, dir: path.join(home.dir, 'profiles', name) }));
  for (const source of sources) await validateProfile(host, source);
  const overrides = options.exportOptions || {};
  if (inspect) return exportChoices(host, { profile: single ? profile : undefined, home,
    plugins: overrides.plugins, profilePlugins: overrides.profilePlugins, profileNames: new Set(selected.keys()) });
  if (!options.output || !path.isAbsolute(options.output)) throw new Error('整合包导出需要绝对文件路径');

  const opts = {
    ...overrides, output: options.output, out: path.dirname(options.output), home: home.dir,
    omitPatches: new Set(sources.filter(source => (single ? contents : selected.get(source.name))?.patch === false).map(source => source.name)),
    async prepareEntries(manifest, entries) {
      for (const source of sources) await restoreVendored(host, source.dir, single ? manifest : manifest.profiles[source.name], manifest, entries);
      if (options.iconFile) {
        const bytes = await host.readFile(options.iconFile);
        if (!safeImage('icon.png', bytes)) throw new Error('整合包头像不是有效的 PNG');
        manifest.icon = 'icon.png';
        entries['icon.png'] = bytes;
      }
      for (const [name, bytes] of Object.entries(entries)) {
        if (name === 'package.json' || /^overrides\/profiles\/[^/]+\/package\.json$/.test(name)) {
          const pkg = JSON.parse(new TextDecoder().decode(bytes));
          if (pkg.dsh?.packforge) delete pkg.dsh.packforge;
          const unit = single ? manifest : manifest.profiles[name.split('/')[2]];
          if (unit && pkg.dependencies) {
            for (const [id, release] of Object.entries(unit.dependencies)) {
              if (typeof pkg.dependencies[id] === 'string' && pkg.dependencies[id].startsWith('file:.dspack-sources/')) {
                pkg.dependencies[id] = release;
                for (const other of ['optionalDependencies','devDependencies']) {
                  if (pkg[other]?.[id]?.startsWith('file:.dspack-sources/')) pkg[other][id] = release;
                }
              }
            }
          }
          entries[name] = new TextEncoder().encode(JSON.stringify(pkg, null, 2) + '\n');
        }
        if (name.endsWith('/cordis.patch.yml') && name !== 'overrides/cordis.patch.yml') {
          const match = /^overrides\/profiles\/([^/]+)\/cordis\.patch\.yml$/.exec(name);
          if (match && manifest.profiles?.[match[1]]?.patch != null) delete entries[name];
        }
      }
      if (single && manifest.patch != null) delete entries['overrides/cordis.patch.yml'];
      ensurePortableSnapshot(entries);
      for (const unit of single ? [manifest] : Object.values(manifest.profiles)) {
        if (unit.patch && /https?:\/\/[^\s@/]+@|-----BEGIN [A-Z ]*PRIVATE KEY-----/i.test(unit.patch)) {
          throw new Error('基础补丁含认证地址或凭据，请清除后重新导出');
        }
      }
      limitSnapshot(Object.entries(entries).map(([rel, bytes]) => ({ rel, size: bytes.length })));
    },
    validateArchive: (manifest, entries) => validateArchive(host, manifest, entries),
  };
  const scannedHome = (await scanProfile(host, home.dir)).files;
  if (single) {
    const scannedProfile = (await scanProfile(host, profile.dir)).files;
    opts.include = limitSnapshot(scannedProfile.filter(file => inProfile(file.rel, contents)));
    opts.homeInclude = limitSnapshot(scannedHome.filter(file => !file.rel.startsWith('profiles/') && inHome(file.rel, contents, true)));
  } else {
    const machineFiles = new Set();
    for (const source of sources) machineFiles.add('profiles/' + source.name + '/package.json');
    const included = scannedHome.filter(file => {
      const matched = /^profiles\/([^/]+)\/(.*)$/.exec(file.rel);
      if (!matched) return inHome(file.rel, options.homeContents);
      return selected.has(matched[1]) && inProfile(matched[2], selected.get(matched[1]));
    });
    for (const file of included) machineFiles.delete(file.rel);
    if (machineFiles.size) throw new Error('所选配置档缺少可导出的 package.json');
    opts.include = limitSnapshot(included);
  }
  const result = single ? await packProfile(host, profile, opts) : await packHome(host, home, opts);
  return { output: result.output, manifest: publicManifest(result.manifest), sha256: result.sha256, size: result.size };
}
