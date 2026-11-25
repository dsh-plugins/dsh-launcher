import { sanitizeSlug } from './manifest.js';
import { planExtensionInstall } from './extensions.js';
import { packBaselineFiles, installedSkillLocation, recoverPackUpdate, syncPackTree } from './update.js';

/** 在独立目录中还原包内容，再以安装基线为准提交文件、配置和依赖。 */
export async function managedInstall(host, manifest, entries, opts, handlers) {
  const whole = manifest.type === 'dshhome';
  const profilesRoot = host.resolvePath(opts.profilesRoot || host.joinPath(host.homedir(), '.dsh', 'profiles'));
  const home = whole
    ? host.resolvePath(opts.home || host.joinPath(host.homedir(), '.dsh'))
    : host.resolvePath(host.joinPath(profilesRoot, '..'));
  const profileName = whole ? undefined : opts.targetProfileName ?? sanitizeSlug(opts.name || manifest.profileName || manifest.name);
  const names = whole ? Object.keys(manifest.profiles) : [profileName];
  for (const name of names) {
    if (!name || !/^[^\\/:*?"<>|\x00-\x1f]+$/u.test(name) || /[. ]$/u.test(name) || name === '.' || name === '..') {
      throw new Error('整合包中的配置档名称无效');
    }
  }
  const target = whole ? home : host.joinPath(profilesRoot, profileName);
  const profilesDir = whole ? 'profiles' : host.basename(profilesRoot);
  if (opts.recoverInterrupted) await recoverPackUpdate(host, home, profilesDir);
  const exists = Boolean(await host.stat(target));
  const updating = !!(opts.update || (exists && opts.force));
  if (updating && !exists) throw new Error(`找不到要更新的目标：${target}`);
  if (exists && !updating && !opts.dryRun) throw new Error(`目标已存在，请使用更新操作：${target}`);
  const scratch = await host.mkdtemp('dspack-prepare-');
  const stagedHome = host.joinPath(scratch, 'home');
  try {
    const skillLocation = whole ? null : await installedSkillLocation(host, home, profilesDir, profileName);
    const skillRoot = (root) => skillLocation ? host.joinPath(root, skillLocation) : handlers.skillRoot?.(root, profileName);
    const listing = opts.dryRun && opts.listResourcesOnly;
    const result = listing ? null : await handlers.stage(stagedHome, whole ? host.joinPath(home, 'profiles') : profilesRoot);
    const plan = await planExtensionInstall(host, {
      home: stagedHome, staging: true, profileName, profileDir: whole ? undefined : host.joinPath(stagedHome, 'profiles', profileName),
      skillRoot: skillRoot(stagedHome), actualSkillRoot: skillRoot(home), manifest, entries,
      credentialHome: home, credentialProfileDir: whole ? undefined : target,
      credentialCwd: opts.credentialCwd,
      credentialEnvironment: opts.credentialEnvironment,
      baselineFiles: await packBaselineFiles(host,home,manifest,profileName,profilesDir), profilesDir,
      installAnchor: opts.installAnchor, overlayPaths: opts.overlayPaths,
      credentials: opts.credentials, confirmedCredentials: opts.confirmedCredentials,
      decisions: opts.decisions,
    });
    const resourceKeys = new Set(plan.items.map(item => item.key));
    for (const key of Object.keys(opts.decisions || {})) {
      if (!resourceKeys.has(key)) throw new Error('未找到指定的导入资源：' + key);
    }
    if (listing) return { resourcesOnly: true, manifest, dir: target, exists, items: plan.items, dryRun: true };
    await plan.apply();
    const previous = updating && opts.previousSource ? await handlers.previous(host.joinPath(scratch, 'previous')) : undefined;
    if (profilesDir !== 'profiles') {
      for (const root of [stagedHome, previous?.stagedHome].filter(Boolean)) {
        await host.move(host.joinPath(root, 'profiles'), host.joinPath(root, profilesDir));
      }
    }
    const mapProfilePath = rel => profilesDir !== 'profiles' && rel.startsWith('profiles/') ? profilesDir + rel.slice(8) : rel;
    const changes = await syncPackTree(host, {
      home, stagedHome, profilesDir,
      manifest, profileName, mode: updating ? 'update' : 'install',
      archiveSha256: opts.archiveSha256, sourceIdentity: opts.sourceIdentity, beforePublish: opts.beforePublish,
      dryRun: !!opts.dryRun, recoverInterrupted: opts.recoverInterrupted === true, resolutions: opts.resolutions, previewToken: opts.previewToken, previous,
      validateProfiles: async (dirs) => {
        for (const dir of dirs) {
          const name = host.basename(dir);
          await handlers.verify(dir, whole ? manifest.profiles[name] : manifest, host.joinPath(stagedHome, profilesDir, name));
        }
      },
      finalize: opts.noInstall ? undefined : (affected) => handlers.finalize({
        ...affected, stagedProfiles: host.joinPath(stagedHome, profilesDir),
      }),
      preservePaths: plan.preservePaths.map(mapProfilePath),
      directories: plan.directories.map(mapProfilePath),
      executablePaths: (plan.executablePaths ?? []).map(mapProfilePath),
      verifyMcp: () => plan.verifyMcp({allowUnresolved:opts.noInstall === true,finalInstallAnchor:opts.finalInstallAnchor}),
      commitCredentials: Object.values(opts.credentials || {}).some(Boolean) ? plan.commitCredentials : undefined,
    });
    return {
      ...result, ...changes, manifest, dir: target, profileName, exists,
      installed: !opts.dryRun && !opts.noInstall, dryRun: !!opts.dryRun,
      items: plan.items, updated: updating && !opts.dryRun,
    };
  } finally {
    await host.rm(scratch, { recursive: true, force: true });
  }
}
