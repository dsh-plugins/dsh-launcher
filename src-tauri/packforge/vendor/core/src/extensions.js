import { discoverSkills, exportSkill, skillRoots } from './extension-skills.js';
import { splitPatch } from './extension-patch.js';
import { expandTemplate, localized, protectedTarget, readJson, readTree, safePath, segment } from './extension-files.js';

export { planExtensionInstall } from './extension-install.js';
export { validateExtensions } from './extension-validate.js';

const utf8 = new TextEncoder();
const sidecars = {paths:'path.json', providers:'provider.json', mcp:'mcp.json', skills:'skill.json'};
const npmName = /^(?:@[a-z0-9._-]+\/)?[a-z0-9][a-z0-9._-]*$/i;

function scopeOf(profile) { return 'profile:' + segment(profile.name); }
function choiceMap(list = []) {
  if (!Array.isArray(list)) throw new Error('扩展资源选择需要数组');
  const selected = new Map();
  for (const value of list) {
    const choice = typeof value === 'string' ? {key:value} : value;
    if (typeof choice?.key !== 'string' || !choice.key || selected.has(choice.key)) throw new Error('已选资源的键缺失或重复');
    selected.set(choice.key,choice);
  }
  return selected;
}

async function installedPlugins(host, profile, readOrphanedPackages = false) {
  const pkg = await readJson(host, host.joinPath(profile.dir, 'package.json'));
  const names = new Set([...Object.keys(pkg?.dependencies || {}), ...(pkg?.dsh?.profile?.bundles || [])]);
  if (!pkg && readOrphanedPackages) {
    const modulesPath = host.joinPath(profile.dir, 'node_modules');
    const modulesType = await host.packEntryType(modulesPath);
    const modules = ['dir', 'symlink'].includes(modulesType) ? await host.readdir(modulesPath) : [];
    if (!modules) throw new Error('无法检查配置档中的插件排除声明：' + profile.name);
    for (const entry of modules) {
      if (!['dir', 'symlink'].includes(entry.type)) {
        continue;
      }
      if (!entry.name.startsWith('@')) {
        names.add(entry.name);
        continue;
      }
      const scoped = await host.readdir(entry.abs);
      if (!scoped) throw new Error('无法检查插件作用域中的排除声明：' + entry.name);
      for (const child of scoped) {
        if (['dir', 'symlink'].includes(child.type)) {
          names.add(entry.name + '/' + child.name);
        }
      }
    }
  }
  return [...names].filter(plugin => npmName.test(plugin));
}

function pluginDeclaration(host, profile, plugin, file) {
  return host.joinPath(profile.dir, 'node_modules', ...plugin.split('/'), 'dsh_pack', file);
}

/** 发现本机文件排除规则。整机规则来源独立于用户勾选的文件。 */
export async function discoverPluginExclusions(host, {home, profiles = []}) {
  const installedProfiles = new Map(profiles.map(profile => [profile.dir, profile]));
  if (home) {
    const profileRoot = host.joinPath(home, 'profiles');
    const entries = await host.packEntryType(profileRoot) === 'dir' ? await host.readdir(profileRoot) : [];
    if (!entries) throw new Error('无法检查所有配置档的插件排除声明');
    for (const entry of entries) {
      if (entry.type === 'dir' && !installedProfiles.has(entry.abs)) {
        installedProfiles.set(entry.abs, {name: entry.name, dir: entry.abs});
      }
    }
  }
  const exclusions = new Set();
  for (const profile of installedProfiles.values()) {
    for (const plugin of await installedPlugins(host, profile, true)) {
      const declaration = pluginDeclaration(host, profile, plugin, 'exclude.json');
      const type = await host.packEntryType(declaration);
      if (type == null) {
        continue;
      }
      const raw = type === 'file' ? await host.readTextFile(declaration) : null;
      if (raw == null) {
        throw new Error('无法读取插件排除声明：' + plugin);
      }
      let templates;
      try {
        templates = JSON.parse(raw);
      } catch {
        throw new Error('插件排除声明的 JSON 格式无效：' + plugin);
      }
      if (!Array.isArray(templates) || !templates.length) {
        throw new Error('插件排除声明须为非空路径数组：' + plugin);
      }
      for (const template of templates) {
        const path = expandTemplate(template, profile.name);
        if (protectedTarget(path) || /^profiles(?:\/[^/]+)?$/i.test(path) ||
            /^profiles\/[^/]+\/(?:skills|\.agent-presets)(?:\/|$)/i.test(path)) {
          throw new Error('插件排除声明占用了保留路径：' + path);
        }
        exclusions.add(path.toLowerCase());
      }
    }
  }
  return exclusions;
}

/** 同时匹配声明的文件、目录以及目录下所有内容。 */
export function matchesPluginExclusion(exclusions, path) {
  const relative = path.toLowerCase();
  for (const root of exclusions) {
    if (relative === root || relative.startsWith(root + '/')) {
      return true;
    }
  }
  return false;
}

async function declaredPaths(host, profiles, exclusions) {
  const resources = new Map();
  for (const profile of profiles) {
    for (const plugin of await installedPlugins(host, profile)) {
      const declaration = await readJson(host, pluginDeclaration(host, profile, plugin, 'paths.json'));
      if (!declaration) continue;
      const rules = Array.isArray(declaration) ? declaration : [declaration];
      for (const [index,rule] of rules.entries()) {
        if (!rule?.name || !Array.isArray(rule.paths) || !rule.paths.length) throw new Error('插件路径声明缺少 name 或 paths：' + plugin);
        const scope = rule.paths.some(p=>typeof p==='string' && p.includes('<profile>')) ? scopeOf(profile) : 'home';
        const id = String(index);
        const key = ['path',scope,plugin,id].join(':');
        const item = {key,id,plugin,scope,kind:'path',name:localized(rule.name),desc:localized(rule.desc,''),paths:rule.paths};
        const targets = item.paths.map(p => {
          const expanded = expandTemplate(p,profile.name);
          if (protectedTarget(expanded)) {
            throw new Error('插件声明占用了保留资源路径：' + expanded);
          }
          return expanded;
        });
        if (targets.every(path => matchesPluginExclusion(exclusions, path))) {
          continue;
        }
        if (resources.has(key) && JSON.stringify(resources.get(key)) !== JSON.stringify(item)) {
          throw new Error('同一插件对全局数据声明不一致：' + plugin);
        }
        resources.set(key,item);
      }
    }
  }
  return [...resources.values()];
}

async function inspect(host, {home,profiles,wholeHome=false,exclusions}) {
  const patches = new Map();
  const providers = [], mcp = [];
  for (const [scope,dir] of [...(home ? [['home',home]] : []), ...profiles.map(p=>[scopeOf(p),p.dir])]) {
    const raw = await host.readTextFile(host.joinPath(dir,'cordis.patch.yml'));
    const patch = splitPatch(raw,scope);
    patches.set(scope,patch.patch);
    for (const item of patch.items) (item.kind === 'provider' ? providers : mcp).push(item);
  }
  exclusions ??= await discoverPluginExclusions(host, {home, profiles});
  const paths = await declaredPaths(host,profiles,exclusions);
  const roots = await skillRoots(host,home,profiles,wholeHome);
  const skills = [];
  for (const [scope,root] of roots) skills.push(...await discoverSkills(host,root,scope));
  return {patches,roots,paths,providers,mcp,skills,exclusions};
}

export async function discoverExtensions(host, location) {
  const {patches,roots,exclusions,...result} = await inspect(host,location);
  for (const kind of ['providers','mcp']) result[kind] = result[kind].map(({value,...item})=>item);
  return result;
}

function ordinaryHomePath(path,manifest,profileName) {
  if (manifest.type === 'dshhome') return path.startsWith('overrides/') ? path.slice(10) : '';
  if (path.startsWith('home/')) return path.slice(5);
  if (path.startsWith('overrides/')) return 'profiles/' + profileName + '/' + path.slice(10);
  return '';
}

function stripPath(entries,manifest,profileName,rel) {
  const norm = rel.toLowerCase();
  for (const path of Object.keys(entries)) {
    const homePath = ordinaryHomePath(path,manifest,profileName).toLowerCase();
    if (homePath === norm || homePath.startsWith(norm+'/')) delete entries[path];
  }
}

function withLabel(item,selection) {
  return { ...item,name:localized(selection.name || item.name,item.id),desc:localized(selection.desc,item.desc) };
}

async function capturePath(host,home,profiles,entries,item,claimed,profileName,exclusions) {
  const files = [];
  const directories = [];
  const profile = item.scope.startsWith('profile:') ? item.scope.slice(8) : profileName;
  for (const [index,template] of item.paths.entries()) {
    const rel = expandTemplate(template,profile);
    if (matchesPluginExclusion(exclusions, rel)) {
      continue;
    }
    const resolved = profiles.find(p=>p.name === profile);
    const prefix = 'profiles/'+profile+'/';
    const target = resolved && rel.startsWith(prefix) ? await safePath(host,resolved.dir,rel.slice(prefix.length)) : await safePath(host,home,rel);
    const stat = await host.stat(target);
    if (!stat) continue;
    const emptyDirs = [];
    const tree = stat.isDirectory
      ? await readTree(host, target, {emptyDirs, exclude: tail => matchesPluginExclusion(exclusions, rel + '/' + tail)})
      : new Map([['',await host.readFile(target)]]);
    for (const tail of emptyDirs) {
      const path = rel + (tail ? '/'+tail : '');
      const key = path.toLowerCase();
      if (protectedTarget(path) || claimed.has(key)) throw new Error('插件目录目标受保护或重复：' + path);
      claimed.add(key);
      directories.push({path,index,tail});
    }
    for (const [tail,bytes] of tree) {
      const path = rel + (tail ? '/'+tail : '');
      if (protectedTarget(path) || !bytes) throw new Error('无法捕获插件数据：' + path);
      const key = path.toLowerCase();
      if (claimed.has(key)) throw new Error('多个插件数据规则声明了同一文件：' + path);
      claimed.add(key);
      const hash = await host.sha256(bytes);
      const entry = 'extension-data/path/' + hash;
      entries[entry] = bytes;
      files.push({path,index,tail,entry,sha256:hash});
    }
  }
  return { ...item, files, ...(directories.length ? {directories} : {}) };
}

export async function collectExtensions(host,{home,profiles,manifest,entries,selection = {},omitPatches = new Set(),exclusions}) {
  const all = await inspect(host,{home,profiles,wholeHome:manifest.type === 'dshhome',exclusions});
  const profileName = profiles[0]?.name || '';
  const single = manifest.type !== 'dshhome';
  if (single && profiles.length) {
    manifest.patch = all.patches.get('profile:' + profileName);
    if (omitPatches.has(profileName)) delete manifest.patch;
  } else if (!single) {
    for (const [name, unit] of Object.entries(manifest.profiles)) {
      if (omitPatches.has(name)) delete unit.patch;
      else unit.patch = all.patches.get('profile:' + name);
    }
  }
  // 每份随包发布的补丁都必须对应已清理的作用域，包括按白名单部分导出的整机快照。
  for (const path of Object.keys(entries)) {
    const rel = ordinaryHomePath(path, manifest, profileName);
    if (matchesPluginExclusion(all.exclusions, rel)) {
      delete entries[path];
      continue;
    }
    const match = /^(?:profiles\/([^/]+)\/)?cordis\.patch\.yml$/i.exec(rel);
    if (!match) continue;
    const scope = match[1] ? 'profile:' + match[1] : 'home';
    if (!all.patches.has(scope)) throw new Error('导出配置档补丁时须同时选择 profiles/' + match[1] + '/package.json：' + path);
    entries[path] = utf8.encode(all.patches.get(scope));
  }
  const selected = Object.fromEntries(Object.keys(sidecars).map(kind=>[kind,choiceMap(selection?.[kind])]));
  const output = {paths:{plugins:Object.create(null)},providers:{items:[]},mcp:{items:[]},skills:{items:[]}};
  const claimed = new Set();
  for (const candidate of all.paths) {
    for (const template of candidate.paths) stripPath(entries,manifest,profileName,
      expandTemplate(template,candidate.scope === 'home' ? profileName : candidate.scope.slice(8)));
    const choice = selected.paths.get(candidate.key);
    if (choice) {
      if (!home) throw new Error('导出插件声明路径需要指定 DSH_HOME（--home）');
      const item = await capturePath(host,home,profiles,entries,withLabel(candidate,choice),claimed,profileName,all.exclusions);
      (output.paths.plugins[item.plugin] ??= []).push(item);
      selected.paths.delete(candidate.key);
    }
  }
  for (const kind of ['providers','mcp']) {
    for (const candidate of all[kind]) {
      const choice = selected[kind].get(candidate.key);
      if (!choice) continue;
      if (candidate.unsupported) throw new Error('所选配置含无法安全移植的脚本、地址、凭据或 YAML 表达式（含别名）：'+candidate.id);
      const {unsupported,...safe} = candidate;
      output[kind].items.push(withLabel(safe,choice));
      selected[kind].delete(candidate.key);
    }
  }
  stripPath(entries,manifest,profileName,'skills');
  // .dsh-skill.json 中的历史仓库地址可能携带认证信息。
  for (const path of Object.keys(entries)) if (/(^|\/)\.dsh-skill\.json$/.test(path)) delete entries[path];
  if (manifest.skills) manifest.skills = manifest.skills.filter(s=>typeof s.path !== 'string' || !s.path.startsWith('skills/'));
  for (const [key,choice] of selected.skills) {
    let candidate = all.skills.find(s=>s.key === key);
    if (!candidate) {
      const match = /^skill:(home|profile:([^:]+)):([^:]+)$/.exec(key);
      if (!match || !choice.repository || (match[2] && (single || !profiles.some(p=>p.name === match[2])))) throw new Error('未找到已选技能：' + key);
      const id = segment(choice.id || match[3]);
      if (id !== match[3]) throw new Error('技能标识与键不一致');
      candidate = {key,kind:'skill',id,scope:match[1],name:id,desc:''};
    }
    if (!home && choice.mode === 'embedded') throw new Error('导出本地技能需要指定 DSH_HOME（--home）');
    const {files,...result} = await exportSkill(host,all.roots.get(candidate.scope),candidate,choice);
    const {source,sourceWarning,repository,subdir,commit,ref,...item} = withLabel(candidate,choice);
    const data = {...item,...result};
    if (files) {
      data.files = [];
      for (const [path,bytes] of files) {
        const hash = await host.sha256(bytes);
        const entry = 'extension-data/skill/' + hash;
        entries[entry] = bytes;
        data.files.push({path,entry,sha256:hash});
      }
    }
    output.skills.items.push(data);
  }
  for (const kind of ['paths','providers','mcp']) if (selected[kind].size) throw new Error('未找到已选扩展资源：'+[...selected[kind].keys()].join(', '));
  for (const [kind,path] of Object.entries(sidecars)) {
    const result = output[kind];
    if (Object.keys(result.plugins ?? result.items).length) entries[path] = utf8.encode(JSON.stringify(result,null,2)+'\n');
    else delete entries[path];
  }
  return output;
}
