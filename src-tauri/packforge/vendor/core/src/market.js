/**
 * 市场索引读取（宿主无关：Electron GUI 与 DSH 客户端插件共用）。
 * - 兼容精简索引（schemaVersion 2，`modpacks[]`）：条目只含指针 + 展示元数据 + `id`；
 * - 完整 manifest + README 存于 `packs/<owner>.<repo>/`，用 `fetchMarketPackDetail` 懒加载；
 * - 兼容旧式单 `downloadUrl`+`sha256`+`size` 与 `files[]` 指针式；
 * - 从下载地址/版本号自动判别 `.dspack`(v4/v5) 与 `.tgz`(v3) 旧格式。
 */

/** 默认市场索引：官方 GitHub Pages 站点（CI 每日刷新扫描 dsh-pack 标签仓库）。 */
export const DEFAULT_MARKET_INDEX = 'https://dsh-packforge.github.io/dsh-pack-market/index.json';

/** 由索引条目的 id/owner/repo 推导懒加载目录 key（`<owner>.<repo>`）。 */
export function packDirId(entry) {
  if (entry?.id && typeof entry.id === 'string') return entry.id;
  if (entry?.owner && entry?.repo) return `${entry.owner}.${entry.repo}`;
  return '';
}

/** 读取本地路径或 http(s) URL 的 index.json，返回归一化后的市场条目列表。
 *  解析异常/缺字段时不抛错，而是带 `error` 说明（packs 为空），便于调用方提示真实原因。 */
export async function readMarketIndex(host, indexPath) {
  const parsed = parseIndex(await fetchText(host, indexPath));
  const index = parsed.index;
  const raw = Array.isArray(index?.modpacks) ? index.modpacks : Array.isArray(index?.packs) ? index.packs : [];
  return {
    schemaVersion: index?.schemaVersion ?? 1,
    generatedAt: index?.generatedAt ?? null,
    packs: raw.map((e) => normalizeMarketPack(e)).filter(Boolean),
    error: parsed.error,
  };
}

/** 懒加载单个整合包的完整 manifest + README（来自 `packs/<owner>.<repo>/`）。
 *  由 index 路径推导 base：URL 去掉尾段 `index.json`；本地路径去掉文件名。
 *  返回 { manifest, readme, dir }：manifest 为解析后的对象（失败 null），readme 为原文（失败 ''）。 */
export async function fetchMarketPackDetail(host, indexPath, entry) {
  const dir = packDirId(entry);
  if (!dir) return { manifest: null, readme: '', dir: '' };
  const base = detailBase(indexPath);
  const manifestSrc = `${base}packs/${dir}/manifest.json`;
  const readmeSrc = `${base}packs/${dir}/README.md`;

  const [rawManifest, readme] = await Promise.all([
    fetchText(host, manifestSrc).catch(() => null),
    fetchText(host, readmeSrc).catch(() => ''),
  ]);
  let manifest = null;
  if (rawManifest) {
    try { manifest = JSON.parse(rawManifest); } catch { manifest = null; }
  }
  return { manifest, readme, dir };
}

/** 取文本（本地路径或 http(s) URL）：URL 走 host.download 拉到临时文件再读（读完清理）；否则当本地路径读。 */
async function fetchText(host, src) {
  if (!/^https?:\/\//i.test(src)) {
    return (await host.readTextFile(host.resolvePath(src))) ?? '';
  }
  const tmp = await host.mkdtemp('pfx-mkt-');
  try {
    const dest = host.joinPath(tmp, 'detail.json');
    await host.download(src, dest);
    return (await host.readTextFile(dest)) ?? '';
  } finally {
    await host.rm(tmp, { recursive: true, force: true }).catch(() => {});
  }
}

/** 由 index 路径推导 detail base（去掉 `index.json` 尾段，保留结尾分隔符）。 */
function detailBase(indexPath) {
  const s = String(indexPath ?? '');
  return s.replace(/index\.json$/i, '');
}

/** 解析索引 JSON：空内容 / 非 JSON / 缺数组字段时返回带 error 的对象。 */
function parseIndex(text) {
  const s = String(text ?? '').trim();
  if (!s) return { index: {}, error: '市场索引内容为空' };
  try {
    const index = JSON.parse(s);
    if (!Array.isArray(index?.modpacks) && !Array.isArray(index?.packs)) {
      return { index, error: '市场索引缺少 modpacks[] 或 packs[] 字段' };
    }
    return { index, error: null };
  } catch {
    return { index: {}, error: `市场索引不是有效 JSON（开头：${s.slice(0, 80)}）` };
  }
}

/** 归一化一条市场条目；不可识别返回 null。 */
export function normalizeMarketPack(entry, locale = 'zh-CN') {
  if (!entry || typeof entry.name !== 'string') return null;
  const urls = collectUrls(entry);
  const manifestVersion = entry.manifestVersion ?? inferManifestVersion(urls);
  return {
    name: entry.name,
    displayName: pickLocale(entry.displayName, locale, entry.name),
    description: pickLocale(entry.description, locale, ''),
    version: entry.version ?? '',
    author: entry.author ?? '',
    icon: pickLocale(entry.icon, locale, ''),
    dshVersion: entry.dshVersion ?? '',
    type: entry.type ?? inferType(manifestVersion),
    profileName: entry.profileName ?? entry.name,
    defaultProfile: entry.defaultProfile ?? '',
    category: entry.category ?? '',
    updatedAt: entry.updatedAt ?? '',
    manifestVersion,
    format: detectFormat(entry, urls, manifestVersion),
    urls,
    downloadUrl: urls[0] ?? '',
    sha256: entry.sha256 ?? entry.files?.[0]?.sha256 ?? '',
    size: entry.size ?? entry.files?.[0]?.size ?? 0,
    id: entry.id ?? '',
    owner: entry.owner ?? '',
    repo: entry.repo ?? '',
    // 精简索引不再平铺这些字段；完整值见 fetchMarketPackDetail 懒加载的 manifest。
    bundles: entry.bundles ?? [],
    dependencies: entry.dependencies ?? {},
    profiles: entry.profiles,
    presets: entry.presets,
    skills: entry.skills,
  };
}

function collectUrls(entry) {
  if (Array.isArray(entry.files) && entry.files.length) {
    const out = [];
    for (const f of entry.files) for (const u of f.urls ?? []) out.push(u);
    if (out.length) return out;
  }
  return entry.downloadUrl ? [entry.downloadUrl] : [];
}

function detectFormat(entry, urls, manifestVersion) {
  if (urls.some((u) => /\.dspack(\?|#|$)/i.test(u))) return 'dspack';
  if (urls.some((u) => /\.tgz(\?|#|$)/i.test(u))) return 'tgz';
  if ([4, 5, 6].includes(manifestVersion)) return 'dspack';
  if (manifestVersion === 3) return 'tgz';
  return 'unknown';
}

/** manifestVersion → type 兜底（未显式声明 type 时）。 */
function inferType(manifestVersion) {
  return (manifestVersion === 5 || manifestVersion === 6) ? 'dshhome' : 'profile';
}

function inferManifestVersion(urls) {
  if (urls.some((u) => /\.dspack/i.test(u))) return 4;
  if (urls.some((u) => /\.tgz/i.test(u))) return 3;
  return 0;
}

function pickLocale(map, locale, fallback) {
  if (typeof map === 'string' && map) return map;
  if (map && typeof map === 'object') {
    if (typeof map[locale] === 'string') return map[locale];
    const first = Object.values(map).find((v) => typeof v === 'string');
    if (first) return first;
  }
  return fallback ?? '';
}
