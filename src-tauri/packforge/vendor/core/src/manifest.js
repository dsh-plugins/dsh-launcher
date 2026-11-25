import { validatePluginMetadata } from './plugin-metadata.js';
import { installedPluginSource } from './registry.js';
// 新导出使用 manifest v6，已有 v4/v5 清单按各自版本解析。
// v5 = v4 全部硬约束（依赖坐标钉死精确版本/commit sha、dshVersion 精确、多语言元数据、files[]）
//      + type 区分形态："profile"（单包）/ "dshhome"（整机，含 profiles/presets/skills/instructions）。

const ICON_PATTERN = /^icons?\/.+\.(png|jpe?g|webp|ico|svg)$/i;

/**
 * 从 Profile 生成 manifest v6（type: "profile"）。
 * @param {Host} host 用于读取 package.json 与 cordis.patch.yml
 * @param {{name: string, dir: string}} profile
 * @param {object} opts { name, displayName, version, description, author, icon, dshVersion, profileName, files }
 * @param {{files: Array}} scan 扫描结果（用于图标探测）
 */
export async function buildManifest(host, profile, opts = {}, scan = { files: [] }) {
  const pkg = parseJson(await host.readTextFile(host.joinPath(profile.dir, 'package.json')));
  const name = sanitizeSlug(opts.name || profile.name);
  const version = opts.version || pkg?.version || '1.0.0';
  const displayName = opts.displayName || niceName(pkg?.name) || name;
  const description = opts.description ?? pkg?.description ?? '';
  const author = opts.author || (typeof pkg?.author === 'string' ? pkg.author : '') || '';
  // v5：dshVersion 必须是精确版本号（如 0.1.1-rc.2）。导出侧由扫描器注入；缺省置空可被导入端兜底。
  const dshVersion = opts.dshVersion || '';
  const icon = opts.icon || findIcon(scan.files) || '';
  const patch = (await host.readTextFile(host.joinPath(profile.dir, 'cordis.patch.yml'))) ?? '';

  return {
    manifestVersion: 6,
    type: 'profile',
    name,
    version,
    displayName,
    description,
    author,
    icon,
    dshVersion,
    ...(opts.dshVersions && { dshVersions: opts.dshVersions }),
    ...(opts.launchers && { launchers: opts.launchers }),
    profileName: opts.profileName || profile.name,
    bundles: extractBundles(pkg),
    dependencies: await coordinatesFromProfileDeps(host, profile.dir, pkg?.dependencies, pkg),
    patch,
    files: opts.files ?? [],
  };
}

/** 从单个 profile 目录生成 ProfileUnit（manifest v5 `profiles` 的值：复用 v4 单 profile 字段，去掉 profileName）。 */
async function buildProfileUnit(host, profileDir) {
  const pkg = parseJson(await host.readTextFile(host.joinPath(profileDir, 'package.json')));
  const patch = (await host.readTextFile(host.joinPath(profileDir, 'cordis.patch.yml'))) ?? '';
  return {
    bundles: extractBundles(pkg),
    dependencies: await coordinatesFromProfileDeps(host, profileDir, pkg?.dependencies, pkg),
    patch,
  };
}

/**
 * 从 home 根与选中的 profile 构建 manifest v6（type: "dshhome"）。
 * 四类单元：profiles（有依赖坐标）+ presets / skills / instructions（纯文件索引）。
 * @param {Host} host
 * @param {{name:string, dir:string}} home home 根目录
 * @param {object} opts { name, displayName, version, description, author, icon, dshVersion,
 *                        defaultProfile, profiles:[{name,dir}], presets, skills, instructions, files }
 */
export async function buildHomeManifest(host, home, opts = {}) {
  const profiles = Object.create(null);
  for (const p of opts.profiles ?? []) {
    if (!p?.name || !p?.dir) continue;
    profiles[p.name] = await buildProfileUnit(host, p.dir);
  }
  const names = Object.keys(profiles);
  return {
    manifestVersion: 6,
    type: 'dshhome',
    name: sanitizeSlug(opts.name || home.name),
    version: opts.version || '1.0.0',
    displayName: opts.displayName || home.name,
    description: opts.description ?? '',
    author: opts.author || '',
    icon: opts.icon || '',
    dshVersion: opts.dshVersion || '',
    ...(opts.dshVersions && { dshVersions: opts.dshVersions }),
    ...(opts.launchers && { launchers: opts.launchers }),
    defaultProfile: opts.defaultProfile || names[0] || '',
    profiles,
    presets: opts.presets ?? {},
    skills: opts.skills ?? [],
    instructions: opts.instructions || 'AGENTS.md',
    files: opts.files ?? [],
  };
}

/** 有序层栈：dsh.profile.bundles 原文顺序，去重，只留字符串。 */
export function extractBundles(pkg) {
  const bundles = pkg?.dsh?.profile?.bundles;
  if (!Array.isArray(bundles)) return [];
  const seen = new Set();
  const out = [];
  for (const b of bundles) {
    if (typeof b === 'string' && b.trim() && !seen.has(b)) {
      seen.add(b);
      out.push(b);
    }
  }
  return out;
}

/** 版本化依赖：package.json.dependencies 原文拷贝。 */
export function extractDependencies(pkg) {
  const deps = pkg?.dependencies;
  if (!deps || typeof deps !== 'object' || Array.isArray(deps)) return {};
  return { ...deps };
}

/** 规范化 slug：小写、非 [a-z0-9-] 转 -、合并连续 -。 */
export function sanitizeSlug(input) {
  return String(input)
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '');
}

/** 展示名美化：去掉 DSH Profile 包名前缀。 */
function niceName(raw) {
  if (!raw) return '';
  return String(raw).replace(/^dsh-profile-/, '').replace(/^dsh-/, '');
}

function findIcon(files) {
  for (const f of files) {
    const rel = (f.rel || '').replace(/\\/g, '/');
    if (ICON_PATTERN.test(rel)) return rel;
  }
  for (const f of files) {
    const rel = (f.rel || '').replace(/\\/g, '/');
    if (/^logo\.[a-z0-9]+$/i.test(rel)) return rel;
  }
  return '';
}

function parseJson(raw) {
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/* ---------------------------------------------------------------------------
 * 坐标转换（v3 §5，v4 原样继承）
 * manifest 形式「坐标 → 固定版本」 ⇄ package.json 形式「包名 → pnpm spec」
 * ------------------------------------------------------------------------- */

/**
 * 正向：坐标 → package.json 依赖（导入/安装侧使用）。
 *   "dsh-pet": "0.2.0"                              → "dsh-pet": "0.2.0"
 *   "github:owner/repo": "<sha>"                    → "repo": "github:owner/repo#<sha>"
 *   "github:owner/repo#path:/pkg": "<sha>"          → "pkg": "github:owner/repo#<sha>&path:pkg"
 */
export function coordsToPkgDeps(dependencies) {
  const out = {};
  for (const [coord, version] of Object.entries(dependencies ?? {})) {
    const git = parseGitCoord(coord);
    if (git) {
      const ref = version === 'latest' ? '' : `#${version}`;
      out[git.name] = `github:${git.owner}/${git.repo}${ref}` + (git.subpath ? `&path:${git.subpath}` : '');
    } else {
      out[coord] = version; // npm 精确版本，原样保留
    }
  }
  return out;
}

/** 解析 manifest 侧的 git 坐标：'github:owner/repo' 或 'github:owner/repo#path:/子目录'。 */
export function parseGitCoord(coord) {
  if (typeof coord !== 'string' || !coord.startsWith('github:')) return null;
  const rest = coord.slice('github:'.length); // 'owner/repo' 或 'owner/repo#path:/pkg'
  const hash = rest.indexOf('#path:/');
  const repoPart = hash >= 0 ? rest.slice(0, hash) : rest;
  const subpath = hash >= 0 ? rest.slice(hash + '#path:/'.length) : '';
  const slash = repoPart.indexOf('/');
  if (slash <= 0) return null;
  return { owner: repoPart.slice(0, slash), repo: repoPart.slice(slash + 1), subpath, name: subpath || repoPart.slice(slash + 1) };
}

/**
 * 反向：package.json 依赖 → 坐标（导出侧使用）。
 *   "dsh-pet": "0.2.0"                            → "dsh-pet": "0.2.0"
 *   "repo": "github:owner/repo#<sha>"             → "github:owner/repo": "<sha>"
 *   "pkg": "github:owner/repo#<sha>&path:pkg"     → "github:owner/repo#path:/pkg": "<sha>"
 */
export function pkgDepsToCoords(dependencies) {
  const out = {};
  for (const [pkgName, spec] of Object.entries(dependencies ?? {})) {
    const git = parsePkgGitSpec(spec);
    if (git) {
      out[`github:${git.owner}/${git.repo}${git.subpath ? `#path:/${git.subpath}` : ''}`] = git.sha;
    } else {
      out[pkgName] = spec; // 暂不订死范围（导出侧 M1 用 node_modules 实测版本钉精确）
    }
  }
  return out;
}

const EXACT_SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/;

/** 是否已是「精确版本」（无 ^ ~ > = 等前缀）。 */
export function isExactSemver(v) {
  return typeof v === 'string' && EXACT_SEMVER.test(v.trim());
}

/**
 * 生成 v5 dependencies（坐标→固定版本）：
 * - git 依赖：commit sha 优先取 package.json 的 `#sha`，缺则从 pnpm-lock.yaml 的 resolution.commit 补齐；
 *   都没有则标记为 `latest`（安装时跟随默认分支最新，不强制钉 sha）；
 * - npm 依赖若仍是范围（^/~ 等），读 node_modules/<name>/package.json 的实测版本钉精确。
 */
export async function coordinatesFromProfileDeps(host, dir, deps, profile = {}) {
  if (!deps || typeof deps !== 'object' || Array.isArray(deps)) return {};
  const lockText = await host.readTextFile(host.joinPath(dir, 'pnpm-lock.yaml'));
  const out = {};
  for (const [pkgName, spec] of Object.entries(deps)) {
    const source = installedPluginSource(profile, pkgName);
    if (source) {
      out[pkgName] = source.version;
      continue;
    }
    const git = parsePkgGitSpec(spec);
    if (git) {
      // 不强制钉 sha：spec 里的 #sha 优先，其次 pnpm-lock 的 resolution.commit；都没有则标记 latest（跟随默认分支最新）。
      const sha = git.sha || gitCommitFromLock(lockText, pkgName);
      out[`github:${git.owner}/${git.repo}${git.subpath ? `#path:/${git.subpath}` : ''}`] = sha || 'latest';
      continue;
    }
    // 只把「semver 范围」钉精确（^/~/>/ 等）。带 `:` 的是协议型 spec（file:/link:/workspace:/URL/别名），
    // 必须原样保留——否则导入侧会把它当成 npm registry 包去 404（例如 file: 本地依赖被压平成 0.2.0）。
    if (typeof spec === 'string' && spec && !isExactSemver(spec) && !spec.includes(':')) {
      const v = await readInstalledVersion(host, host.joinPath(dir, 'node_modules', pkgName, 'package.json'));
      out[pkgName] = v ?? spec;
    } else {
      out[pkgName] = spec;
    }
  }
  return out;
}

/**
 * 从 pnpm-lock.yaml 解析某 git 依赖的 commit sha（40 位十六进制）。
 * 扫描 `packages:` 区块内 `pkgName@…` 条目，取其 resolution.commit；找不到返回 null。
 */
export function gitCommitFromLock(text, pkgName) {
  if (!text || !pkgName) return null;
  const lines = String(text).split(/\r?\n/);
  let inPackages = false;
  let active = -1; // 目标条目的 key 缩进；-1 = 不在目标条目内
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    if (!inPackages) {
      if (/^packages:\s*$/.test(trimmed)) inPackages = true;
      continue;
    }
    const indent = line.length - line.trimStart().length;
    if (trimmed.endsWith(':') && indent === 0) {
      if (/^packages:\s*$/.test(trimmed)) { active = -1; continue; }
      break; // 顶层区块（snapshots: 等）→ packages 结束
    }
    const key = line.match(/^\s*([^:]+):\s*$/);
    if (key) {
      const k = key[1].replace(/^["']|["']$/g, '');
      if (k === pkgName || k.startsWith(`${pkgName}@`)) {
        active = indent;
      } else if (active >= 0 && indent <= active) {
        active = -1;
      }
      continue;
    }
    if (active >= 0) {
      const cm = line.match(/\bcommit:\s*['"]?([0-9a-f]{40})['"]?/);
      if (cm) return cm[1];
    }
  }
  return null;
}

async function readInstalledVersion(host, pkgPath) {
  const pkg = parseJson(await host.readTextFile(pkgPath));
  return typeof pkg?.version === 'string' ? pkg.version : null;
}

/** 解析 package.json 侧的 git spec：github:owner/repo#sha[&path:pkg] 或 git+https://github.com/...  */
export function parsePkgGitSpec(spec) {
  if (typeof spec !== 'string') return null;

  let body = spec;
  if (body.startsWith('github:')) {
    body = body.slice('github:'.length); // 'owner/repo#sha[&path:pkg]'
    let subpath = '';
    const amp = body.indexOf('&path:');
    if (amp >= 0) {
      subpath = body.slice(amp + '&path:'.length);
      body = body.slice(0, amp);
    }
    const hash = body.indexOf('#');
    const repoPart = hash >= 0 ? body.slice(0, hash) : body;
    const sha = hash >= 0 ? body.slice(hash + 1) : '';
    const slash = repoPart.indexOf('/');
    if (slash <= 0) return null;
    return { owner: repoPart.slice(0, slash), repo: repoPart.slice(slash + 1), subpath, sha };
  }

  const m = spec.match(
    /^(?:git\+)?https?:\/\/(?:www\.)?github\.com\/([^/#]+)\/([^/#]+?)(?:\.git)?(?:#([^&]+))?(?:&path:([^#]+))?$/,
  );
  if (m) {
    return { owner: m[1], repo: m[2], subpath: m[4] || '', sha: m[3] || '' };
  }
  return null;
}

/* ---------------------------------------------------------------------------
 * 校验
 * ------------------------------------------------------------------------- */

/**
 * manifest 结构校验（v5：profile 与 dshhome；v4：兼容单 profile），返回错误信息数组（空数组 = 合法）。
 * - v4：type 仅接受 'profile'（'collection' 预留报「暂未支持」）；
 * - v5：type 接受 'profile' 或 'dshhome'（dshhome 校验 profiles / presets / skills / instructions / defaultProfile）。
 */
export function validateManifest(m) {
  const errors = [];
  if (!m || typeof m !== 'object' || Array.isArray(m)) return ['manifest.json 缺失或不是对象'];

  if (![4, 5, 6].includes(m.manifestVersion)) {
    if (m.manifestVersion === 3 || m.manifestVersion === 2) {
      errors.push(`manifestVersion 为 ${m.manifestVersion}（旧版 .tgz 格式），本工具仅安装 v4/v5/v6(.dspack) 整合包`);
    } else {
      errors.push('manifestVersion 必须为 4、5 或 6');
    }
  }

  // 公共字段（v4/v5 一致）
  if (typeof m.name !== 'string' || !m.name.trim()) errors.push('manifest.name 缺失或为空');
  if (typeof m.version !== 'string' || !m.version.trim()) errors.push('manifest.version 缺失或为空');
  if (m.patch !== undefined && typeof m.patch !== 'string') errors.push('manifest.patch 必须是字符串');
  if (m.dshVersion !== undefined && typeof m.dshVersion !== 'string') errors.push('manifest.dshVersion 必须是字符串');
  for (const f of ['displayName', 'description']) {
    if (m[f] !== undefined && !isLocaleString(m[f])) errors.push(`manifest.${f} 必须是字符串或多语言对象`);
  }

  if (m.manifestVersion === 6) {
    for (const key of ['name','version']) {
      if (typeof m[key] === 'string' && (/[\\/:*?"<>|\x00-\x1f]|[. ]$/u.test(m[key]) || /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(m[key]))) {
        errors.push('manifest.' + key + ' 包含不能用于发布文件名的字符');
      }
    }
    if (m.type === 'dshhome') {
      if (m.plugins !== undefined) {
        errors.push('dshhome 的 plugins 必须位于对应的 profiles 条目内');
      }
      for (const [id, unit] of Object.entries(m.profiles || {})) {
        for (const error of validatePluginMetadata(unit)) {
          errors.push('profiles[' + id + '].' + error);
        }
      }
    } else {
      errors.push(...validatePluginMetadata(m));
    }
  }
  if (m.manifestVersion === 5 || m.manifestVersion === 6) {
    if (m.type === 'dshhome') errors.push(...validateDshHome(m));
    else if ((m.manifestVersion === 5 && m.type === undefined) || m.type === 'profile') errors.push(...validateProfile(m));
    else errors.push('type 仅支持 "profile" 或 "dshhome"');
  } else {
    errors.push(...validateProfile(m));
  }
  return errors;
}

/** 单 profile 校验（v4/v5 共用）。 */
function validateProfile(m) {
  const errors = [];
  if (m.type !== undefined && m.type !== 'profile') {
    errors.push('type 仅支持 "profile"（collection 为预留值，暂未支持）');
  }
  if (!Array.isArray(m.bundles) || m.bundles.some((b) => typeof b !== 'string')) {
    errors.push('manifest.bundles 必须是字符串数组');
  }
  if (typeof m.dependencies !== 'object' || m.dependencies === null || Array.isArray(m.dependencies)) {
    errors.push('manifest.dependencies 必须是对象');
  } else {
    for (const [k, v] of Object.entries(m.dependencies)) {
      if (typeof v !== 'string' || !v) errors.push(`dependencies[${k}] 必须是「坐标 → 固定版本」字符串`);
    }
  }
  if (m.files !== undefined) errors.push(...validateFiles(m.files));
  return errors;
}

/** v5 dshhome 校验。 */
function validateDshHome(m) {
  const errors = [];
  if (m.type !== undefined && m.type !== 'dshhome') {
    errors.push('type 仅支持 "dshhome"（单 profile 请用 type:"profile"）');
  }

  if (typeof m.profiles !== 'object' || m.profiles === null || Array.isArray(m.profiles)) {
    errors.push('manifest.profiles 必须是对象（name → ProfileUnit）');
  } else {
    const names = Object.keys(m.profiles);
    if (names.length === 0) errors.push('manifest.profiles 至少含 1 个 profile');
    for (const reserved of ['web', 'headless']) {
      if (names.includes(reserved)) errors.push(`profiles 不得含安装基线模板「${reserved}」`);
    }
    for (const [name, u] of Object.entries(m.profiles)) {
      if (!u || typeof u !== 'object' || Array.isArray(u)) {
        errors.push(`profiles[${name}] 必须是对象`);
        continue;
      }
      if (!Array.isArray(u.bundles) || u.bundles.some((b) => typeof b !== 'string')) {
        errors.push(`profiles[${name}].bundles 必须是字符串数组`);
      }
      if (typeof u.dependencies !== 'object' || u.dependencies === null || Array.isArray(u.dependencies)) {
        errors.push(`profiles[${name}].dependencies 必须是对象`);
      } else {
        for (const [k, v] of Object.entries(u.dependencies)) {
          if (typeof v !== 'string' || !v) errors.push(`profiles[${name}].dependencies[${k}] 必须是「坐标 → 固定版本」字符串`);
        }
      }
      if (u.patch !== undefined && typeof u.patch !== 'string') errors.push(`profiles[${name}].patch 必须是字符串`);
    }
    if (typeof m.defaultProfile !== 'string' || !m.defaultProfile) {
      errors.push('manifest.defaultProfile 缺失或为空');
    } else if (!names.includes(m.defaultProfile)) {
      errors.push(`defaultProfile「${m.defaultProfile}」不在 profiles 中`);
    }
  }

  if (m.presets !== undefined) {
    if (typeof m.presets !== 'object' || m.presets === null || Array.isArray(m.presets)) {
      errors.push('manifest.presets 必须是对象（name → PresetUnit）');
    } else {
      for (const [name, u] of Object.entries(m.presets)) {
        if (!u || typeof u !== 'object' || Array.isArray(u) || typeof u.path !== 'string' || !u.path) {
          errors.push(`presets[${name}] 必须是含 path 的对象`);
        }
      }
    }
  }

  if (m.skills !== undefined) {
    if (!Array.isArray(m.skills)) {
      errors.push('manifest.skills 必须是数组');
    } else {
      m.skills.forEach((s, i) => {
        if (!s || typeof s !== 'object' || Array.isArray(s) || typeof s.path !== 'string' || !s.path) {
          errors.push(`skills[${i}] 必须是含 path 的对象`);
        }
      });
    }
  }

  if (m.instructions !== undefined && (typeof m.instructions !== 'string' || !m.instructions)) {
    errors.push('manifest.instructions 必须是非空字符串');
  }

  if (m.files !== undefined) errors.push(...validateFiles(m.files));
  return errors;
}

/** files[] 数组校验（v4/v5 共用）。 */
function validateFiles(files) {
  const errors = [];
  if (!Array.isArray(files)) {
    errors.push('manifest.files 必须是数组');
  } else {
    files.forEach((f, i) => {
      for (const e of validateFileEntry(f)) errors.push(`files[${i}] ${e}`);
    });
  }
  return errors;
}

/** files[] 单条校验（v4 §3：path/sha256/size/urls[]）。 */
export function validateFileEntry(f) {
  const errors = [];
  if (!f || typeof f !== 'object' || Array.isArray(f)) return ['不是对象'];
  if (typeof f.path !== 'string' || !f.path || f.path.startsWith('/') || /^[a-zA-Z]:/.test(f.path)) {
    errors.push('path 必须是相对路径（"+"分隔，不以盘符/斜杠开头）');
  }
  if (typeof f.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(f.sha256)) {
    errors.push('sha256 必须是 64 位十六进制');
  }
  if (typeof f.size !== 'number' || !Number.isInteger(f.size) || f.size <= 0) {
    errors.push('size 必须是正整数');
  }
  if (!Array.isArray(f.urls) || f.urls.length === 0 || f.urls.some((u) => typeof u !== 'string' || !/^https?:\/\//i.test(u))) {
    errors.push('urls 必须是非空数组，且每项是 http(s) 地址');
  }
  return errors;
}

/** displayName/description 的 string|map 判定。 */
export function isLocaleString(v) {
  if (typeof v === 'string') return true;
  if (v && typeof v === 'object' && !Array.isArray(v)) {
    const keys = Object.keys(v);
    return keys.length > 0 && keys.every((k) => typeof k === 'string' && k !== '' && typeof v[k] === 'string');
  }
  return false;
}

/** 按界面语言解析多语言元数据：字符串原样，map 按 locale → en-US → zh-CN → 首项回退。 */
export function resolveLocale(value, locale) {
  if (typeof value === 'string') return value;
  if (value && typeof value === 'object') {
    if (locale && typeof value[locale] === 'string') return value[locale];
    if (typeof value['en-US'] === 'string') return value['en-US'];
    if (typeof value['zh-CN'] === 'string') return value['zh-CN'];
    for (const k of Object.keys(value)) {
      if (typeof value[k] === 'string') return value[k];
    }
  }
  return '';
}
