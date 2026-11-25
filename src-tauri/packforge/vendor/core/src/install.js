import { managedInstall } from './managed-install.js';
import { applyPluginSources, verifyActivePluginSources } from './registry.js';
import { parseDspack, decodeText } from './dspack.js';
import { validateManifest, coordsToPkgDeps, sanitizeSlug } from './manifest.js';
import { listInstalledDshVersions } from './discovery.js';

/**
 * 一键安装：读取本地/URL 的 .dspack → 校验头 & manifest → 按 type 分支安装。
 * - profile（v5）：单 profile，装到 $DSH_HOME/profiles/<name>（现有语义，假设 DSH 已装）；
 * - dshhome（v5）：整个 DSH_HOME 快照，顺序为「先确保 dshVersion 已装 → 建 home →
 *   逐 profile install → home 级资源 → files[]/skills[] 下载」。任一环节失败整体回滚。
 *
 * 安全要点：
 * - .dspack 为标准 ZIP，格式合法性由 parseDspack（dspack.json 标记）与 manifest 校验共同保证；
 * - overrides/ 与 files[].path 的相对路径禁用 `..` 段（防逃逸）；
 * - target 名经 sanitizeSlug 规范化；同名目标无 --force 时报错；
 * - 指定 expectedSha256/expectedSize 时先做完整性校验。
 *
 * @param {Host} host
 * @param {object} opts { source, profilesRoot?, home?, name?, registry?, force?, noInstall?, dryRun?,
 *                        timeoutMs?, installedDshVersions?, expectedSha256?, expectedSize? }
 * @returns {Promise<object>}
 */
export async function installPack(host, opts = {}) {
  const { source } = opts;
  if (!source) throw new Error('请指定要安装的整合包（本地 .dspack 路径或 URL）');

  // 进度回调（GUI 用它避免「卡住」观感）：onProgress(stage, detail)
  const progress = (stage, detail) => {
    if (typeof opts.onProgress === 'function') opts.onProgress(stage, detail);
  };

  const profilesRoot = opts.profilesRoot || host.joinPath(host.homedir(), '.dsh', 'profiles');

  progress('download', typeof source === 'string' && /^https?:\/\//i.test(source) ? '下载整合包' : '读取整合包');
  const { path: packPath, tempDir } = await resolvePackSource(host, source);

  try {
    await verifyIntegrity(host, packPath, opts);

    progress('extract', '解析并校验整合包');
    const bytes = await host.readFile(packPath);
    if (!bytes) throw new Error('无法读取整合包文件');
    const archiveSha256 = await host.sha256(bytes);
    if (opts.expectedSha256 && archiveSha256 !== opts.expectedSha256.toLowerCase()) {
      throw new Error('读取时归档内容已变化，请重新预览');
    }
    const { entries } = parseDspack(bytes);

    if (!entries['manifest.json']) throw new Error('整合包缺少 manifest.json（不是有效的 .dspack）');
    const manifest = parseJson(decodeText(entries['manifest.json']));
    const errors = validateManifest(manifest);
    if (errors.length) throw new Error(`整合包不合法：${errors.join('；')}`);
    if (opts.validateArchive) await opts.validateArchive(manifest, entries);

    const stage = (home, authProfilesRoot) => {
      const staged = { ...opts, home, authProfilesRoot, dryRun: false, noInstall: true, force: false };
      const profiles = host.joinPath(home, 'profiles');
      return manifest.type === 'dshhome'
        ? installDshHome(host, manifest, entries, staged, progress)
        : installProfile(host, manifest, entries, staged, progress, profiles);
    };
    if (manifest.manifestVersion === 6 || opts.update) {
      return await managedInstall(host, manifest, entries, { ...opts, archiveSha256 }, {
        stage,
        verify: (dir, unit, verified) => verifyActivePluginSources(host, dir, unit, verified, {prepareLock:false}),

        previous: async (previousHome) => {
          if (!/^[a-f0-9]{64}$/i.test(opts.previousSha256 ?? '')) {
            throw new Error('旧包需要可核验的 SHA-256');
          }
          const oldPack = await resolvePackSource(host, opts.previousSource);
          try {
            await verifyIntegrity(host, oldPack.path, { expectedSha256: opts.previousSha256 });
            const old = parseDspack(await host.readFile(oldPack.path));
            const baseline = JSON.parse(decodeText(old.entries['manifest.json']));
            if (baseline.name !== manifest.name || (baseline.type ?? 'profile') !== (manifest.type ?? 'profile') ||
                baseline.manifestVersion >= 6) {
              throw new Error('用于迁移的旧包必须是同一整合包的历史格式和相同类型');
            }
            await installPack(host, {
              source: oldPack.path, expectedSha256: opts.previousSha256, noInstall: true,
              profilesRoot: host.joinPath(previousHome, 'profiles'), home: previousHome,
              name: opts.name || manifest.profileName || manifest.name, targetProfileName: opts.targetProfileName,
              baselineOnly: true,
            });
            return { stagedHome: previousHome, manifest: baseline, sha256: opts.previousSha256, expectedSha256: opts.previousSha256 };
          } finally {
            if (oldPack.tempDir) await host.rm(oldPack.tempDir, { recursive: true, force: true });
          }
        },
        finalize: async ({ affectedProfiles, stagedProfiles }) => {
          for (const dir of affectedProfiles) {
            const pkg = JSON.parse(await host.readTextFile(host.joinPath(dir, 'package.json')));
            const unit = manifest.type === 'dshhome' ? manifest.profiles[host.basename(dir)] : manifest;
            await verifyActivePluginSources(host, dir, unit, host.joinPath(stagedProfiles, host.basename(dir)));
            progress('install', '安装配置档依赖 ' + host.basename(dir));
            await pnpmInstall(host, dir, opts, !!(await host.stat(host.joinPath(dir, 'pnpm-lock.yaml'))));

            const check = await reconcileProfile(host, dir, { dependencies: pkg.dependencies, bundles: pkg.dsh?.profile?.bundles });
            if (check.missing.length) throw new Error('无法解析插件层栈：' + check.missing.join(', '));
          }
        },
      });
    }
    if (manifest.manifestVersion === 5 && manifest.type === 'dshhome') {
      return await installDshHome(host, manifest, entries, opts, progress);
    }
    return await installProfile(host, manifest, entries, opts, progress, profilesRoot);
  } finally {
    if (tempDir) await host.rm(tempDir, { recursive: true, force: true }).catch(() => {});
  }
}

/** 单 profile（v5）安装：现有语义，装到 profilesRoot/<name>。 */
async function installProfile(host, manifest, entries, opts, progress, profilesRoot) {
  if (manifest.manifestVersion === 6) await ensureDsh(host, manifest, opts);
  const profileName = opts.targetProfileName ?? sanitizeSlug(opts.name || manifest.profileName || manifest.name);
  if (!profileName) throw new Error('无法确定 Profile 名称');
  const target = host.joinPath(profilesRoot, profileName);

  // dry-run 只读预览：即便目标已存在也照常返回计划（真实安装才要求 --force）。
  if (opts.dryRun) {
    const exists = (await host.stat(target)) != null;
    return { profileName, dir: target, manifest, dryRun: true, installed: false, reconcile: null, filesDownloaded: 0, exists };
  }

  if ((await host.stat(target)) != null && !opts.force) {
    throw new Error(`Profile「${profileName}」已存在：${target}（使用 --force 覆盖）`);
  }
  if ((await host.stat(target)) != null) {
    await host.rm(target, { recursive: true, force: true });
  }
  await host.mkdir(target);

  try {
    progress('extract', '写入 overrides/ 与 package.json');
    await materializePackage(host, target, manifest, entries);
    if (manifest.manifestVersion === 6) {
      await applyPluginSources(host, target, manifest, {
        vendored: manifest.vendored, entries, authDir: host.joinPath(opts.authProfilesRoot, profileName),
      });
    }
    // home/ → $DSH_HOME 根（上一级目录内容：全局 skill / 预设），与 overrides/（profile 根）并列。
    await materializeHome(host, host.joinPath(profilesRoot, '..'), entries);

    let installed = false;
    let reconcile = null;
    if (!opts.noInstall) {
      installed = true;
      progress('install', '运行 pnpm install（依赖重建，可能较慢）');
      await pnpmInstall(host, target, opts, !!entries['pnpm-lock.yaml']);
      reconcile = await reconcileProfile(host, target, manifest);
      if (reconcile.missing.length > 0) {
        throw new Error(
          `整合包层栈有 ${reconcile.missing.length} 个 bundle 无法解析为补丁层：` +
            `${reconcile.missing.join(', ')}。这些包未声明 dsh.bundle.patch，属于无效的整合包。`,
        );
      }
    }

    const files = manifest.files ?? [];
    if (files.length) progress('files', `下载 ${files.length} 个 files[] 条目`);
    const filesDownloaded = await downloadFiles(host, target, files);

    progress('done', profileName);
    return { profileName, dir: target, manifest, dryRun: false, installed, reconcile, filesDownloaded };
  } catch (e) {
    await host.rm(target, { recursive: true, force: true }).catch(() => {});
    throw e;
  }
}

/** dshhome（v5）安装：整个 DSH_HOME 快照，顺序「先装 DSH → 建 home → 逐 profile → home 级资源 → 指针下载」。 */
async function installDshHome(host, manifest, entries, opts, progress) {
  // ① 先确保 dshVersion 的 DSH 已安装：多个 profile 的 bundle 都靠安装基线解析。
  await ensureDsh(host, manifest, opts);

  const homeRoot = opts.home ? host.resolvePath(opts.home) : host.joinPath(host.homedir(), '.dsh');

  if (opts.dryRun) {
    const exists = (await host.stat(homeRoot)) != null;
    return {
      type: 'dshhome', dir: homeRoot, manifest, dryRun: true, installed: false, exists,
      profiles: Object.keys(manifest.profiles), defaultProfile: manifest.defaultProfile, filesDownloaded: 0,
    };
  }

  if ((await host.stat(homeRoot)) != null && !opts.force) {
    throw new Error(`目标 DSH_HOME 已存在：${homeRoot}（使用 --force 覆盖）`);
  }
  if ((await host.stat(homeRoot)) != null) {
    await host.rm(homeRoot, { recursive: true, force: true });
  }
  await host.mkdir(homeRoot);

  try {
    // ② 逐 profile：overrides/profiles/<name>/ 落盘 → pnpm install → 对账（各自独立）
    const installed = [];
    for (const [name, unit] of Object.entries(manifest.profiles)) {
      const profileDir = host.joinPath(homeRoot, 'profiles', name);
      progress('extract', `写入 profile「${name}」`);
      await materializeProfile(host, profileDir, name, unit, entries);
      if (manifest.manifestVersion === 6) {
        await applyPluginSources(host, profileDir, unit, {
          vendored: manifest.vendored, entries, authDir: host.joinPath(opts.authProfilesRoot, name),
        });
      }

      if (!opts.noInstall) {
        progress('install', `运行 pnpm install（${name}，可能较慢）`);
        await pnpmInstall(host, profileDir, opts, !!entries['pnpm-lock.yaml']);
        const reconcile = await reconcileProfile(host, profileDir, unit);
        if (reconcile.missing.length > 0) {
          throw new Error(
            `profile「${name}」层栈有 ${reconcile.missing.length} 个 bundle 无法解析为补丁层：` +
              `${reconcile.missing.join(', ')}。这些包未声明 dsh.bundle.patch。`,
          );
        }
      }
      installed.push(name);
    }

    // ③ home 级 overrides：.agent-presets/ skills/ AGENTS.md data/ 等
    progress('extract', '写入 home 级资源（preset / skill / 指令 / 数据）');
    await materializeHomeOverrides(host, homeRoot, entries);

    // ④ files[] + 重 skills[] 指针下载
    const heavySkills = (manifest.skills ?? [])
      .filter((s) => s.sha256 && s.size)
      .map((s) => ({ path: s.path, sha256: s.sha256, size: s.size, urls: s.urls }));
    const all = [...(manifest.files ?? []), ...heavySkills];
    if (all.length) progress('files', `下载 ${all.length} 个 files[]/skills[] 条目`);
    const filesDownloaded = await downloadFiles(host, homeRoot, all);

    progress('done', manifest.name);
    return {
      type: 'dshhome', dir: homeRoot, manifest, dryRun: false, installed: true,
      profiles: installed, defaultProfile: manifest.defaultProfile, filesDownloaded,
    };
  } catch (e) {
    await host.rm(homeRoot, { recursive: true, force: true }).catch(() => {});
    throw e;
  }
}

/** 检查 dshVersion 是否已安装（未钉定则跳过）；未装报错提示先用启动器装。 */
async function ensureDsh(host, manifest, opts) {
  if (opts.baselineOnly) return;
  const selected = manifest.manifestVersion === 6 ? opts.selectedDshVersion : undefined;
  const wanted = selected || manifest.dshVersion;
  if (!wanted) return;
  if (selected) {
    const choices = manifest.dshVersions || (manifest.dshVersion ? [manifest.dshVersion] : []);
    if (choices.length && !choices.includes(selected)) throw new Error('选定的 DSH 版本不在整合包的兼容版本中');
  }
  const installed = opts.installedDshVersions ?? (await listInstalledDshVersions(host));
  if (!Array.isArray(installed) || !installed.includes(wanted)) {
    const have = Array.isArray(installed) ? installed.join(', ') || '无' : '未知';
    throw new Error(`整合包依赖 DSH ${wanted}，但本机未安装（已装：${have}）。请先用启动器安装该版本后再导入。`);
  }
}

/* ------------------------------------------------------------------ */

/** 把「本地路径 | http(s) URL」统一解析为本地文件路径；URL 会下载到临时目录（调用方负责清理返回的 tempDir）。 */
export async function resolvePackSource(host, source) {
  if (/^https?:\/\//i.test(source)) {
    const tempDir = await host.mkdtemp('dspack-dl-');
    const dest = host.joinPath(tempDir, 'pack.dspack');
    await host.download(source, dest);
    return { path: dest, tempDir };
  }
  const local = host.resolvePath(source);
  const st = await host.stat(local);
  if (!st?.isFile) throw new Error(`找不到整合包文件：${local}`);
  return { path: local, tempDir: null };
}

/** 完整性校验（可选，来自市场索引的 sha256/size）。 */
export async function verifyIntegrity(host, packPath, opts = {}) {
  // 只有「正整数」的 expectedSize 才参与校验：0 / 缺失 / 非数字一律视为未提供，
  // 避免市场条目缺 size 时把 0 当成「期望 0 字节」误判（files 不可能为 0 字节）。
  if (Number.isInteger(opts.expectedSize) && opts.expectedSize > 0) {
    const st = await host.stat(packPath);
    if (!st || st.size !== opts.expectedSize) {
      throw new Error(`大小不符：期望 ${opts.expectedSize} 字节，实际 ${st?.size ?? '未知'}`);
    }
  }
  if (opts.expectedSha256) {
    const actual = await host.sha256File(packPath);
    if (actual !== opts.expectedSha256.toLowerCase()) {
      throw new Error(`sha256 校验失败（可能被篡改）：期望 ${opts.expectedSha256}，实际 ${actual}`);
    }
  }
}

/** 单 profile：overrides/* 落盘 + 重建 package.json + 快照机器文件。 */
async function materializePackage(host, dir, manifest, entries) {
  // 1) overrides/* → profile 根
  for (const [entryPath, data] of Object.entries(entries)) {
    if (!entryPath.startsWith('overrides/')) continue;
    const rel = safeRel(entryPath.slice('overrides/'.length));
    if (!rel) continue;
    await host.writeFile(host.joinPath(dir, rel), data);
  }

  // 2) package.json：以 manifest 为唯一事实源重建 dependencies / bundles；保留快照其余字段
  const base = parseJson(decodeText(entries['package.json'] || new Uint8Array())) ?? {};
  const pkg = base && typeof base === 'object' && !Array.isArray(base) ? base : {};
  pkg.dependencies = coordsToPkgDeps(manifest.dependencies ?? {});
  if (manifest.manifestVersion === 6) {
    delete pkg.scripts;
    delete pkg.packageManager;
    delete pkg.devEngines;
    delete pkg.peerDependencies;
    delete pkg.peerDependenciesMeta;
    for (const field of ['optionalDependencies', 'devDependencies']) {
      if (pkg[field] && typeof pkg[field] === 'object') {
        pkg[field] = Object.fromEntries(Object.keys(pkg[field]).filter(id => Object.hasOwn(pkg.dependencies,id)).map(id => [id,pkg.dependencies[id]]));
      } else delete pkg[field];
    }
    if (pkg.dsh?.packforge) delete pkg.dsh.packforge;
  }
  pkg.dsh = { ...(pkg.dsh ?? {}), profile: { ...(pkg.dsh?.profile ?? {}), bundles: manifest.bundles ?? [] } };
  await host.writeTextFile(host.joinPath(dir, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');

  // 3) pnpm 设置 / 锁文件快照（根机器文件，可选）
  for (const name of ['pnpm-workspace.yaml', 'pnpm-lock.yaml']) {
    if (entries[name]) await host.writeFile(host.joinPath(dir, name), entries[name]);
  }

  // 4) cordis.patch.yml：overrides 已优先落地；缺则回退 manifest.patch
  const patchPath = host.joinPath(dir, 'cordis.patch.yml');
  if ((await host.stat(patchPath)) == null && typeof manifest.patch === 'string') {
    await host.writeTextFile(patchPath, manifest.patch);
  }
}

/** 单 profile 包携带的 home 级内容：home/* → $DSH_HOME 根（上一级目录）。 */
async function materializeHome(host, homeRoot, entries) {
  for (const [entryPath, data] of Object.entries(entries)) {
    if (!entryPath.startsWith('home/')) continue;
    const rel = safeRel(entryPath.slice('home/'.length));
    if (!rel) continue;
    await host.writeFile(host.joinPath(homeRoot, rel), data);
  }
}

/** dshhome 单个 profile：overrides/profiles/<name>/ 落盘 + 以 ProfileUnit 重建 package.json / patch。 */
async function materializeProfile(host, profileDir, name, unit, entries) {
  await host.mkdir(profileDir);

  // 1) overrides/profiles/<name>/* → profile 根
  const prefix = `overrides/profiles/${name}/`;
  for (const [entryPath, data] of Object.entries(entries)) {
    if (!entryPath.startsWith(prefix)) continue;
    const rel = safeRel(entryPath.slice(prefix.length));
    if (!rel) continue;
    await host.writeFile(host.joinPath(profileDir, rel), data);
  }

  // 2) package.json：以 ProfileUnit 为唯一事实源（bundle 层栈 + 坐标→依赖）
  const pkg = {
    name: `dsh-profile-${name}`,
    private: true,
    dependencies: coordsToPkgDeps(unit.dependencies ?? {}),
    dsh: { profile: { bundles: unit.bundles ?? [] } },
  };
  await host.writeTextFile(host.joinPath(profileDir, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');

  // 3) cordis.patch.yml：overrides 优先；缺则回退 unit.patch
  const patchPath = host.joinPath(profileDir, 'cordis.patch.yml');
  if ((await host.stat(patchPath)) == null && typeof unit.patch === 'string') {
    await host.writeTextFile(patchPath, unit.patch);
  }
}

/** dshhome：home 级 overrides（.agent-presets/ skills/ AGENTS.md data/ 等，profiles/ 除外）落盘。 */
async function materializeHomeOverrides(host, homeRoot, entries) {
  for (const [entryPath, data] of Object.entries(entries)) {
    if (!entryPath.startsWith('overrides/')) continue;
    const rel = safeRel(entryPath.slice('overrides/'.length));
    if (!rel) continue;
    if (rel.startsWith('profiles/')) continue; // profile 已在逐 profile 阶段单独落盘
    await host.writeFile(host.joinPath(homeRoot, rel), data);
  }
}

async function pnpmInstall(host, target, opts, frozen) {
  // 依赖重建可能较慢（尤其 git 依赖走 git clone），给足超时但绝不无限卡死。
  const timeoutMs = opts.timeoutMs > 0 ? opts.timeoutMs : 10 * 60 * 1000;
  const args = ['install'];
  if (frozen) args.push('--frozen-lockfile');
  if (opts.registry) args.push('--registry', opts.registry);
  let r = await host.exec('pnpm', args, { cwd: target, timeoutMs });
  // frozen-lockfile 失配时回退普通安装（v4/v5 导入语义）
  if (frozen && r.status !== 0) {
    r = await host.exec('pnpm', ['install', ...(opts.registry ? ['--registry', opts.registry] : [])], { cwd: target, timeoutMs });
  }
  if (r.error) throw new Error(`pnpm install 执行失败：${r.error}`);
  if (r.status !== 0) throw new Error(`pnpm install 失败（退出码 ${r.status ?? '未知'}）`);
}

/** files[] 重内容：每个 url 依次尝试下载 → sha256+size 校验 → 落到 path。 */
async function downloadFiles(host, target, files) {
  let count = 0;
  const tmp = await host.mkdtemp('dspack-files-');
  try {
    for (const f of files ?? []) {
      const rel = safeRel(f.path);
      const tmpFile = host.joinPath(tmp, `dl-${count}`);
      let ok = false;
      let lastErr = null;
      for (const url of f.urls) {
        try {
        await host.download(url, tmpFile, { maxBytes: f.size });
          const sha = await host.sha256File(tmpFile);
          const st = await host.stat(tmpFile);
          if (sha !== String(f.sha256).toLowerCase() || (st?.size ?? -1) !== f.size) {
            throw new Error('完整性校验失败');
          }
          ok = true;
          break;
        } catch (e) {
          lastErr = e;
        }
      }
      if (!ok) throw new Error(`files[] 下载失败：${rel}（${lastErr?.message ?? '无可用源'}）`);

      await host.move(tmpFile, host.joinPath(target, rel));
      count += 1;
    }
    return count;
  } finally {
    await host.rm(tmp, { recursive: true, force: true }).catch(() => {});
  }
}

/**
 * 安装后对账（v4/v5 沿用 DSH 语义：Bundle 唯一判据 = dsh.bundle.patch 声明存在）。
 * 对 dshhome 的 ProfileUnit 同样适用（unit 含 bundles / dependencies）。
 * @returns {{ missing: string[], added: string[] }}
 *   - missing：层栈中「同时是依赖」的包，装完无 dsh.bundle.patch → 无效，需回滚；
 *   - added：依赖里声明了 dsh.bundle.patch 却未进层栈 → 自动补进（调用方需写回 manifests）。
 */
export async function reconcileProfile(host, profileDir, manifest) {
  const missing = [];
  const added = [];
  const pkgDeps = coordsToPkgDeps(manifest.dependencies ?? {});
  const bundles = [...(manifest.bundles ?? [])];

  for (const name of bundles) {
    if (!Object.hasOwn(pkgDeps, name)) continue; // 模板型 bundle：由 DSH 安装目录 fallback 兜底
    const pkg = await readJson(host, host.joinPath(profileDir, 'node_modules', name, 'package.json'));
    if (!pkg || pkg.dsh?.bundle?.patch === undefined) missing.push(name);
  }
  for (const name of Object.keys(pkgDeps)) {
    const pkg = await readJson(host, host.joinPath(profileDir, 'node_modules', name, 'package.json'));
    if (pkg && pkg.dsh?.bundle?.patch !== undefined && !bundles.includes(name)) {
      bundles.push(name);
      added.push(name);
    }
  }
  return { missing, added };
}

/* ------------------- 工具 ------------------- */

function parseJson(raw) {
  if (raw == null || raw === '') return null;
  try {
    return raw instanceof Uint8Array ? JSON.parse(decodeText(raw)) : JSON.parse(raw);
  } catch {
    return null;
  }
}

async function readJson(host, p) {
  return parseJson(await host.readTextFile(p));
}

/** 归一化相对路径并拒绝危险段（防路径穿越）。 */
function safeRel(rel) {
  const r = String(rel).replace(/\\/g, '/');
  if (!r || r.startsWith('/') || /^[a-zA-Z]:/.test(r)) throw new Error(`非法相对路径：${rel}`);
  if (r.split('/').includes('..')) throw new Error(`路径含危险段 '..'：${rel}`);
  return r;
}
