// Profile 发现：经典路径 + DSH 启动器实例（双源），以及精确 dshVersion 枚举。
// 主机无关：平台差异（homedir / %APPDATA% / XDG）通过 Host.env/homedir/joinPath 抽象。

const SKIP_DIRS = new Set(['node_modules', '__temp__']);

/** 判断 profile 目录名是否跳过（点目录 / 脚手架 / 依赖）。 */
export function skipProfileDir(name) {
  return typeof name !== 'string' || name.startsWith('.') || SKIP_DIRS.has(name);
}

/** 启动器 config.json 候选位置（Windows / Linux / macOS）。 */
export function launcherConfigCandidates(host) {
  const c = [];
  const appData = host.env('APPDATA');
  if (appData) c.push(host.joinPath(appData, 'in.dsh-plug.dsh-launcher', 'config.json'));
  const xdg = host.env('XDG_CONFIG_HOME') || host.joinPath(host.homedir(), '.config');
  c.push(host.joinPath(xdg, 'in.dsh-plug.dsh-launcher', 'config.json'));
  c.push(host.joinPath(xdg, 'dsh-launcher', 'config.json'));
  return c;
}

async function readJson(host, p) {
  const raw = await host.readTextFile(p);
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

async function findLauncherConfig(host, override) {
  if (override !== undefined) return override || null; // 空串/null 显式禁用探测
  for (const p of launcherConfigCandidates(host)) {
    if ((await host.stat(p)) != null) return p;
  }
  return null;
}

async function readLauncherConfig(host, override) {
  const p = await findLauncherConfig(host, override);
  if (!p) return null;
  return await readJson(host, p);
}

/**
 * 发现全部 profile（经典 + 启动器，按目录去重）。
 * @param {Host} host
 * @param {object} opts { home?, launcherConfig? } 测试可注入；launcherConfig 为 '' 表示禁用。
 * @returns {{profiles: Array<{name,dir,source,home}>, roots: Array<{source,root,home}>}}
 *   source ∈ 'classic' | 'launcher'；home 为启动器实例名（classic 为 null）。
 */
export async function discoverProfiles(host, opts = {}) {
  const home = opts.home || host.homedir();
  const roots = [];
  const profiles = [];
  const seen = new Set(); // 去重（绝对目录）

  const collect = async (rootDir, source, homeName) => {
    roots.push({ source, root: rootDir, home: homeName ?? null });
    const entries = await host.readdir(rootDir);
    if (!Array.isArray(entries)) return;
    const dirs = entries.filter((e) => e.type === 'dir' && !skipProfileDir(e.name));
    dirs.sort((a, b) => a.name.localeCompare(b.name));
    for (const e of dirs) {
      if (seen.has(e.abs)) continue;
      seen.add(e.abs);
      profiles.push({ name: e.name, dir: e.abs, source, home: homeName ?? null });
    }
  };

  // 1) 经典 ~/.dsh/profiles
  await collect(host.joinPath(home, '.dsh', 'profiles'), 'classic', null);

  // 2) 启动器 homes[]（config.json 是权威注册表）
  const cfg = await readLauncherConfig(host, opts.launcherConfig);
  for (const h of cfg?.homes ?? []) {
    if (!h?.path) continue;
    await collect(host.joinPath(h.path, 'profiles'), 'launcher', h.name || h.id || h.path);
  }

  return { profiles, roots };
}

/**
 * 发现全部 DSH_HOME 实例（经典默认 home + 启动器 homes[]）。
 * dshhome 导出 / 安装以「整个 home」为单元，故需要 home 而非 profile 粒度。
 * @param {Host} host
 * @param {object} opts { home?, launcherConfig? }
 * @returns {Array<{name, dir, source}>} source ∈ 'classic' | 'launcher'
 */
export async function discoverHomes(host, opts = {}) {
  const defaultHome = opts.home || host.homedir();
  const homes = [];
  const seen = new Set();

  const add = (dir, name, source) => {
    const abs = host.resolvePath(dir);
    if (seen.has(abs)) return;
    seen.add(abs);
    homes.push({ name, dir: abs, source });
  };

  // 1) 经典 ~/.dsh（默认单实例）
  add(host.joinPath(defaultHome, '.dsh'), 'default', 'classic');

  // 2) 启动器 homes[]（config.json 是权威注册表）
  const cfg = await readLauncherConfig(host, opts.launcherConfig);
  for (const h of cfg?.homes ?? []) {
    if (!h?.path) continue;
    add(h.path, h.name || h.id || h.path, 'launcher');
  }

  return homes;
}


/** 仅在目标 HOME 或整合包版本能确定本机 DSH 安装时，返回解析内置 bundle 的 package.json。 */
export async function findLauncherInstallAnchor(host, home, version) {
  const config = await readLauncherConfig(host);
  if (!Array.isArray(config?.versions)) return undefined;
  const key = path => {
    const abs = host.resolvePath(path);
    return abs.includes('\\') ? abs.toLowerCase() : abs;
  };
  const target = key(home);
  const homes = new Set((Array.isArray(config.homes) ? config.homes : [])
    .filter(row => typeof row?.path === 'string' && row.path && key(row.path) === target)
    .map(row => row.id));
  const active = new Set((Array.isArray(config.instances) ? config.instances : [])
    .filter(row => row && homes.has(row.home_id)).map(row => row.version_id));
  if (!active.size && !version) return undefined;
  const matches = config.versions.filter(row => row && !row.wsl && typeof row.dir === 'string' && row.dir &&
    (!active.size || active.has(row.id)) && (!version || row.version === version));
  const dirs = [...new Set(matches.map(row => host.resolvePath(row.dir)))];
  if (dirs.length !== 1) return undefined;
  for (const location of ['apps/cli/package.json', 'node_modules/@deepseek-ai/dsh/package.json']) {
    const anchor = host.joinPath(dirs[0], location);
    const pkg = await readJson(host, anchor);
    if (pkg?.name === '@deepseek-ai/dsh' && (!version || pkg.version === version)) return anchor;
  }
  return undefined;
}

/** 枚举启动器已安装的 DSH 版本（精确版本号），降序。 */
export async function listInstalledDshVersions(host, opts = {}) {
  const cfg = await readLauncherConfig(host, opts.launcherConfig);
  const versions = (cfg?.versions ?? []).map((v) => v?.version).filter((v) => typeof v === 'string' && v);
  return sortVersionsDesc(versions);
}

/**
 * 解析 profile 输入：路径优先（手动选择目录），其次按名字匹配已发现 profile。
 * @returns {{name,dir,source,home}|null}
 */
export async function resolveProfileInput(host, input, opts = {}) {
  if (!input) return null;
  const abs = host.resolvePath(input);
  const st = await host.stat(abs);
  if (st?.isDirectory) {
    return { name: host.basename(abs), dir: abs, source: 'custom', home: null };
  }
  const { profiles } = await discoverProfiles(host, opts);
  const hits = profiles.filter((p) => p.name === input);
  return hits.length ? hits[0] : null;
}

/* ------------------- 版本比较（用于 dshVersion 精确钉定） ------------------- */

function parseVersion(v) {
  const m = String(v).match(/^(\d+)\.(\d+)\.(\d+)(?:-([A-Za-z]+)\.(\d+))?/);
  if (!m) return { num: [0, 0, 0], pre: null };
  return { num: [+m[1], +m[2], +m[3]], pre: m[4] ? +m[5] : null };
}

/** 比较 a、b：a>b 返回正，相等返回 0。正式版 > 预发布。 */
export function compareVersions(a, b) {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  for (let i = 0; i < 3; i++) {
    if (pa.num[i] !== pb.num[i]) return pa.num[i] - pb.num[i];
  }
  if (pa.pre === null && pb.pre === null) return 0;
  if (pa.pre === null) return 1;
  if (pb.pre === null) return -1;
  return pa.pre - pb.pre;
}

/** 降序排序版本号。 */
export function sortVersionsDesc(versions) {
  return [...versions].sort((a, b) => compareVersions(b, a));
}