import { Gunzip } from 'fflate';
import { parseDocument } from 'yaml';
import validRange from 'semver/ranges/valid.js';
import satisfies from 'semver/functions/satisfies.js';

const npmName = /^(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/;
const version = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:(?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*))*))?(?:\+[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)?$/;
const decoder = new TextDecoder();
const maximumTarballBytes = 256 * 1024 * 1024;

/** 专用源只绑定当前 profile 中完整、确定的 npm 坐标。 */
export function exactNpm(id, value) {
  return typeof id === 'string' && npmName.test(id) && typeof value === 'string' && version.test(value);
}

/** 分发文件里只保存服务地址，认证信息由安装机提供。 */
export function registryUrl(raw) {
  if (typeof raw !== 'string' || !raw || raw !== raw.trim()) {
    throw new Error('registry 必须是 HTTP(S) 地址');
  }
  const url = new URL(raw);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || raw.includes('?') || raw.includes('#') || /[\\\s]/.test(raw)) {
    throw new Error('registry 必须是无凭据、查询参数或片段的 HTTP(S) 地址');
  }
  return url.href.replace(/\/?$/, '/');
}

/** 本机 npm 认证项按请求域和路径匹配，私库令牌不发送到其余域名。 */
async function registryHeaders(host, url, authDir) {
  if (new URL(url).protocol !== 'https:') {
    return {};
  }
  const env = (key) => typeof host.env === 'function' ? host.env(key) : null;
  const config = env('NPM_CONFIG_USERCONFIG') || env('npm_config_userconfig') || host.joinPath(host.homedir(), '.npmrc');
  const paths = [config];
  if (authDir) {
    paths.push(host.joinPath(authDir, '.npmrc'));
  }
  const raw = (await Promise.all([...new Set(paths)].map((path) => host.readTextFile(path)))).join('\n');
  const uri = new URL(url);
  const request = '//' + uri.host + uri.pathname;
  let match = null;
  for (const line of (raw || '').split(/\r?\n/)) {
    const parsed = line.trim().match(/^(\/\/[^=]*?\/):(_authToken|_auth)\s*=\s*(.+)$/);
    if (!parsed || !request.startsWith(parsed[1]) || (match && parsed[1].length < match.prefix.length)) {
      continue;
    }
    const token = parsed[3].replace(/^(['"])(.*)\1$/, '$2').replace(/\$\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g, (_, key) => env(key) || '');
    if (token && !/[\r\n]/.test(token)) {
      match = { prefix: parsed[1], value: (parsed[2] === '_auth' ? 'Basic ' : 'Bearer ') + token };
    }
  }
  return match ? { Authorization: match.value } : {};
}

function tarballPackage(bytes) {
  if (bytes.length < 18 || bytes.length > maximumTarballBytes) throw new Error('内嵌依赖归档大小无效');
  let length = 0;
  const chunks = [];
  const stream = new Gunzip(chunk => {
    length += chunk.length;
    if (length > maximumTarballBytes) throw new Error('依赖归档解压大小超过限制');
    chunks.push(chunk);
  });
  for (let offset = 0; offset < bytes.length; offset += 16 * 1024) {
    stream.push(bytes.subarray(offset, offset + 16 * 1024), offset + 16 * 1024 >= bytes.length);
  }
  const tar = new Uint8Array(length);
  let written = 0;
  for (const chunk of chunks) {
    tar.set(chunk, written);
    written += chunk.length;
  }
  let pkg;
  for (let offset = 0; offset + 512 <= tar.length;) {
    const header = tar.subarray(offset, offset + 512);
    const filename = decoder.decode(header.subarray(0, 100)).replace(/\0.*$/s, '');
    const size = Number.parseInt(decoder.decode(header.subarray(124, 136)).replace(/\0.*$/s, '').trim(), 8);
    if (!filename || !Number.isSafeInteger(size) || size < 0 || offset + 512 + size > tar.length) {
      break;
    }
    offset += 512;
    if (filename === 'package/package.json' && (header[156] === 48 || header[156] === 0)) {
      if (pkg || size > 1024 * 1024) throw new Error('tarball 中的 package.json 无效或重复');
      pkg = JSON.parse(decoder.decode(tar.subarray(offset, offset + size)));
    }
    offset += Math.ceil(size / 512) * 512;
  }
  if (!pkg) throw new Error('tarball 缺少 package/package.json');
  return pkg;
}

/** 为无专用源的内嵌依赖核验其精确坐标和实际字节。 */
export async function verifyVendored(host, id, release, copy, entries) {
  const bytes = entries?.[copy.path];
  if (!(bytes instanceof Uint8Array) || copy.version !== release || bytes.length !== copy.size ||
      !/^[a-f0-9]{64}$/.test(copy.sha256) || await host.sha256(bytes) !== copy.sha256) {
    throw new Error(id + '@' + release + ' 的内嵌依赖摘要或大小不符');
  }
  const pkg = tarballPackage(bytes);
  if (pkg.name !== id || pkg.version !== release) throw new Error(id + '@' + release + ' 的内嵌依赖身份不符');
  return bytes;
}

async function checkIntegrity(bytes, dist) {
  const candidates = String(dist.integrity || '').split(/\s+/);
  for (const bits of ['512', '384', '256']) {
    const prefix = 'sha' + bits + '-';
    const hashes = candidates.filter((entry) => entry.startsWith(prefix));
    if (hashes.length) {
      const digest = new Uint8Array(await globalThis.crypto.subtle.digest('SHA-' + bits, bytes));
      const actual = prefix + btoa(String.fromCharCode(...digest));
      if (!hashes.includes(actual)) {
        throw new Error('tarball integrity 不符');
      }
      return;
    }
  }
  throw new Error('registry 缺少可验证的 dist.integrity');
}

/** 在指定 registry 单独取得精确版本，验证元数据与压缩包身份，不参与其他依赖的解析。 */
export async function fetchPluginTarball(host, id, release, source, { authDir } = {}) {
  if (!exactNpm(id, release)) {
    throw new Error('依赖坐标必须是精确 npm 版本：' + id + '@' + release);
  }
  const base = registryUrl(source);
  const headersFor = (url) => url.startsWith(base) ? registryHeaders(host, url, authDir) : {};
  const temp = await host.mkdtemp('dspack-registry-');
  try {
    const packageUrl = base + id.replace('/', '%2F');
    const file = host.joinPath(temp, 'metadata.json');
    const downloadMetadata = async (url) => host.download(url, file, {
      headers: await headersFor(url), redirectHeaders: headersFor, maxBytes: 4 * 1024 * 1024,
    });
    let packageMetadata = false;
    try {
      try {
        await downloadMetadata(packageUrl + '/' + encodeURIComponent(release));
      } catch (error) {
        if (error.status !== 404) {
          throw error;
        }
        await downloadMetadata(packageUrl);
        packageMetadata = true;
      }
    } catch (error) {
      const status = Number.isInteger(error.status) ? '（HTTP ' + error.status + '）' : '';
      throw new Error('无法从声明的 registry 获取版本元数据' + status);
    }
    let meta;
    try {
      const data = JSON.parse(await host.readTextFile(file));
      meta = packageMetadata ? data?.versions?.[release] : data;
    } catch {
      throw new Error('registry 返回了无效的版本元数据');
    }
    if (packageMetadata && !meta) {
      throw new Error('声明的 registry 未提供所选版本');
    }
    if (meta?.name !== id || meta.version !== release) {
      throw new Error('registry 返回的包名或版本不符');
    }
    const tarUrl = new URL(meta.dist?.tarball);
    const localHttp = tarUrl.protocol === 'http:' && tarUrl.origin === new URL(base).origin;
    if ((!localHttp && tarUrl.protocol !== 'https:') || tarUrl.username || tarUrl.password || tarUrl.hash) {
      throw new Error('registry 返回了无效的 tarball 地址');
    }
    const archive = host.joinPath(temp, 'package.tgz');
    try {
      const headers = await headersFor(tarUrl.href);
      await host.download(tarUrl.href, archive, { headers, redirectHeaders: headersFor, maxBytes: maximumTarballBytes });
    } catch {
      throw new Error('无法下载 registry 指定的 tarball');
    }
    const bytes = await host.readFile(archive);
    if (!bytes?.length) {
      throw new Error('tarball 为空');
    }
    await checkIntegrity(bytes, meta.dist);
    const pkg = tarballPackage(bytes);
    if (pkg.name !== id || pkg.version !== release) {
      throw new Error('tarball 内的包名或版本不符');
    }
    return bytes;
  } catch (error) {
    throw new Error(id + '@' + release + ' [' + base + ']：' + error.message);
  } finally {
    await host.rm(temp, { recursive: true, force: true });
  }
}

/** 只恢复本工具内容寻址文件的精确版本，用户改写的依赖不会继承旧的选源记录。 */
export function installedPluginSource(pkg, id) {
  const record = pkg?.dsh?.packforge?.pluginSources?.[id];
  if (!record || !exactNpm(id, record.version) || !/^[a-f0-9]{64}$/.test(record.sha256)) {
    return null;
  }
  if (pkg.dependencies?.[id] !== 'file:.dspack-sources/' + record.sha256 + '.tgz') {
    return null;
  }
  return { version: record.version, registry: record.registry == null ? null : registryUrl(record.registry) };
}

/** 验证本机来源记录仍匹配安装时保存的不可变原件。 */
export async function verifyInstalledPluginSource(host, dir, pkg, id) {
  const saved = installedPluginSource(pkg, id);
  if (!saved) {
    return null;
  }
  const expected = pkg.dsh.packforge.pluginSources[id].sha256;
  const bytes = await host.readFile(host.joinPath(dir, '.dspack-sources', expected + '.tgz'));
  if (!bytes || await host.sha256(bytes) !== expected) {
    throw new Error(id + '@' + saved.version + ' 的已安装来源记录缺少匹配的 tarball');
  }
  const actual = tarballPackage(bytes);
  if (actual.name !== id || actual.version !== saved.version) {
    throw new Error(id + '@' + saved.version + ' 的已安装来源与 tarball 身份不符');
  }
  return saved;
}

async function unlockSources(host, dir, sources) {
  const path = host.joinPath(dir, 'pnpm-lock.yaml');
  const raw = await host.readTextFile(path);
  if (!raw) {
    return;
  }
  const lock = parseDocument(raw);
  if (lock.errors.length) {
    throw new Error('专用源安装无法校验 pnpm-lock.yaml');
  }
  const files = new Set(Object.values(sources).map((source) => 'file:.dspack-sources/' + source.sha256 + '.tgz'));
  let changed = false;
  for (const id of Object.keys(sources)) {
    for (const field of ['dependencies', 'optionalDependencies', 'devDependencies']) {
      for (const key of [[field, id], ['importers', '.', field, id]]) {
        if (lock.hasIn(key)) {
          lock.deleteIn(key);
          changed = true;
        }
      }
    }
  }
  for (const section of ['packages', 'snapshots']) {
    for (const item of lock.get(section)?.items || []) {
      const key = item.key.value;
      if (typeof key === 'string' && [...files].some((file) => key.includes(file))) {
        lock.deleteIn([section, key]);
        changed = true;
      }
    }
  }
  if (changed) {
    await host.writeTextFile(path, lock.toString());
  }
}

function assertSourceOverrides(id, release, pkg, overrides) {
  const name = pkg.name;
  for (const key of Object.keys(overrides || {})) {
    const split = key.lastIndexOf('>');
    if (split >= 0 && key.slice(0, split) !== name && !key.slice(0, split).startsWith(name + '@')) {
      continue;
    }
    if (split >= 0 && pkg.version && key.slice(0, split).startsWith(name + '@')) {
      const parentRange = key.slice(name.length + 1, split);
      if (validRange(parentRange) && !satisfies(pkg.version, parentRange)) continue;
    }
    const selector = key.slice(split + 1);
    if (selector === id || selector.startsWith(id + '@')) {
      const range = selector.slice(id.length + 1);
      if (!range || !validRange(range) || satisfies(release, range)) {
        throw new Error(id + '@' + release + ' 的专用源与 overrides[' + key + '] 冲突');
      }
    }
  }
}

async function workspaceOverrides(host, dir) {
  const raw = await host.readTextFile(host.joinPath(dir, 'pnpm-workspace.yaml'));
  const doc = raw ? parseDocument(raw) : null;
  if (doc?.errors.length) {
    throw new Error('专用源安装无法校验 pnpm-workspace.yaml');
  }
  return doc?.get('overrides')?.toJSON();
}

/** 比较合并后的直接依赖与暂存阶段已核验的来源，防止同版本改用旧锁或本地替换。 */
export async function verifyActivePluginSources(host, dir, unit, verifiedDir, { prepareLock = true } = {}) {
  const verified = JSON.parse(await host.readTextFile(host.joinPath(verifiedDir, 'package.json')));
  const declared = Object.entries(verified?.dsh?.packforge?.pluginSources ?? {});
  if (!declared.length) return;
  const pkg = JSON.parse(await host.readTextFile(host.joinPath(dir, 'package.json')));
  const overrides = await workspaceOverrides(host, dir);
  const pins = Object.create(null);
  for (const [id, expected] of declared) {
    const release = unit.dependencies[id];
    const actual = pkg.optionalDependencies?.[id] ?? pkg.dependencies?.[id] ?? pkg.devDependencies?.[id];
    if (actual === undefined || (exactNpm(id, actual) && actual !== release)) {
      continue;
    }
    const saved = await verifyInstalledPluginSource(host, dir, pkg, id);
    if (saved && saved.version !== release && actual === pkg.dependencies[id]) {
      continue;
    }
    const active = pkg.dsh?.packforge?.pluginSources?.[id];
    if (!saved || saved.version !== release || saved.registry !== (expected.registry ?? null) ||
        active.sha256 !== expected.sha256 || ['optionalDependencies', 'devDependencies'].some((field) =>
          Object.hasOwn(pkg[field] || {}, id) && pkg[field][id] !== actual)) {
      throw new Error(id + '@' + release + ' 合并后的依赖与已验证来源不同，请重新处理冲突');
    }
    assertSourceOverrides(id, release, pkg, pkg.pnpm?.overrides);
    assertSourceOverrides(id, release, pkg, overrides);
    pins[id] = active;
  }
  if (prepareLock && Object.keys(pins).length) {
    await unlockSources(host, dir, pins);
  }
}

/** 在每个 profile 的依赖安装前固定本次归档的原件，后续 pnpm 重装也会读取相同内容。 */
export async function applyPluginSources(host, dir, unit, { vendored, entries, authDir } = {}) {
  const declared = Object.entries(unit.dependencies || {}).filter(([id, release]) =>
    unit.plugins?.[id]?.source !== undefined || vendored?.[id]?.version === release);
  if (!declared.length) {
    return;
  }
  const pkgPath = host.joinPath(dir, 'package.json');
  const pkg = JSON.parse(await host.readTextFile(pkgPath));
  const overrides = await workspaceOverrides(host, dir);
  const sources = Object.create(null);
  for (const [id, release] of declared) {
    const source = unit.plugins?.[id]?.source;
    if (source && source.type !== 'npm' || !exactNpm(id, release)) {
      throw new Error('无效的专用源依赖：' + id);
    }
    assertSourceOverrides(id, release, pkg, pkg.pnpm?.overrides);
    assertSourceOverrides(id, release, pkg, overrides);
    const copy = vendored?.[id];
    const supplied = copy?.version === release ? await verifyVendored(host, id, release, copy, entries) : undefined;
    const bytes = source ? await fetchPluginTarball(host, id, release, source.registry, { authDir }) : supplied;
    const sha256 = await host.sha256(bytes);
    if (supplied && copy.sha256 !== sha256) {
      throw new Error(id + '@' + release + ' 的内嵌 tarball 与指定源不符');
    }
    const rel = '.dspack-sources/' + sha256 + '.tgz';
    await host.writeFile(host.joinPath(dir, ...rel.split('/')), bytes);
    pkg.dependencies[id] = 'file:' + rel;
    for (const field of ['optionalDependencies', 'devDependencies']) {
      if (pkg[field] && Object.hasOwn(pkg[field], id)) {
        pkg[field][id] = 'file:' + rel;
      }
    }
    sources[id] = { version: release, registry: source ? registryUrl(source.registry) : null, sha256 };
  }
  pkg.dsh = { ...pkg.dsh, packforge: { ...pkg.dsh?.packforge, pluginSources: sources } };
  await unlockSources(host, dir, sources);
  await host.writeTextFile(pkgPath, JSON.stringify(pkg, null, 2) + '\n');
}
