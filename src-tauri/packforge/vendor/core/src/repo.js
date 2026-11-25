// 导出「源仓库」（v6 源形态）：把 Profile 物化为可二次开发 / 重打包的 git 仓库。
// 三档内容（opts.content，默认 readme）：
//   - manifest：仅清单（manifest.json）
//   - readme  ：清单 + README.md
//   - full    ：全套（机器文件 + overrides/ + .dspackignore）
// 仓库目录名 = <out>/<name>（不带版本号）；release/ 始终产出 .dspack + .sha256（gitignore 不入库）；
// 首次 git init + commit，之后每次导出增量 commit；同版本 release 冲突抛 ReleaseConflictError。
// 布局遵循 pack-structure v3：机器文件进根目录，其余用户文件进 overrides/。
import { resolveLocale } from './manifest.js';
import { prepareProfileArchive, writeProfileArchive } from './pack.js';

// 发布清单和内嵌图标可单独浏览。其余归档条目仅随完整源码发布。
function sourceEntries(entries, content) {
  return Object.fromEntries(Object.entries(entries).filter(([path]) =>
    path !== 'manifest.json' && (content === 'full' || path.startsWith('assets/plugins/')),
  ));
}

async function checkedSourcePath(host, root, relative) {
  const parts = relative.split('/');
  if (!parts.length || parts.some((part) => !part || part === '.' || part === '..' || /[\\:\x00-\x1f]|[. ]$/u.test(part) || part.toLowerCase() === '.git') || parts[0].toLowerCase() === 'release') {
    throw new Error('源仓库文件路径无效');
  }
  let path = root;
  for (const part of parts) {
    if (await host.packEntryType(path) === 'symlink') throw new Error('源仓库路径包含链接：' + relative);
    path = host.joinPath(path, part);
  }
  if (await host.packEntryType(path) === 'symlink') throw new Error('源仓库路径包含链接：' + relative);
  return path;
}

// 只清理上次导出写入且内容未变的文件。手动修改过的旧资源须由作者决定是否继续发布。
async function planSourceFiles(host, root, inventoryFile, next) {
  const saved = await host.readTextFile(inventoryFile);
  const previous = saved ? JSON.parse(saved) : {};
  const remove = [];
  const hashes = Object.create(null);
  for (const [rel, data] of Object.entries(next)) {
    await checkedSourcePath(host, root, rel);
    hashes[rel] = await host.sha256(data);
  }
  for (const [rel, hash] of Object.entries(previous)) {
    if (Object.hasOwn(hashes, rel)) continue;
    const path = await checkedSourcePath(host, root, rel);
    const data = await host.readFile(path);
    if (!data) continue;
    if (await host.sha256(data) !== hash) throw new Error('上次发布的文件已被手动修改，请核对后再导出：' + rel);
    remove.push(path);
  }
  return { hashes, remove };
}

/** 合法内容档。 */
export const REPO_CONTENT_LEVELS = ['manifest', 'readme', 'full'];

/** 内容档 → 中文标签（GUI/CLI 展示）。 */
export const REPO_CONTENT_LABEL = {
  manifest: '仅清单（manifest.json）',
  readme: '清单 + README',
  full: '全套文件（overrides/ + release/）',
};

/**
 * 导出源仓库。
 * @param {Host} host
 * @param {{name:string,dir:string}} profile
 * @param {object} opts { out?, content?, replaceRelease?, displayName?, dshVersion?, ...其余透传 buildManifest/packProfile }
 *   - replaceRelease：undefined（冲突抛 ReleaseConflictError）/ true（覆盖）/ 'skip'（跳过 release，仍 commit 源码）
 * @returns {Promise<{dir,manifest,content,written:string[],readme:string,ignored:string,
 *                    release?:{dspack,sha256,sha256Value},git:{initialized,committed,reason},conflicted:boolean}>}
 */
export async function exportRepo(host, profile, opts = {}) {
  const content = REPO_CONTENT_LEVELS.includes(opts.content) ? opts.content : 'readme';
  const archive = await prepareProfileArchive(host, profile, opts);
  const { manifest, entries, scan } = archive;

  const parent = opts.out ? host.resolvePath(opts.out) : host.cwd();
  const repoDir = host.joinPath(parent, manifest.name); // 仓库名不带版本号
  const releaseDir = host.joinPath(repoDir, 'release');
  const releaseDspack = `${manifest.name}-${manifest.version}.dspack`;
  const releasePath = host.joinPath(releaseDir, releaseDspack);
  const releaseExists = (await host.stat(releasePath)) != null;

  // 版本冲突：同版本 release 产物已存在且未明确「覆盖/跳过」→ 抛错交上层决定
  if (releaseExists && opts.replaceRelease !== true && opts.replaceRelease !== 'skip') {
    throw new ReleaseConflictError(releasePath, manifest.name, manifest.version);
  }

  // 先产 release，拿到 .dspack 校验和写进仓库根部的清单（清单在 .dspack 之外，不构成循环）；
  // .dspack 内部的 manifest（由 packProfile 生成）不带该字段。
  for (const dir of [repoDir, releaseDir]) {
    if (await host.packEntryType(dir) === 'symlink') throw new Error('源仓库输出目录不能是链接');
  }
  const inventoryFile = host.joinPath(releaseDir, '.source-files.json');
  for (const generated of [inventoryFile, releasePath, releasePath + '.sha256']) {
    if (await host.packEntryType(generated) === 'symlink') throw new Error('源仓库产物不能写入链接');
  }
  for (const file of ['manifest.json', 'README.md', '.gitignore', '.dspackignore']) {
    await checkedSourcePath(host, repoDir, file);
  }
  const source = sourceEntries(entries, content);
  const { hashes, remove } = await planSourceFiles(host, repoDir, inventoryFile, source);
  let release = null;
  if (!(opts.replaceRelease === 'skip' && releaseExists)) {
    const pack = await writeProfileArchive(host, archive, {
      ...opts,
      out: releaseDir,
      force: opts.replaceRelease === true,
    });
    const shaName = `${releaseDspack}.sha256`;
    await host.writeTextFile(host.joinPath(releaseDir, shaName), `${pack.sha256}  ${releaseDspack}\n`);
    release = { dspack: releaseDspack, sha256: shaName, sha256Value: pack.sha256 };
    manifest.sha256 = pack.sha256; // 顶层 sha256 = release .dspack 校验和
  }

  const written = [];
  const writeText = async (name, body) => {
    await host.writeTextFile(host.joinPath(repoDir, name), body);
    written.push(name);
  };

  // 1) 清单（所有档；含 release sha256）
  await writeText('manifest.json', `${JSON.stringify(manifest, null, 2)}\n`);

  // 2) README（readme / full）
  const readme = content === 'manifest' ? '' : renderReadme(manifest, scan);
  if (content !== 'manifest') await writeText('README.md', readme);

  // 3) 全套源码 + .dspackignore（full）
  let ignored = '';
  if (content === 'full') {
    ignored = renderDspackIgnore();
    await writeText('.dspackignore', ignored);
  }
  for (const path of remove) await host.rm(path, { recursive: false, force: true });
  for (const [dest, data] of Object.entries(source)) {
    await host.writeFile(host.joinPath(repoDir, ...dest.split('/')), data);
    written.push(dest);
  }
  await host.writeTextFile(inventoryFile, JSON.stringify(hashes));

  // 4) .gitignore（所有档：release 产物不入版本管理）
  await writeText('.gitignore', renderGitignore());

  // 5) git init + add + commit（容错：无 git / 无变更时降级）
  const git = await commitRepo(host, repoDir, manifest);

  return {
    dir: repoDir,
    manifest,
    content,
    written,
    readme,
    ignored,
    release,
    git,
    conflicted: releaseExists,
  };
}

/** 版本冲突错误：release/ 已存在同版本产物。 */
export class ReleaseConflictError extends Error {
  constructor(file, name, version) {
    super(`版本冲突：release/ 已存在 ${name}@${version}（${file}）`);
    this.name = 'ReleaseConflictError';
    this.code = 'RELEASE_CONFLICT';
    this.file = file;
    this.packName = name;
    this.packVersion = version;
  }
}

/** 生成 .gitignore（git 版本管理忽略规则：release 产物不入库）。 */
export function renderGitignore() {
  return '# DSH PackForge 自动生成：release/ 产物不纳入版本管理\nrelease/\n';
}

/** git init + add + commit；无 git 或提交无变更时降级（不算失败）。 */
async function commitRepo(host, repoDir, manifest) {
  const message = `export:${manifest.name}@${manifest.version}`;
  const runGit = async (args) => {
    if (typeof host.git === 'function') {
      const result = await host.git(args, {cwd:repoDir});
      return result;
    }
    if (args.some(arg => /[^a-zA-Z0-9_.:@+\/-]/u.test(arg))) throw new Error('当前宿主不能安全传递 Git 参数');
    return host.exec('git', args, {cwd:repoDir});
  };
  let gitOk = false;
  try {
    const r = await runGit(['--version']);
    gitOk = r?.status === 0;
  } catch { gitOk = false; }
  if (!gitOk) return { initialized: false, committed: false, reason: 'git 不可用' };

  const dotGit = host.joinPath(repoDir, '.git');
  const existed = (await host.stat(dotGit)) != null;
  if (!existed) await runGit(['init']);
  await runGit(['add', '-A']);
  const r = await runGit(['commit', '-m', message]);
  const committed = r?.status === 0;
  return { initialized: !existed, committed, reason: committed ? '' : '无变更或提交失败' };
}

/**
 * 生成 README.md（markdown）。描述来源为 manifest v5（displayName/description 支持 i18n map）。
 * @param {object} m manifest v5
 * @param {{files:Array,excluded:Array}} scan 扫描结果（用于文件清单段落）
 */
export function renderReadme(m, scan = { files: [], excluded: [] }) {
  const disp = resolveLocale(m.displayName, 'zh-CN') || m.name;
  const desc = resolveLocale(m.description, 'zh-CN');
  const bundles = (m.bundles ?? []).map((b) => `- \`${b}\``).join('\n') || '（无）';
  const deps = Object.entries(m.dependencies ?? {})
    .map(([k, v]) => `- \`${k}\` @ \`${v}\``)
    .join('\n') || '（无）';

  return [
    `# ${disp}`,
    '',
    desc || '（无描述）',
    '',
    `> 由 DSH PackForge 生成 · manifest v${m.manifestVersion} · type=${m.type || 'profile'}`,
    '',
    '## 元信息',
    '',
    '| 字段 | 值 |',
    '| --- | --- |',
    `| 整合包 | \`${m.name}\` v\`${m.version}\` |`,
    `| DSH 版本 | ${m.dshVersion ? `\`${m.dshVersion}\`` : '未钉定（安装端兜底）'} |`,
    `| 作者 | ${m.author || '—'} |`,
    `| 层栈 | ${(m.bundles ?? []).length} 个 bundle |`,
    `| 依赖 | ${Object.keys(m.dependencies ?? {}).length} 个 |`,
    '',
    '## 层栈（bundles）',
    '',
    bundles,
    '',
    '## 依赖（坐标 → 固定版本）',
    '',
    deps,
    '',
    '## 文件清单',
    '',
    `源 Profile 共 ${scan.files?.length ?? 0} 个文件（排除 ${scan.excluded?.length ?? 0} 个命中规则项）。`,
    '打包时机器文件（`package.json` / `pnpm-workspace.yaml` / `pnpm-lock.yaml`）进根目录，其余进 `overrides/`。',
    '',
    '## 使用',
    '',
    '- 分发：重打包生成 `.dspack`（产物输出到 `release/`）',
    `- 安装：\`dspack install release/${m.name}-${m.version}.dspack\``,
    '',
  ].join('\n');
}

/**
 * 生成 .dspackignore（gitignore 风格）——与引擎安全规则 security.js 对齐，
 * 另追加仓库自身不入包项（.git/ / release/ / .dspackignore / README.md）。
 */
export function renderDspackIgnore() {
  return [
    '# DSH PackForge 打包忽略规则（自动生成；与引擎安全规则 security.js 对齐）',
    '# 重打包为 .dspack 时，命中以下模式的文件/目录不会进入整合包。',
    '',
    '# --- 依赖与构建产物 ---',
    'node_modules/',
    'dist/',
    'build/',
    'coverage/',
    '.cache/',
    '.turbo/',
    '.pnpm-store/',
    '',
    '# --- 自动生成的 DSH 配置（安装时由层栈 + patch 重新合成） ---',
    'cordis.yml',
    '',
    '# --- 打包产物（防嵌套） ---',
    'manifest.json',
    '*.dspack',
    '*.tgz',
    '*.tar.gz',
    '*.zip',
    '',
    '# --- 其它包管理器锁文件（DSH 固定使用 pnpm） ---',
    'package-lock.json',
    'yarn.lock',
    '',
    '# --- 日志 ---',
    'npm-debug.log',
    'pnpm-debug.log',
    '',
    '# --- 敏感文件 ---',
    '.env',
    '.env.*',
    '.npmrc',
    '.netrc',
    '.yarnrc',
    '.yarnrc.yml',
    '.pypirc',
    '.npmignore',
    '*.key',
    '*.pem',
    '*.p12',
    '*.pfx',
    '*.crt',
    '*.der',
    '*.asc',
    '*.credentials',
    'credentials*.yml',
    'id_rsa*',
    'id_ed25519*',
    'id_ecdsa*',
    'id_ed448*',
    'id_dsa*',
    'secrets*.json',
    'secrets*.yml',
    '*api_key*',
    '*token*',
    '',
    '# --- 仓库自身（不入包） ---',
    '.git/',
    '.dspackignore',
    'release/',
    'README.md',
    '',
  ].join('\n');
}