import { isLocaleString, parsePkgGitSpec } from './manifest.js';
import { exactNpm, installedPluginSource, verifyInstalledPluginSource, registryUrl } from './registry.js';
import { parseDocument } from 'yaml';

const object = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
const assetPrefix = 'assets/plugins/';
const maxIconBytes = 256 * 1024;
const isPackageId = (id) => /^(?:@[a-z0-9][\w.-]*\/)?[a-z0-9][\w.-]*$/i.test(id);
const safeRelative = (s) => typeof s === 'string' && s.length > 0 && s.split('/').every(
  (p) => p !== '' && p !== '.' && p !== '..' && !/[\x00-\x1f\\:#?%<>|*"]|[. ]$/.test(p) &&
    !/^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(p),
);

function remoteIcon(raw) {
  try {
    const url = new URL(raw);
    return url.protocol === 'https:' && !url.username && !url.password;
  } catch {
    return false;
  }
}

/** 图标属于归档展示资源；安装器不得将其写入 profile 的 overrides。 */
export function isPluginAsset(raw) {
  return safeRelative(raw) && raw.startsWith(assetPrefix) && /\.(svg|png|jpe?g|webp)$/i.test(raw);
}

/** 检查单个 profile 的插件描述及其精确选源，返回含插件 ID 的字段错误。 */
export function validatePluginMetadata(unit) {
  if (unit?.plugins === undefined) {
    return [];
  }
  if (!object(unit.plugins)) {
    return ['plugins 必须是对象'];
  }
  const errors = [];
  for (const [id, item] of Object.entries(unit.plugins)) {
    const label = 'plugins[' + id + ']';
    if (!id || (!unit.bundles?.includes?.(id) && !Object.hasOwn(unit.dependencies || {}, id))) {
      errors.push(label + ' 必须对应本 profile 的 bundle 或依赖');
    }
    if (!object(item)) {
      errors.push(label + ' 必须是对象');
      continue;
    }
    for (const field of ['name', 'desc']) {
      if (item[field] !== undefined && !isLocaleString(item[field])) {
        errors.push(label + '.' + field + ' 必须是字符串或多语言对象');
      }
    }
    if (item.icon !== undefined && (typeof item.icon !== 'string' || !(isPluginAsset(item.icon) || remoteIcon(item.icon)))) {
      errors.push(label + '.icon 必须为 HTTPS 地址或 assets/plugins/ 下的图标路径');
    }
    if (item.source !== undefined) {
      try {
        if (!object(item.source) || item.source.type !== 'npm' || !exactNpm(id, unit.dependencies?.[id])) {
          throw new Error('必须关联当前 profile 的精确 npm 版本');
        }
        if (Object.keys(item.source).some((key) => !['type', 'registry'].includes(key))) {
          throw new Error('只允许 type 和 registry');
        }
        registryUrl(item.source.registry);
      } catch (error) {
        errors.push(label + '.source ' + error.message);
      }
    }
  }
  return errors;
}

/** 对归档资源交叉引用做大小和路径校验，两种包形态共用此规则。 */
export function validatePluginAssets(manifest, entries) {
  const units = manifest.type === 'dshhome' ? Object.values(manifest.profiles || {}) : [manifest];
  for (const unit of units) {
    for (const [id, item] of Object.entries(unit?.plugins || {})) {
      const icon = item?.icon;
      if (typeof icon === 'string' && !remoteIcon(icon)) {
        if (!isPluginAsset(icon) || !Object.hasOwn(entries, icon) || !entries[icon]?.length || entries[icon].length > maxIconBytes) {
          throw new Error(id + ' 的归档图标缺失、路径无效或超过 256 KiB');
        }
      }
    }
  }
  for (const id of Object.keys(manifest.vendored || {})) {
    const users = units.filter((unit) => Object.hasOwn(unit?.dependencies || {}, id));
    if (!users.some((unit) => unit.plugins?.[id]?.source)) {
      continue;
    }
    const pins = new Set(users.map((unit) => {
      const source = unit.plugins?.[id]?.source;
      return unit.dependencies[id] + ' ' + (source ? registryUrl(source.registry) : '');
    }));
    if (pins.size > 1) {
      throw new Error(id + ' 的共享内嵌副本与各 profile 的来源或版本不一致');
    }
  }
}

function plainPath(value) {
  const rel = typeof value === 'string' ? value.replace(/^\.\//, '') : '';
  return safeRelative(rel) ? rel : null;
}

/** npm 包本身可为 pnpm 的链接。进入包后只读取真实目录和文件。 */
async function packagePath(host, root, rel, finalType = 'file') {
  if (!plainPath(rel)) {
    return null;
  }
  const parts = rel.split('/');
  let current = root;
  for (const [i, part] of parts.entries()) {
    const list = await host.readdir(current);
    const entry = list?.find((e) => e.name === part);
    if (entry?.type !== (i === parts.length - 1 ? finalType : 'dir')) {
      return null;
    }
    current = host.joinPath(current, part);
  }
  return current;
}

async function collectLocales(host, pkgDir, pkg) {
  const name = Object.create(null);
  const desc = Object.create(null);
  const files = new Map();
  const exports = pkg.exports ?? { './locale/*.json': './locale/*.json' };
  for (const [key, value] of Object.entries(exports)) {
    const match = key.match(/^\.\/locale\/([\w*-]+)\.json$/);
    if (!match) {
      continue;
    }
    const template = typeof value === 'string' ? value : value?.default;
    const target = typeof template === 'string' && plainPath(template.replace('*', 'lang')) ? template.replace(/^\.\//, '') : null;
    if (!target) {
      continue;
    }
    if (match[1] !== '*') {
      files.set(match[1], target);
      continue;
    }
    const slash = target.lastIndexOf('/');
    const dirname = slash < 0 ? '' : target.slice(0, slash);
    const filename = target.slice(slash + 1);
    if (dirname.includes('*') || !filename.includes('*') || filename.indexOf('*') !== filename.lastIndexOf('*')) {
      continue;
    }
    const [prefix, suffix] = filename.split('*');
    const dir = dirname ? await packagePath(host, pkgDir, dirname, 'dir') : pkgDir;
    if (!dir) {
      continue;
    }
    for (const entry of (await host.readdir(dir)) || []) {
      if (entry.type === 'file' && entry.name.startsWith(prefix) && entry.name.endsWith(suffix)) {
        const lang = entry.name.slice(prefix.length, suffix ? -suffix.length : undefined);
        if (/^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$/.test(lang)) {
          files.set(lang, (dirname ? dirname + '/' : '') + entry.name);
        }
      }
    }
  }
  for (const [lang, rel] of files) {
    const path = await packagePath(host, pkgDir, rel);
    if (!path) {
      continue;
    }
    const raw = await host.readTextFile(path);
    const meta = raw ? JSON.parse(raw)?.meta : null;
    if (typeof meta?.title === 'string' && meta.title.trim()) {
      name[lang] = meta.title;
    }
    if (typeof meta?.description === 'string' && meta.description.trim()) {
      desc[lang] = meta.description;
    }
  }
  return {
    name: Object.keys(name).length ? name : pkg.name,
    desc: Object.keys(desc).length ? desc : typeof pkg.description === 'string' ? pkg.description : undefined,
  };
}

async function readYaml(host, ...parts) {
  const raw = await host.readTextFile(host.joinPath(...parts));
  const doc = raw ? parseDocument(raw) : null;
  return doc?.errors.length ? null : doc;
}

function workspaceRoutes(workspace) {
  const routes = Object.create(null);
  const invalid = { default: '' };
  let map;
  try {
    map = workspace?.get('registries', true)?.toJS(workspace);
  } catch {
    return invalid;
  }
  if (map == null) return routes;
  if (!object(map)) return invalid;
  const entries = Object.entries(map);
  if (entries.every(([key, value]) => (key === 'default' || key.startsWith('@')) && typeof value === 'string')) {
    return Object.assign(routes, map);
  }
  for (const [url, entry] of entries) {
    if (!/^https?:\/\//i.test(url) || !object(entry)) return invalid;
    if (entry.ecosystem && entry.ecosystem !== 'npm') continue;
    if (entry.scopes === undefined) continue;
    if (!Array.isArray(entry.scopes)) return invalid;
    for (const scope of entry.scopes) {
      if (typeof scope !== 'string' || !scope.startsWith('@')) return invalid;
      const key = scope === '@' ? 'default' : scope;
      if (Object.hasOwn(routes, key)) return invalid;
      routes[key] = url;
    }
  }
  return routes;
}

const packageScope = id => id.startsWith('@') ? id.split('/')[0] : 'default';

function workspaceRegistry(routes, id) {
  const raw = routes[packageScope(id)] ?? routes.default;
  if (!raw || !/^https?:\/\//i.test(raw) || raw.includes('${')) return undefined;
  try {
    return registryUrl(raw);
  } catch {
    return undefined;
  }
}

async function registryContext(host, dir, routes, registry) {
  const [lock, modules] = await Promise.all([
    readYaml(host, dir, 'pnpm-lock.yaml'), readYaml(host, dir, 'node_modules', '.modules.yaml'),
  ]);
  const settings = Object.create(null);
  const user = host.env?.('NPM_CONFIG_USERCONFIG') || host.env?.('npm_config_userconfig') || host.joinPath(host.homedir(), '.npmrc');
  for (const path of [user, host.joinPath(dir, '.npmrc')]) {
    const raw = await host.readTextFile(path);
    for (const line of (raw || '').split(/\r?\n/)) {
      const match = line.trim().match(/^(?:(@[a-z0-9._-]+):)?registry\s*=\s*(\S+)$/i);
      if (match) {
        settings[match[1] || 'default'] = match[2];
      }
    }
  }
  const envRegistry = host.env?.('npm_config_registry') || host.env?.('NPM_CONFIG_REGISTRY');
  if (envRegistry) {
    settings.default = envRegistry;
  }
  return { lock, modules, routes, registry, settings };
}

const publicNpm = 'https://registry.npmjs.org/';
const isPublic = (url) => typeof url === 'string' && (url === publicNpm || url + '/' === publicNpm);

function inferredPublic(scope, ctx) {
  const installed = ctx.modules?.getIn(['registries', scope]) ?? ctx.modules?.getIn(['registries', 'default']);
  const workspaceRoute = ctx.routes[scope] ?? ctx.routes.default ?? ctx.registry;
  const settings = ctx.settings[scope] ?? ctx.settings.default;
  if ((installed != null && !isPublic(installed)) || (workspaceRoute != null && !isPublic(workspaceRoute)) ||
      (settings != null && !isPublic(settings))) {
    return false;
  }
  return isPublic(installed) || isPublic(workspaceRoute) || isPublic(settings);
}

async function publicArtifact(host, pkg, id, release, source, loadContext) {
  if (pkg.private === true || pkg.name !== id || pkg.version !== release || !exactNpm(id, release)) {
    return false;
  }
  if (source && registryUrl(source.registry) !== publicNpm) {
    return false;
  }
  const ctx = await loadContext();
  const short = id.split('/').pop();
  const publicUrl = 'https://registry.npmjs.org/' + id + '/-/' + short + '-' + release + '.tgz';
  const key = ['packages', id + '@' + release, 'resolution'];
  const resolved = ctx.lock?.getIn([...key, 'tarball']);
  const integrity = ctx.lock?.getIn([...key, 'integrity']);
  const recorded = resolved || pkg._resolved;
  const scope = packageScope(id);
  if (!source) {
    if (recorded && recorded !== publicUrl) {
      return false;
    }
    if (!recorded && (!integrity || !inferredPublic(scope, ctx))) {
      return false;
    }
  }
  const tmp = await host.mkdtemp('dspack-public-icon-');
  try {
    const file = host.joinPath(tmp, 'metadata.json');
    await host.download('https://registry.npmjs.org/' + id.replace('/', '%2F') + '/' + encodeURIComponent(release), file);
    const meta = JSON.parse(await host.readTextFile(file));
    return meta?.name === id && meta.version === release && meta.dist?.tarball === publicUrl &&
      (!integrity || meta.dist.integrity === integrity);
  } catch {
    // 离线导出或无法确认公开来源时，图标随归档分发。
    return false;
  } finally {
    await host.rm(tmp, { recursive: true, force: true });
  }
}

async function iconFor(host, pkgDir, pkg, id, release, source, context, entries) {
  const value = pkg.exports?.['./icon'];
  const rel = plainPath((typeof value === 'string' ? value : value?.default) || pkg.icon);
  const ext = rel?.match(/\.(svg|png|jpe?g|webp)$/i)?.[1];
  if (!ext) {
    return undefined;
  }
  const path = await packagePath(host, pkgDir, rel);
  if (!path) {
    return undefined;
  }
  const bytes = await host.readFile(path);
  if (!bytes?.length || bytes.length > maxIconBytes) {
    return undefined;
  }
  if (await publicArtifact(host, pkg, id, release, source, context)) {
    return 'https://cdn.jsdelivr.net/npm/' + id + '@' + release + '/' + rel.split('/').map(encodeURIComponent).join('/');
  }
  const asset = assetPrefix + await host.sha256(bytes) + '.' + ext.toLowerCase();
  entries[asset] = bytes;
  return asset;
}

async function readProfilePackage(host, dir) {
  const raw = await host.readTextFile(host.joinPath(dir, 'package.json'));
  return raw ? JSON.parse(raw) : {};
}

/** 内嵌副本由整个归档共享，各配置档仅为其余依赖推断专用源。 */
export async function collectHomePluginMetadata(host, profiles, units, profilePlugins, entries) {
  const packages = new Map();
  const vendored = new Set();
  for (const { name, dir } of profiles) {
    const pkg = await readProfilePackage(host, dir);
    packages.set(name, pkg);
    for (const id of Object.keys(pkg.dsh?.packforge?.pluginSources || {})) {
      const saved = installedPluginSource(pkg, id);
      if (saved && saved.registry === null && saved.version === units[name].dependencies?.[id]) vendored.add(id);
    }
  }
  for (const { name, dir } of profiles) {
    await collectPluginMetadata(host, dir, units[name], {
      plugins: profilePlugins?.[name], entries, profile: packages.get(name), vendored,
    });
  }
}

/** 导出已安装插件的静态展示资料，作者指定的字段优先。 */
export async function collectPluginMetadata(host, profileDir, unit, { plugins, entries, profile, vendored }) {
  if (plugins !== undefined && !object(plugins)) {
    throw new Error('plugins 必须是对象');
  }
  profile ??= await readProfilePackage(host, profileDir);
  const workspacePath = await packagePath(host, profileDir, 'pnpm-workspace.yaml');
  const workspace = workspacePath ? await readYaml(host, workspacePath) : null;
  const routes = workspaceRoutes(workspace);
  let context;
  const loadContext = () => context ??= registryContext(host, profileDir, routes, workspace?.get('registry'));
  const ids = new Set([...(unit.bundles || []), ...Object.keys(plugins || {}), ...Object.keys(profile.dependencies || {})]);
  const result = Object.create(null);
  for (const id of ids) {
    const git = parsePkgGitSpec(profile.dependencies?.[id]);
    const coord = git ? 'github:' + git.owner + '/' + git.repo + (git.subpath ? '#path:/' + git.subpath : '') : id;
    const key = unit.bundles?.includes(id) || Object.hasOwn(unit.dependencies || {}, id) ? id : coord;
    const override = Object.hasOwn(plugins || {}, key) ? plugins[key] : undefined;
    const dir = isPackageId(id) ? host.joinPath(profileDir, 'node_modules', ...id.split('/')) : null;
    const installed = dir ? await packagePath(host, dir, 'package.json') : null;
    const text = installed ? await host.readTextFile(installed) : null;
    const pkg = text ? JSON.parse(text) : null;
    const saved = await verifyInstalledPluginSource(host, profileDir, profile, id);
    const release = unit.dependencies?.[key];
    const inferred = !saved && !vendored?.has(key) && !Object.hasOwn(override || {}, 'source') && exactNpm(key, release)
      ? workspaceRegistry(routes, key) : undefined;
    if (!override && !unit.bundles?.includes(id) && !pkg?.dsh?.bundle && !pkg?.dsh?.client && !saved && !inferred) {
      continue;
    }
    if (override !== undefined && !object(override)) {
      throw new Error('plugins[' + key + '] 必须是对象');
    }
    const current = typeof release === 'string' && override?.version === release;
    const registry = saved && saved.version === release ? saved.registry : inferred;
    let source = registry ? { type: 'npm', registry } : undefined;
    if (override && Object.hasOwn(override, 'source') && !current) {
      throw new Error('插件的精确版本已变化，请重新选择包源：' + key);
    }
    if (current && Object.hasOwn(override, 'source')) {
      source = override.source?.type === 'npm' && override.source.registry === undefined ? undefined : override.source;
    }
    const entry = pkg ? await collectLocales(host, dir, pkg) : { name: id };
    if (source !== undefined && source !== null) {
      entry.source = source;
    }
    for (const field of ['name', 'desc']) {
      if (override && Object.hasOwn(override, field)) {
        entry[field] = override[field];
      }
    }
    if (override?.icon) {
      entry.icon = override.icon;
      if (isPluginAsset(entry.icon) && !Object.hasOwn(entries, entry.icon)) {
        const path = await packagePath(host, profileDir, entry.icon);
        if (path) {
          entries[entry.icon] = await host.readFile(path);
        }
      }
    } else if (pkg) {
      // 配置档当前选择公共源，仍须核实本地安装包的实际来源。
      const iconSource = isPublic(inferred) ? undefined : entry.source;
      const icon = await iconFor(host, dir, pkg, key, release, iconSource, loadContext, entries);
      if (icon) {
        entry.icon = icon;
      }
    }
    if (entry.desc === undefined) {
      delete entry.desc;
    }
    result[key] = entry;
  }
  if (Object.keys(result).length) {
    unit.plugins = result;
  }
  const errors = validatePluginMetadata(unit);
  if (errors.length) {
    throw new Error(errors.join('；'));
  }
  validatePluginAssets(unit, entries);
  return result;
}
