import { configBaseline, mergeConfig } from './update-config.js';
import { isExcluded } from './security.js';
import { isExactSemver } from './manifest.js';

const statePath = '.dsh-pack/updates';
const encoder = new TextEncoder();
const sha = /^[a-f0-9]{64}$/;
const own = (object, key) => Object.hasOwn(object, key);

function segment(name) {
  if (typeof name !== 'string' || !name || /[\\/:*?"<>|\x00-\x1f]|[. ]$/.test(name) ||
      /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/i.test(name)) {
    throw new Error('更新路径包含非法名称');
  }
  return name;
}

function relative(path) {
  if (typeof path !== 'string') {
    throw new Error('更新路径必须为相对路径');
  }
  for (const name of path.split('/')) {
    segment(name);
  }
  return path;
}

function managed(path, profilesDir) {
  const lower = path.toLowerCase();
  const parts = lower.split('/');
  if (parts.includes('.git') || parts.includes('node_modules') || parts.includes('attachments')) {
    return false;
  }
  if (parts.includes('.dsh-pack') && !(parts[0] === '.dsh-pack' && ['skills', 'agent-presets'].includes(parts[1]))) {
    return false;
  }
  if (parts[0] === profilesDir.toLowerCase() && ['vendor-blobs', '.dspack-sources'].includes(parts[2])) {
    return parts.length < 4 || lower.endsWith('.tgz');
  }
  const slot = parts[0] === '.dsh-pack';
  const offset = slot || parts[0] === profilesDir.toLowerCase() && parts.length >= 3 ? 2 : 0;
  // 源文件扫描已约束导出范围；安装生成的配置、锁和下载资源也应参与更新。
  const name = parts.at(-1);
  const check = parts.slice(offset).join('/').replace(/\.(?:tgz|zip|dspack|tar\.gz)$/, '.artifact');
  return !isExcluded(check) &&
    !parts.includes('.credentials.yaml') && name !== '.dshpkcfg';
}

function credentialPath(path) {
  return /^(?:\.credentials\.ya?ml|credentials\.ya?ml)$/.test(path);
}

async function typeAt(host, root, rel) {
  let dir = root;
  const parts = rel.split('/');
  for (let i = 0; i < parts.length; i++) {
    dir = host.joinPath(dir, segment(parts[i]));
    const type = await host.packEntryType(dir);
    if (!type) {
      return null;
    }
    if (type === 'symlink' || i < parts.length - 1 && type !== 'dir') {
      throw new Error('更新路径经过符号链接或非目录：' + rel);
    }
    if (i === parts.length - 1) {
      return type;
    }
  }
}

async function requiredText(host, path) {
  const value = await host.readTextFile(path);
  if (value === null) {
    throw new Error('无法读取更新文件：' + path);
  }
  return value;
}

async function fileState(host, root, path) {
  const type = await typeAt(host, root, path);
  if (!type) {
    return null;
  }
  if (type !== 'file') {
    throw new Error('更新需要普通文件，已有路径类型不符：' + path);
  }
  const hash = await host.sha256File(host.joinPath(root, path));
  if (!sha.test(hash)) {
    throw new Error('无法计算文件摘要：' + path);
  }
  return hash;
}

function skillFile(path) {
  return path.startsWith('skills/') || path.startsWith('.dsh-pack/skills/');
}

async function stageFiles(host, root, profilesDir, executablePaths = []) {
  const result = Object.create(null);
  const executable = new Set(executablePaths);
  async function walk(dir, prefix) {
    const entries = await host.readdir(dir);
    if (!entries) {
      throw new Error('无法读取安装暂存目录：' + dir);
    }
    const names = new Set();
    for (const { name, type } of entries) {
      const path = prefix + segment(name);
      if (names.has(name.toLowerCase())) {
        throw new Error('暂存文件存在大小写冲突：' + path);
      }
      names.add(name.toLowerCase());
      if (path !== '.dsh-pack' && !managed(path, profilesDir)) {
        continue;
      }
      const absolute = host.joinPath(dir, name);
      if (type === 'dir') {
        await walk(absolute, path + '/');
      } else if (type === 'file') {
        const hash = await host.sha256File(absolute);
        let tree;
        if (/\.(?:json|ya?ml)$/i.test(name) && name !== 'pnpm-lock.yaml') {
          const size = (await host.stat(absolute))?.size;
          if (size <= 4 * 1024 * 1024) {
            tree = await configBaseline(host, path, await requiredText(host, absolute));
          }
        }
        result[path] = tree ? { hash, tree } : { hash };
        if (skillFile(path)) {
          const mode = await host.packFileMode(absolute);
          result[path].executable = executable.has(path) || (mode !== null && Boolean(mode & 0o111));
        }
      } else {
        throw new Error('暂存目录包含符号链接或特殊文件：' + path);
      }
    }
  }
  await walk(root, '');
  return result;
}

async function readState(host, home, key, profilesDir) {
  const rel = statePath + '/baselines/' + key + '.json';
  const type = await typeAt(host, home, rel);
  if (!type) {
    return null;
  }
  const state = JSON.parse(await requiredText(host, host.joinPath(home, rel)));
  if (state.schema !== 1 || !state.files || typeof state.files !== 'object' || Array.isArray(state.files)) {
    throw new Error('整合包更新基线格式无效');
  }
  if (state.archiveSha256 !== undefined && !sha.test(state.archiveSha256) ||
      state.sourceIdentity !== undefined && (typeof state.sourceIdentity !== 'string' || !state.sourceIdentity) ||
      state.packType !== undefined && !['profile', 'dshhome'].includes(state.packType) ||
      state.profileName !== undefined && segment(state.profileName) !== state.profileName) {
    throw new Error('整合包更新基线的来源或目标无效');
  }
  for (const [path, entry] of Object.entries(state.files)) {
    if (!managed(relative(path), segment(state.profilesDir || profilesDir)) || !sha.test(entry?.hash) ||
      (entry.executable !== undefined && typeof entry.executable !== 'boolean')) {
      throw new Error('整合包更新基线包含不受管的路径或无效摘要');
    }
  }
  if (state.directories != null && (typeof state.directories !== 'object' || Array.isArray(state.directories) ||
    Object.entries(state.directories).some(([path,owned]) => typeof owned !== 'boolean' ||
      !managed(relative(path), segment(state.profilesDir || profilesDir))))) {
    throw new Error('整合包更新基线包含无效的目录记录');
  }
  return state;
}

/** 已安装技能沿用其原始目录，使桌面端与宿主插件交替更新时保持同一份数据。 */
export async function installedSkillLocation(host, home, profilesDir, profileName) {
  const unit = segment(profilesDir) + '/' + segment(profileName);
  const key = await host.sha256(encoder.encode('profile/' + unit.toLowerCase()));
  const state = await readState(host, home, key, profilesDir);
  const slot = '.dsh-pack/skills/' + profileName;
  const files = Object.keys(state?.files || {});
  if (files.some(file => file.startsWith(slot + '/'))) return slot;
  if (files.some(file => file.startsWith('skills/'))) return 'skills';
  if (await typeAt(host, home, slot) === 'dir') return slot;
  return null;
}

async function sharedPaths(host, home, key, profilesDir) {
  const owners = new Set();
  const rel = statePath + '/baselines';
  if (!await typeAt(host, home, rel)) {
    return owners;
  }
  const entries = await host.readdir(host.joinPath(home, rel));
  if (!entries) {
    throw new Error('无法读取共享文件的安装记录');
  }
  for (const entry of entries) {
    if (!/^[0-9a-f]{64}\.json$/.test(entry.name) || entry.name === key + '.json') {
      continue;
    }
    const state = await readState(host, home, entry.name.slice(0, -5), profilesDir);
    for (const path of Object.keys(state.files)) {
      owners.add(path.toLowerCase());
    }
    for (const [path,created] of Object.entries(state.directories ?? {})) {
      owners.add(path.toLowerCase());
      if (created) owners.add('directory:' + path.toLowerCase());
    }
  }
  return owners;
}

function fields(manifest, profileName, profilesDir) {
  const type = manifest.type ?? 'profile';
  const names = type === 'profile' ? [segment(profileName || manifest.profileName || manifest.name)] :
    Object.keys(manifest.profiles ?? {}).map(segment);
  return { type, units: names.map(name => profilesDir + '/' + name) };
}

async function baselineKey(host, type, units) {
  return host.sha256(encoder.encode(type === 'profile' ? 'profile/' + units[0].toLowerCase() : 'dshhome'));
}

/** 扩展配置在首次接管已有文件时需要保留原有配置项。 */
export async function packBaselineFiles(host, home, manifest, profileName, profilesDir) {
  const {type,units} = fields(manifest,profileName,profilesDir);
  return (await readState(host,home,await baselineKey(host,type,units),profilesDir))?.files;
}

// 同卷上别名的大小写由目录项决定。区分大小写的卷可以同时存放两个同名异形文件。
async function alignCase(host, opts, base, oldDirs, derived, next) {
  const desired = new Map();
  const spellings = paths => {
    const result = new Set();
    for (const path of paths) {
      let prefix = '';
      for (const part of path.split('/')) {
        prefix = prefix ? prefix + '/' + part : part;
        result.add(prefix);
      }
    }
    return result;
  };
  for (const path of spellings([...Object.keys(next), ...(opts.directories ?? [])])) {
    desired.set(path.toLowerCase(), path);
  }
  const aliases = new Map(), renames = [];
  const kept = (opts.preservePaths ?? []).map(path => relative(path).toLowerCase());
  for (const path of spellings([...Object.keys(base), ...Object.keys(oldDirs), ...Object.keys(derived)])) {
    const lower = path.toLowerCase(), target = desired.get(lower);
    if (!target || target === path || kept.some(k => lower === k || lower.startsWith(k+'/'))) continue;
    const parts = path.split('/'), to = target.split('/');
    const name = parts.pop(), renamed = to.pop();
    const parent = parts.join('/'), destParent = to.join('/');
    if (parent !== destParent && aliases.get(parent) !== destParent) continue;
    if (name !== renamed) {
      const dir = host.joinPath(opts.home, parent);
      const entries = (await host.readdir(dir) ?? []).filter(entry => entry.name.toLowerCase() === name.toLowerCase());
      if (entries.length > 1) continue;
      if (entries.length === 1) {
        if (!await typeAt(host, opts.home, path) || !await typeAt(host, opts.home, target)) continue;
        if (entries[0].name === name) {
          const from = [...to, name].join('/');
          renames.push({kind:'case', path:target, from});
        }
      }
    }
    aliases.set(path, target);
  }
  const align = record => Object.fromEntries(Object.entries(record).map(([path, value]) => [aliases.get(path) ?? path, value]));
  return {base:align(base), oldDirs:align(oldDirs), derived:align(derived), renames};
}

async function moveCase(host, home, from, to, restoring = false) {
  const parts = from.split('/'), name = parts.pop(), target = to.split('/').at(-1);
  const entries = await host.readdir(host.joinPath(home, ...parts));
  const found = entries?.filter(e => e.name.toLowerCase() === name.toLowerCase()) ?? [];
  if (found.length !== 1) throw new Error('大小写重命名目标已发生变化：' + from);
  if (restoring && found[0].name === target) return;
  if (found[0].name !== name || found[0].type === 'symlink') throw new Error('大小写重命名源已发生变化：' + from);
  await host.move(host.joinPath(home, from), host.joinPath(home, to));
}

async function compare(host, opts, key, next, otherOwners, base, derived, oldDirectories, profilesDir, renames) {
  const changes = renames.map(({path, from}) => ({path, from, action:'rename', conflict:false}));
  const conflicts = [];
  const writes = [];
  const checks = renames.map(({from,path}) => ['rename',from,path]);
  const unmanaged = new Set();
  const preserved = new Set();
  const requested = opts.preservePaths ?? [];
  if (!Array.isArray(requested)) {
    throw new Error('保留路径必须是相对路径列表');
  }
  const prefixes = requested.map(path => relative(path).toLowerCase());
  const choices = opts.resolutions ?? {};
  for (const path of [...new Set([...Object.keys(base), ...Object.keys(next)])].sort()) {
    const old = own(base, path) ? base[path] : null;
    const incoming = own(next, path) ? next[path] : null;
    if (prefixes.some(prefix => path.toLowerCase() === prefix || path.toLowerCase().startsWith(prefix + '/'))) {
      checks.push([path, 'preserve', old?.hash ?? null]);
      changes.push({ path, action: 'keep', preserved: true, conflict: false });
      preserved.add(path);
      continue;
    }
    const current = await fileState(host, opts.home, path);
    const before = old?.hash ?? null;
    const after = incoming?.hash ?? null;
    const choice = own(choices, path) ? choices[path] : null;
    checks.push([path, before, current, after]);
    const trackMode = skillFile(path);
    const mode = trackMode && current !== null ? await host.packFileMode(host.joinPath(opts.home, path)) : null;
    const localExec = mode === null ? null : Boolean(mode & 0o111);
    const oldExec = Boolean(old?.executable), nextExec = Boolean(incoming?.executable);
    const desiredExec = localExec === null || !old || oldExec !== nextExec ? nextExec : localExec;
    if (trackMode) checks.push([path,'executable',old?.executable,localExec,incoming?.executable]);
    let merged;
    let conflict = false;
    let action;
    const shared = otherOwners.has(path.toLowerCase());
    const generated = current && own(derived, path) && derived[path] === current;
    if (!old && current && !shared && !generated && choice !== 'upstream') {
      unmanaged.add(path);
    }
    if (current === after) {
      action = 'unchanged';
    } else if (after === before || shared && !after) {
      action = 'keep';
    } else if ((current === before || generated) && !shared) {
      action = after ? (current ? 'replace' : 'add') : 'remove';
    } else {
      if (before && current && after && old.tree && !shared) {
        merged = await mergeConfig(host, path, old.tree,
          await requiredText(host, host.joinPath(opts.home, path)),
          await requiredText(host, host.joinPath(opts.stagedHome, path)), choice, choices);
      }
      conflict = !merged || merged.conflicts > 0;
      if (conflict) {
        conflicts.push({ path, base: before, local: current, upstream: after, shared, members: merged?.conflicts ?? 1, fields: merged?.fields });
      }
      action = merged ? 'merge' : choice === 'upstream' ? (after ? 'replace' : 'remove') : 'keep';
    }
    let exec = desiredExec;
    const modeConflict = localExec !== null && ((!incoming && old && oldExec !== localExec && !shared) ||
      (incoming && (!old || shared && oldExec !== nextExec) && localExec !== nextExec));
    if (modeConflict) {
      if (!conflict) conflicts.push({path, base:before, local:current, upstream:after, members:1});
      conflict = true;
      if (choice !== 'upstream') {
        exec = localExec;
        action = 'keep';
      } else {
        action = incoming ? 'replace' : 'remove';
      }
    }
    const changeMode = incoming && localExec !== null && exec !== localExec && !unmanaged.has(path);
    if (changeMode && (action === 'keep' || action === 'unchanged')) action = 'chmod';
    if (action !== 'unchanged' || conflict) {
      changes.push({ path, action, conflict });
    }
    if (['add', 'replace', 'remove', 'merge', 'chmod'].includes(action)) {
      const data = merged ? encoder.encode(merged.text) : null;
      const expected = action === 'chmod' ? current : data ? await host.sha256(data) : after;
      if (current !== expected || changeMode) {
        writes.push({ path, current, expected, data, localCopy:action === 'chmod',
          ...(trackMode ? {mode, executable:exec} : {}) });
      }
    }
  }
  const directories = Object.create(null);
  const directoryOps = [];
  if (!Array.isArray(opts.directories ?? [])) throw new Error('空目录声明必须为列表');
  const selected = new Set(opts.directories ?? []);
  for (const path of [...new Set([...Object.keys(oldDirectories), ...selected])].sort()) {
    const lower = relative(path).toLowerCase();
    const incoming = selected.has(path);
    if (!managed(path, profilesDir) || (incoming && await typeAt(host, opts.stagedHome, path) !== 'dir')) {
      throw new Error('空目录声明与暂存内容不符：' + path);
    }
    const had = own(oldDirectories, path);
    const current = await typeAt(host, opts.home, path);
    const created = had ? oldDirectories[path] : otherOwners.has('directory:' + lower);
    const skip = prefixes.some(prefix => lower === prefix || lower.startsWith(prefix + '/'));
    if (skip) {
      if (had) directories[path] = created;
      checks.push([path, 'directory', 'preserve', current]);
      continue;
    }
    let action = 'keep';
    let conflict = false;
    let local = current;
    if (incoming) {
      directories[path] = created;
      if (!current && !had) {
        action = 'add';
        directories[path] = true;
        directoryOps.push({path, kind:'directory', existed:false});
      } else if (current && current !== 'dir') {
        if (current !== 'file') throw new Error('空目录目标已被特殊文件占用：' + path);
        local = await fileState(host, opts.home, path);
        const removed = writes.some(w => w.path === path && w.expected === null);
        conflict = !removed;
        if (conflict) conflicts.push({path, base:had ? 'directory' : null, local, upstream:'directory', members:1});
        if (removed || choices[path] === 'upstream') {
          action = 'replace';
          directoryOps.push({path, kind:'directory', existed:false, ...(removed ? {} : {replaces:local})});
          directories[path] = true;
        }
      } else if (current === 'dir') {
        action = 'unchanged';
      }
    } else if (created && current === 'dir' && !otherOwners.has(lower)) {
      const children = await host.readdir(host.joinPath(opts.home, path));
      if (!children) throw new Error('无法读取待移除的目录：' + path);
      local = children.length ? 'nonempty' : 'empty';
      const prefix = lower + '/';
      if (!children.length && ![...selected,...Object.keys(next)].some(p => p.toLowerCase().startsWith(prefix))) {
        action = 'remove';
        directoryOps.push({path, kind:'directory', existed:true});
      }
    }
    checks.push([path, 'directory', had, created, local, incoming]);
    if (action !== 'unchanged') changes.push({path, action, conflict, type:'directory'});
  }
  const token = await host.sha256(encoder.encode(JSON.stringify([key, opts.archiveSha256, opts.sourceIdentity, opts.manifest.version, checks, [...otherOwners].sort()])));
  return { changes, conflicts, writes, token, unmanaged, preserved, directories, directoryOps, renames };
}

async function restore(host, home, dir, journal) {
  for (const [index, item] of [...journal.operations.entries()].reverse()) {
    const path = relative(item.path);
    if (!['file', 'modules', 'credential', 'directory', 'case'].includes(item.kind) ||
      item.kind === 'case' && (!managed(path, journal.profilesDir) || relative(item.from).toLowerCase() !== path.toLowerCase() ||
        item.from.split('/').slice(0,-1).join('/') !== path.split('/').slice(0,-1).join('/')) ||
      item.kind === 'modules' && (path.split('/').length !== 3 || !path.endsWith('/node_modules')) ||
      item.kind === 'credential' && !credentialPath(path) ||
      (item.kind === 'file' || item.kind === 'directory') && !managed(path, journal.profilesDir)) {
      throw new Error('更新恢复记录含非法目标');
    }
    if (journal.tracked && !await host.packEntryType(host.joinPath(dir, 'begun', String(index)))) {
      continue;
    }
    const dest = host.joinPath(home, path);
    const copy = host.joinPath(dir, 'old', String(index));
    if (item.kind === 'case') {
      await moveCase(host, home, path, item.from, true);
    } else if (item.kind === 'directory') {
      const current = await typeAt(host, home, path);
      if (item.existed) {
        if (!current) await host.mkdir(dest);
      } else {
        if (current === 'dir') await host.removePackDirIfEmpty(dest);
        if (item.replaces) {
          const remaining = await typeAt(host, home, path);
          if (!remaining) await host.copyPackFile(copy, dest);
          else if (remaining !== 'file' || await fileState(host, home, path) !== item.replaces) {
            throw new Error('目录目标已被本机修改，原文件仍保存在恢复记录中：' + path);
          }
        }
      }
    } else if (item.kind === 'modules') {
      if (item.existed && !await host.packEntryType(copy)) {
        continue;
      }
      if (await typeAt(host, home, path)) {
        await host.rm(dest, { recursive: true, force: true });
      }
      if (item.existed) {
        await host.move(copy, dest);
      }
    } else if (item.existed) {
      if (await typeAt(host, home, path) === 'dir') {
        throw new Error('无法还原已变为目录的文件：' + path);
      }
      await host.copyPackFile(copy, dest, item.kind === 'credential');
    } else if (await typeAt(host, home, path)) {
      await host.rm(dest, { recursive: false, force: true });
    }
  }
}

async function removeCreatedDirs(host, home, directories) {
  for (const dir of [...directories].sort((a,b) => b.length - a.length)) {
    if (await typeAt(host, home, relative(dir)) === 'dir') {
      await host.removePackDirIfEmpty(host.joinPath(home, dir));
    }
  }
}

async function recover(host, home, profilesDir, inspectOnly) {
  const txn = statePath + '/transaction';
  if (!await typeAt(host, home, txn)) {
    return;
  }
  if (inspectOnly) {
    throw new Error('上次更新尚未结束，请先恢复更新事务');
  }
  const dir = host.joinPath(home, txn);
  if (await typeAt(host, home, txn + '/journal.json')) {
    const journal = JSON.parse(await requiredText(host, host.joinPath(dir, 'journal.json')));
    if (!sha.test(journal.key) || !journal.id || !Array.isArray(journal.operations)) {
      throw new Error('更新恢复记录损坏');
    }
    segment(journal.profilesDir);
    if ((await readState(host, home, journal.key, profilesDir))?.transaction !== journal.id) {
      await restore(host, home, dir, journal);
      await removeCreatedDirs(host, home, journal.directories ?? []);
    }
    await host.rm(host.joinPath(dir, 'journal.json'), { recursive: false, force: true });
  }
  await host.rm(dir, { recursive: true, force: true });
}

/** 在持有同一安装目录锁时恢复被中断的依赖、配置文件和安装记录。 */
export async function recoverPackUpdate(host, home, profilesDir = 'profiles') {
  if (!await typeAt(host, home, statePath + '/transaction')) return;
  await typeAt(host, home, statePath + '/locks');
  return host.withPackLock(host.joinPath(home, statePath), () => recover(host, home, segment(profilesDir), false));
}

async function dependenciesPresent(host, home, unit) {
  const dir = host.joinPath(home, unit);
  const raw = await host.readTextFile(host.joinPath(dir, 'package.json'));
  if (raw === null) return false;
  let pkg;
  try { pkg = JSON.parse(raw); } catch { return false; }
  if (!pkg || typeof pkg !== 'object' || Array.isArray(pkg)) return false;
  const required = Object.entries(pkg.dependencies ?? {}).filter(([name]) => !own(pkg.optionalDependencies ?? {}, name));
  if (!required.length) return true;
  if (await typeAt(host, home, unit + '/node_modules') !== 'dir') return false;
  for (const [name, spec] of required) {
    if (!/^(?:@[a-z0-9._-]+\/)?[a-z0-9][a-z0-9._-]*$/i.test(name)) throw new Error('无效的依赖名称：' + name);
    const file = await host.readTextFile(host.joinPath(dir, 'node_modules', ...name.split('/'), 'package.json'));
    if (file === null) return false;
    try {
      const installed = JSON.parse(file);
      if (!installed || typeof installed !== 'object' || (isExactSemver(spec) && installed.version !== spec.trim())) return false;
    } catch {
      return false;
    }
  }
  return true;
}

async function planDependencies(host, opts, plan, units, state) {
  const ready = new Set(state?.dependenciesReady ?? []), missing = [];
  for (const unit of units) {
    if (ready.has(unit) && !await dependenciesPresent(host, opts.home, unit)) {
      ready.delete(unit);
      missing.push(unit);
    }
    if (plan.writes.some(w => w.path.startsWith(unit + '/') &&
        /^(?:package\.json|pnpm-(lock|workspace)\.yaml|(?:vendor-blobs|\.dspack-sources)\/.+\.tgz)$/.test(w.path.slice(unit.length + 1)))) {
      ready.delete(unit);
    }
  }
  const deps = opts.finalize ? units.filter(unit => !ready.has(unit)) : [];
  for (const unit of deps) plan.changes.push({path:unit + '/node_modules', action:'install', conflict:false});
  if (missing.length) plan.token = await host.sha256(encoder.encode(JSON.stringify([plan.token,missing])));
  return {ready, deps};
}

async function commit(host, opts, key, next, plan, units, profilesDir, {base, derived}, {ready, deps}) {
  const home = opts.home;
  const dir = host.joinPath(home, statePath, 'transaction');
  const id = globalThis.crypto.randomUUID();
  const operations = [...plan.renames, ...plan.writes.map(w => ({ path: w.path, existed: w.current !== null, kind: 'file' }))];
  operations.push(...plan.directoryOps);
  for (const unit of deps) {
    const modules = unit + '/node_modules';
    const type = await typeAt(host, home, modules);
    if (type && type !== 'dir') {
      throw new Error('依赖目录不是普通目录：' + modules);
    }
    operations.push({ path: modules, existed: !!type, kind: 'modules' });
    const lock = unit + '/pnpm-lock.yaml';
    if (!operations.some(op => op.path === lock)) {
      operations.push({ path: lock, existed: !!await typeAt(host, home, lock), kind: 'file' });
    }
  }
  if (opts.commitCredentials) {
    for (const unit of units) {
      const patch = unit + '/cordis.patch.yml';
      if (!operations.some(op => op.path === patch)) {
        operations.push({ path: patch, existed: !!await typeAt(host, home, patch), kind: 'file' });
      }
    }
    for (const path of new Set(opts.credentialPaths ?? ['.credentials.yaml'])) {
      if (!credentialPath(path)) {
        throw new Error('凭据事务路径无效');
      }
      operations.push({ path, existed: !!await typeAt(host, home, path), kind: 'credential' });
    }
  }
  const directories = new Set();
  for (const {path} of operations) {
    const parts = path.split('/');
    for (let i = 1; i < parts.length; i++) {
      const dir = parts.slice(0, i).join('/');
      if (!directories.has(dir) && !await typeAt(host, home, dir)) {
        directories.add(dir);
      }
    }
  }
  const journal = { id, key, profilesDir, operations, directories: [...directories], tracked: true };
  const mark = index => host.writeTextFile(host.joinPath(dir, 'begun', String(index)), '');
  await host.mkdir(dir);
  try {
    for (const [index, item] of operations.entries()) {
      if (item.replaces || (item.existed && item.kind !== 'modules' && item.kind !== 'directory')) {
        await host.copyPackFile(host.joinPath(home, item.path), host.joinPath(dir, 'old', String(index)), item.kind === 'credential');
      }
    }
    await host.writeTextFile(host.joinPath(dir, 'journal.tmp'), JSON.stringify(journal));
    await host.move(host.joinPath(dir, 'journal.tmp'), host.joinPath(dir, 'journal.json'));
    for (const [index, item] of operations.entries()) {
      if (item.kind === 'case') {
        await mark(index);
        await moveCase(host, home, item.from, item.path);
      } else if (item.kind === 'modules' && item.existed) {
        await mark(index);
        await host.move(host.joinPath(home, item.path), host.joinPath(dir, 'old', String(index)));
      }
    }
    for (const [offset, write] of plan.writes.entries()) {
      const index = plan.renames.length + offset;
      if (await fileState(host, home, write.path) !== write.current ||
        (write.mode != null && await host.packFileMode(host.joinPath(home, write.path)) !== write.mode)) {
        throw new Error('文件在预览后发生变化：' + write.path);
      }
      const dest = host.joinPath(home, write.path);
      if (write.expected === null) {
        await mark(index);
        await host.rm(dest, { recursive: false, force: true });
      } else {
        const temp = host.joinPath(dir, 'new', String(index));
        if (write.data) {
          await host.writeFile(temp, write.data);
        } else {
          await host.copyPackFile(host.joinPath(write.localCopy ? home : opts.stagedHome, write.path), temp);
        }
        if (write.executable !== undefined) {
          const sourceMode = write.mode ?? await host.packFileMode(temp);
          if (sourceMode !== null) {
            const bits = write.executable ? (sourceMode & 0o111) || (0o100 | ((sourceMode & 0o044) >> 2)) : 0;
            await host.chmodPackFile(temp, (sourceMode & 0o666) | bits);
          }
        }
        if (await host.sha256File(temp) !== write.expected) {
          throw new Error('暂存内容在预览后发生变化：' + write.path);
        }
        await mark(index);
        await host.move(temp, dest);
      }
    }
    for (const [index,item] of operations.entries()) {
      if (item.kind !== 'directory') continue;
      const dest = host.joinPath(home, item.path);
      const current = await typeAt(host, home, item.path);
      if (item.existed) {
        if (current !== 'dir') throw new Error('目录在预览后发生变化：' + item.path);
        await mark(index);
        await host.removePackDirIfEmpty(dest);
      } else {
        if (item.replaces ? await fileState(host, home, item.path) !== item.replaces : !!current) {
          throw new Error('目录目标在预览后发生变化：' + item.path);
        }
        await mark(index);
        if (item.replaces) await host.rm(dest, {recursive:false, force:false});
        await host.mkdir(dest);
      }
    }
    if (opts.validateProfiles) await opts.validateProfiles(units.map(unit => host.joinPath(home, unit)));
    if (deps.length) {
      for (const [index,item] of operations.entries()) {
        if (item.kind === 'modules' || deps.some(unit => item.path === unit + '/pnpm-lock.yaml')) {
          await mark(index);
        }
      }
      await opts.finalize({ affectedProfiles: deps.map(unit => host.joinPath(home, unit)) });
      for (const unit of deps) {
        ready.add(unit);
      }
    }
    if (opts.verifyMcp) await opts.verifyMcp();
    if (opts.commitCredentials) {
      for (const [index,item] of operations.entries()) {
        if (item.kind === 'credential' || units.some(unit => item.path === unit + '/cordis.patch.yml')) {
          await mark(index);
        }
      }
      await opts.commitCredentials();
    }
    const files = Object.assign(Object.create(null), Object.fromEntries(Object.entries(next).filter(([path]) => !plan.unmanaged.has(path) && !plan.preserved.has(path))));
    for (const path of plan.preserved) {
      if (own(base, path)) {
        files[path] = base[path];
      }
    }
    const derivedLocks = { ...derived };
    for (const unit of deps) {
      const lock = unit + '/pnpm-lock.yaml';
      const hash = await fileState(host, home, lock);
      const operation = operations.find(item => item.path === lock);
      const owned = own(files, lock) || own(base, lock) || own(derivedLocks, lock) || !operation?.existed;
      if (hash && owned && !plan.unmanaged.has(lock)) {
        derivedLocks[lock] = hash;
      } else {
        delete derivedLocks[lock];
      }
    }
    const { type, units: names } = fields(opts.manifest, opts.profileName, profilesDir);
    const state = { schema: 1, profilesDir, name: opts.manifest.name, version: opts.manifest.version,
      archiveSha256: opts.archiveSha256, sourceIdentity: opts.sourceIdentity, packType: type,
      profileName: type === 'profile' ? names[0].slice(profilesDir.length + 1) : undefined,
      transaction: id, files, directories: plan.directories, derivedLocks, dependenciesReady: units.filter(unit => ready.has(unit)) };
    await host.writeTextFile(host.joinPath(dir, 'baseline.tmp'), JSON.stringify(state));
    const baseline = host.joinPath(home, statePath, 'baselines', key + '.json');
    if (opts.beforePublish) await opts.beforePublish({ transaction: id, baseline });
    await host.move(host.joinPath(dir, 'baseline.tmp'), baseline);
  } catch (error) {
    try {
      await recover(host, home, profilesDir, false);
    } catch (recovery) {
      throw new AggregateError([error, recovery], '更新失败，恢复未完成：' + recovery.message);
    }
    throw error;
  }
  await recover(host, home, profilesDir, false);
}

/** 将暂存 HOME 的受管文件与当前安装和上次发布内容三方比较，预览或以事务应用所选变更。 */
export async function syncPackTree(host, opts) {
  const { home, stagedHome, manifest, previous } = opts;
  const profilesDir = segment(opts.profilesDir || 'profiles');
  if (host.resolvePath(home) === host.resolvePath(stagedHome) || !['install', 'update'].includes(opts.mode)) {
    throw new Error('更新需要独立的暂存目录和明确的安装模式');
  }
  const { type, units } = fields(manifest, opts.profileName, profilesDir);
  const key = await baselineKey(host, type, units);
  const next = await stageFiles(host, stagedHome, profilesDir, opts.executablePaths);
  await typeAt(host, home, statePath + '/locks');
  async function run() {
    await recover(host, home, profilesDir, opts.dryRun);
    const state = await readState(host, home, key, profilesDir);
    if (state && opts.mode === 'install' && !opts.dryRun) {
      throw new Error('目标已安装整合包，请重新预览并选择更新');
    }
    if (state && state.name !== manifest.name) {
      throw new Error('目标已绑定其他整合包：' + state.name);
    }
    if (state?.sourceIdentity && state.sourceIdentity !== opts.sourceIdentity || state?.packType && state.packType !== type ||
        state?.profileName && state.profileName !== opts.profileName) {
      throw new Error('更新包的来源或配置档与已安装记录不符');
    }
    const boundState = state?.sourceIdentity || !opts.sourceIdentity ? state : null;
    let base = boundState?.files;
    if (!base && previous) {
      if (!sha.test(previous.sha256) || previous.sha256 !== previous.expectedSha256 ||
        previous.manifest?.name !== manifest.name || (previous.manifest.type ?? 'profile') !== type) {
        throw new Error('旧包的身份或 SHA-256 校验不匹配');
      }
      base = await stageFiles(host, previous.stagedHome, profilesDir);
    }
    const owners = await sharedPaths(host, home, key, profilesDir);
    const aligned = await alignCase(host, opts, base ?? {}, boundState?.directories ?? {}, boundState?.derivedLocks ?? {}, next);
    const plan = await compare(host, opts, key, next, owners, aligned.base, aligned.derived, aligned.oldDirs, profilesDir, aligned.renames);
    const dependencies = await planDependencies(host, opts, plan, units, boundState);
    const choices = opts.resolutions ?? {};
    if (opts.previewToken && opts.previewToken !== plan.token) {
      throw new Error('安装内容或本地文件已变化，请重新预览');
    }
    for (const [path, choice] of Object.entries(choices)) {
      if (choice === 'upstream' && !opts.previewToken || !plan.conflicts.some(c => c.path === path || c.fields?.some(f => f.key === path)) || !['local', 'upstream'].includes(choice)) {
        throw new Error('冲突选择无效或缺少对应的预览');
      }
    }
    const result = {
      applied: false, changes: plan.changes, conflicts: plan.conflicts, previewToken: plan.token,
      previousVersion: state?.version ?? previous?.manifest?.version ?? null, version: manifest.version,
      unknownBase: !base && opts.mode === 'update',
    };
    if (opts.dryRun) {
      return result;
    }
    await commit(host, opts, key, next, plan, units, profilesDir, aligned, dependencies);
    return { ...result, applied: true };
  }
  if (opts.dryRun) {
    if (opts.recoverInterrupted) await recoverPackUpdate(host, home, profilesDir);
    return run();
  }
  const homeExisted = await host.packEntryType(home);
  const controlDirs = [statePath + '/locks', statePath, '.dsh-pack'];
  const created = [];
  for (const rel of controlDirs) {
    if (!await typeAt(host, home, rel)) {
      created.push(rel);
    }
  }
  try {
    return await host.withPackLock(host.joinPath(home, statePath), run);
  } catch (error) {
    await removeCreatedDirs(host, home, created);
    if (!homeExisted) {
      await host.removePackDirIfEmpty(home);
    }
    throw error;
  }
}
