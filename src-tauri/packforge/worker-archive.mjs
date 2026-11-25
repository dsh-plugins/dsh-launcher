import path from 'node:path';
import { Buffer } from 'node:buffer';
import { isExcluded } from './vendor/core/src/security.js';
import { validatedItems } from './vendor/core/src/extension-validate.js';
import { digestFiles, expandTemplate, segment } from './vendor/core/src/extension-files.js';
import { exactNpm, verifyVendored } from './vendor/core/src/registry.js';
import validVersion from 'semver/functions/valid.js';

const files = new Set(['package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml']);
const maximumIcons = 2 * 1024 * 1024;
const reservedProfiles = ['web', 'headless'];

function portablePath(value) {
  if (typeof value !== 'string' || !value || value.length > 1024 || value.split('/').some(part =>
    !part || part === '.' || part === '..' || /[\\:\x00-\x1f<>|?*"]|[. ]$/.test(part) ||
    /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) {
    throw new Error('整合包归档含非法路径');
  }
}

export function publicManifest(manifest) {
  if (!manifest) return null;
  const { patch, profiles, ...value } = manifest;
  if (profiles) value.profiles = Object.fromEntries(Object.entries(profiles).map(([name, { patch, ...unit }]) => [name, unit]));
  return value;
}

export function safeImage(name, bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length === 0 || bytes.length > 256 * 1024) return null;
  const data = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length);
  let mime;
  switch (path.extname(name).toLowerCase()) {
    case '.png':
      if (data.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) mime = 'image/png';
      break;
    case '.jpg': case '.jpeg':
      if (data[0] === 0xff && data[1] === 0xd8) mime = 'image/jpeg';
      break;
    case '.webp':
      if (data.toString('ascii', 0, 4) === 'RIFF' && data.toString('ascii', 8, 12) === 'WEBP') mime = 'image/webp';
      break;
    case '.svg': {
      const svg = data.toString('utf8');
      if (!/<svg(?:\s|>)/i.test(svg) || /<!DOCTYPE|<!ENTITY|<\?xml-stylesheet|<(?:script|foreignObject|iframe|object|image|feImage)\b|\son[a-z]+\s*=|@import/i.test(svg)) break;
      if ([...svg.matchAll(/(?:\bhref\s*=\s*['"]|\burl\(\s*['"]?)([^'"\s)]*)/ig)].some(match => !match[1].startsWith('#'))) break;
      mime = 'image/svg+xml';
      break;
    }
  }
  return mime ? `data:${mime};base64,${data.toString('base64')}` : null;
}

export function previewIcons(entries) {
  let size = 0;
  const result = Object.create(null);
  for (const [name, bytes] of Object.entries(entries)) {
    const uri = safeImage(name, bytes);
    if (uri && size + uri.length <= maximumIcons) {
      result[name] = uri;
      size += uri.length;
    }
  }
  return result;
}

function targetTracker() {
  const tracked = new Map();
  return {
    add(target) {
      portablePath(target);
      const key = target.toLowerCase();
      const parts = key.split('/');
      const original = target.split('/');
      for (let i = 1; i < parts.length; i++) {
        const parent = parts.slice(0, i).join('/');
        const parentName = original.slice(0, i).join('/');
        const previous = tracked.get(parent);
        if (previous?.exact || previous && previous.name !== parentName) {
          throw new Error('整合包包含重复或重叠的目标：' + target);
        }
        if (!previous) tracked.set(parent, { exact: false, name: parentName });
      }
      if (tracked.has(key)) throw new Error('整合包包含重复或重叠的目标：' + target);
      tracked.set(key, { exact: true, name: target });
    },
  };
}

/** 核对归档实际资源，安装时使用选定的配置档名称重新计算目标。 */
export async function validateArchive(host, manifest, entries, options = {}) {
  if (manifest?.manifestVersion !== 6) return;
  const whole = manifest.type === 'dshhome';
  const template = manifest.profileName ?? manifest.name;
  const profile = options.targetProfileName ?? (reservedProfiles.includes(template.toLowerCase()) ? 'pack' : template);
  if (!whole) segment(profile);
  if (manifest.dshVersion && !validVersion(manifest.dshVersion) || manifest.dshVersions !== undefined &&
      (!Array.isArray(manifest.dshVersions) || !manifest.dshVersions.length ||
       manifest.dshVersions.some(version => typeof version !== 'string' || !validVersion(version)) ||
       new Set(manifest.dshVersions).size !== manifest.dshVersions.length ||
       manifest.dshVersion && !manifest.dshVersions.includes(manifest.dshVersion))) {
    throw new Error('整合包 DSH 兼容版本声明无效');
  }
  const target = targetTracker();
  const scopes = whole ? Object.keys(manifest.profiles || {}) : [profile];
  if (new Set(scopes.map(name => name.toLowerCase())).size !== scopes.length) throw new Error('配置档名称存在大小写冲突');
  for (const name of scopes) {
    segment(name);
    if (reservedProfiles.includes(name.toLowerCase()) || ['__proto__', 'prototype', 'constructor'].includes(name)) {
      throw new Error('整合包不能占用保留的配置档名称');
    }
  }
  const entriesCase = new Set();
  for (const [name, bytes] of Object.entries(entries)) {
    portablePath(name.endsWith('/') ? name.slice(0, -1) : name);
    if (entriesCase.has(name.toLowerCase())) throw new Error('归档条目存在大小写冲突：' + name);
    entriesCase.add(name.toLowerCase());
    if (!name.includes('/') && name !== 'manifest.json' && isExcluded(name)) {
      throw new Error('归档含有受保护的本机文件：' + name);
    }
    if (name.startsWith('assets/plugins/') && !name.endsWith('/') && !safeImage(name, bytes)) {
      throw new Error('整合包插件图标格式或内容无效：' + name);
    }
    if (name.startsWith('extension-data/') && await host.sha256(bytes) !== name.split('/').at(-1)) {
      throw new Error('扩展文件 SHA-256 不符：' + name);
    }
    if (name.endsWith('/')) continue;
    let output;
    if (name.startsWith('overrides/')) output = (whole ? '' : `profiles/${profile}/`) + name.slice('overrides/'.length);
    else if (name.startsWith('home/')) {
      if (whole || name.toLowerCase().startsWith('home/profiles/')) throw new Error('整合包包含无效的 home 快照：' + name);
      output = name.slice('home/'.length);
    } else if (!whole && files.has(name)) output = `profiles/${profile}/${name}`;
    if (!output) continue;
    if (isExcluded(output) || /(?:^|\/)\.?pnpmfile\.[cm]?js$/i.test(output) ||
        whole && output.toLowerCase().startsWith('profiles/') && (!output.startsWith('profiles/') || !scopes.includes(output.split('/')[1]))) {
      throw new Error('整合包试图写入受保护目录或未声明的配置档：' + output);
    }
    target.add(output);
  }
  for (const file of manifest.files ?? []) {
    const output = (whole ? '' : `profiles/${profile}/`) + file.path;
    if (isExcluded(output) || /(?:^|\/)(?:package\.json|cordis\.patch\.yml|pnpm-(?:workspace|lock)\.yaml)$/i.test(output)) {
      throw new Error('外部下载文件占用了受管路径：' + output);
    }
    target.add(output);
  }
  for (const record of validatedItems(manifest, entries)) {
    if (record.kind === 'path') {
      const name = !whole || record.scope === 'home' ? profile : record.scope.slice(8);
      const roots = record.paths.map(template => expandTemplate(template, name));
      for (const file of [...record.files, ...(record.directories ?? [])]) {
        target.add(roots[file.index] + (file.tail ? '/' + file.tail : ''));
      }
    }
    if (record.kind === 'skill') {
      const root = record.scope === 'home' ? 'skills' : `.dsh-pack/skills/${record.scope.slice(8)}`;
      const destination = `${root}/${record.id}`;
      target.add(destination);
      if (record.mode === 'embedded') {
        const entriesForSkill = new Map(record.files.map(file => [file.path, entries[file.entry]]));
        if (await digestFiles(host, entriesForSkill) !== record.digest) throw new Error('技能文件集合摘要不符：' + record.id);
      }
    }
  }
  if (manifest.vendored !== undefined) {
    if (!manifest.vendored || Array.isArray(manifest.vendored) || typeof manifest.vendored !== 'object') throw new Error('内嵌依赖清单无效');
    const pins = new Set();
    for (const [id, copy] of Object.entries(manifest.vendored)) {
      if (!copy || !exactNpm(id, copy.version) || !scopes.some(name => (whole ? manifest.profiles[name] : manifest).dependencies?.[id] === copy.version)) {
        throw new Error('内嵌依赖的坐标未在配置档中声明：' + id);
      }
      const entry = copy.path;
      if (typeof entry !== 'string' || !entry.startsWith('vendor/') || !entry.endsWith('.tgz') || pins.has(entry)) {
        throw new Error('内嵌依赖归档路径无效：' + id);
      }
      pins.add(entry);
      await verifyVendored(host, id, copy.version, copy, entries);
    }
    for (const name of Object.keys(entries)) {
      if (name.startsWith('vendor/') && !name.endsWith('/') && !pins.has(name)) throw new Error('归档含有未声明的内嵌依赖：' + name);
    }
  } else if (Object.keys(entries).some(name => name.startsWith('vendor/') && !name.endsWith('/'))) {
    throw new Error('归档含有未声明的内嵌依赖');
  }
}
