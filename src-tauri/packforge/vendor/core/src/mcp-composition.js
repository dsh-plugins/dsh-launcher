import { parseDocument } from 'yaml';
import { safePath } from './extension-files.js';

const client = '@deepseek-ai/dsh-mcp-client';
const manager = '@deepseek-ai/dsh-mcp-manager';
const npmName = /^(?:@[a-z0-9._-]+\/)?[a-z0-9][a-z0-9._-]*$/i;
const serverName = /^[a-z0-9_-]{1,32}$/i;
const expression = '\u0000dsh:js';
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const dynamic = value => value === expression || (object(value) && Object.hasOwn(value,'__jsExpr'));

function parseLayer(raw) {
  if (!raw?.trim()) return [];
  const doc = parseDocument(raw, {customTags:[{tag:'tag:yaml.org,2002:js',resolve:()=>expression}]});
  if (doc.errors.length || doc.warnings.length) throw new Error('补丁无法静态解析');
  const patches = doc.toJS({maxAliasCount:64});
  if (!Array.isArray(patches) || !patches.every(object)) throw new Error('补丁不是配置行数组');
  return patches;
}

// Node 按锚点逐级查找 node_modules。宿主安装目录优先于 profile，pnpm 的包链接由 Host 读取。
async function bundleRoot(host, anchors, name) {
  const visited = new Set();
  for (let dir of anchors) {
    while (dir && !visited.has(dir)) {
      visited.add(dir);
      if (host.basename(dir) !== 'node_modules') {
        const root = host.joinPath(dir,'node_modules',...name.split('/'));
        const manifestPath = host.joinPath(root,'package.json');
        const json = await host.readTextFile(manifestPath);
        if (json != null) return {root,manifest:JSON.parse(json)};
        if (await host.stat(manifestPath)) throw new Error('bundle 清单不可读');
      }
      const parent = host.resolvePath(dir,'..');
      if (parent === dir) break;
      dir = parent;
    }
  }
  return null;
}

async function readBundle(host, anchors, name) {
  if (!npmName.test(name)) throw new Error('无效的 bundle 名称');
  const pkg = await bundleRoot(host,anchors,name);
  if (!pkg) return null;
  const declared = pkg?.manifest?.dsh?.bundle?.patch;
  const files = typeof declared === 'string' ? [declared] : declared;
  if (!Array.isArray(files)) throw new Error('无法定位 bundle 补丁');
  const result = [];
  for (const file of files) {
    if (typeof file !== 'string') throw new Error('无效的 bundle 补丁路径');
    const relative = file.replace(/^\.\//,'');
    if (!relative || relative.split('/').some(s=>!s || s === '.' || s === '..' || /[\\:\x00-\x1f]/.test(s))) {
      throw new Error('无效的 bundle 补丁路径');
    }
    const raw = await host.readTextFile(await safePath(host,pkg.root,relative));
    if (raw == null) throw new Error('bundle 补丁尚不可读');
    result.push(...parseLayer(raw));
  }
  return result;
}

// 来源标记附在副本的顶层属性上，随 Cordis 对该行和 config 的实际覆盖一起传播。
function markLayers(layers, unresolved) {
  const patches = [], groups = new Set(), regrouped = new Set();
  function mark(row, scope, inserted, depth = 0) {
    if (!object(row) || depth > 64) throw new Error('补丁行无法静态解析');
    const result = {};
    if (dynamic(row.id) || dynamic(row.name) || dynamic(row.group) || dynamic(row.insert)) unresolved.add(scope);
    for (const key of ['id','name','disabled','group']) {
      if (Object.hasOwn(row,key)) result[key] = row[key];
    }
    if (inserted) result.rowScope = scope;
    if (row.group === true && typeof row.id === 'string') groups.add(row.id);
    if (Object.hasOwn(row,'insert')) {
      if (!Array.isArray(row.insert)) throw new Error('无效的补丁插入行');
      result.insert = row.insert.map(child => mark(child,scope,true,depth+1));
    }
    if (Object.hasOwn(row,'config')) {
      result.configScope = scope;
      const group = row.group === true || groups.has(row.id);
      if (group && dynamic(row.config)) unresolved.add(scope);
      if (group && Array.isArray(row.config)) {
        result.config = row.config.map(child => mark(child,scope,true,depth+1));
        if (!inserted) {
          const collect = entries => {
            for (const entry of entries) {
              if (entry.id) regrouped.add(entry.id);
              if (entry.group && Array.isArray(entry.config)) collect(entry.config);
            }
          };
          collect(result.config);
        }
      } else result.config = row.config;
    }
    return result;
  }
  for (const {scope,patches:source} of layers) {
    for (const row of source) {
      // 旧版宿主对覆盖整个 group.config 后的子项保留旧索引，新版会重建。
      if (row.id && regrouped.has(row.id)) unresolved.add(scope+'：重编组后的条目标识');
      patches.push(mark(row,scope,false));
    }
  }
  return patches;
}

function summarize(roots, unresolved) {
  const rows = [], servers = [];
  function visit(nodes, parentEnabled) {
    for (const node of nodes) {
      const {id,name,rowScope:scope,configScope=scope,config} = node;
      const enabled = parentEnabled && node.disabled !== true;
      if (dynamic(id) || dynamic(name) || dynamic(node.group) ||
        ((node.group || name === manager || name === client) && dynamic(node.disabled))) unresolved.add(scope);
      if (typeof id === 'string' && !dynamic(id)) rows.push({id,name,scope,configScope,enabled});
      function binding(server, entry, prefix) {
        if (!id || dynamic(id) || typeof server !== 'string' || !serverName.test(server) ||
          !object(entry) || dynamic(entry) || dynamic(entry.enabled)) {
          unresolved.add(configScope);
          return;
        }
        const key = entry.oauth?.credentialKey;
        if (dynamic(entry.oauth) || dynamic(key)) unresolved.add(configScope);
        servers.push({server,owner:prefix+':'+id,scope:configScope,rowScope:scope,
          enabled:enabled && (prefix !== 'manager' || entry.enabled !== false),
          ...(typeof key === 'string' && !dynamic(key) ? {oauthKey:key} : {})});
      }
      if (name === manager) {
        if (dynamic(config) || dynamic(config?.servers)) unresolved.add(configScope);
        else if (object(config?.servers)) for (const [server,entry] of Object.entries(config.servers)) binding(server,entry,'manager');
      } else if (name === client) {
        binding(config?.serverName,config,'client');
      }
      if (node.group === true && Array.isArray(config)) visit(config,enabled);
    }
  }
  visit(roots,true);
  return {rows,servers};
}

/** 按 bundle、profile、HOME、显式启动覆盖文件的顺序读取本机 MCP 配置；未安装的依赖保留为待核验项。 */
export async function readMcpComposition(host, options) {
  const {profileDir,home,installAnchor,overlayPaths = []} = options;
  const layers = [], unresolved = new Set(), unresolvedBundles = [], missingBundles = [];
  const anchors = [];
  if (installAnchor) anchors.push(host.resolvePath(installAnchor,'..'));
  if (profileDir) anchors.push(host.resolvePath(profileDir));
  let bundles = options.bundles;
  if (bundles === undefined && profileDir) {
    const raw = await host.readTextFile(host.joinPath(profileDir,'package.json'));
    if (raw != null) {
      try { bundles = JSON.parse(raw)?.dsh?.profile?.bundles; } catch { unresolved.add('profile'); }
    }
  }
  if (bundles != null && !Array.isArray(bundles)) unresolved.add('profile');
  for (const name of Array.isArray(bundles) ? bundles : []) {
    try {
      const patches = await readBundle(host,anchors,name);
      if (patches === null) {
        missingBundles.push(name);
        unresolvedBundles.push(name);
        unresolved.add(name);
      } else layers.push({scope:'bundle',patches});
    } catch {
      const label = typeof name === 'string' && npmName.test(name) ? name : 'bundle';
      unresolved.add(label);
      unresolvedBundles.push(label);
    }
  }
  async function layer(scope, path, override, required = false) {
    try {
      const raw = override !== undefined ? override : path ? await host.readTextFile(path) : null;
      if (raw == null && required) { unresolved.add(scope); return; }
      layers.push({scope,patches:parseLayer(raw)});
    } catch { unresolved.add(scope); }
  }
  await layer('profile',profileDir && host.joinPath(profileDir,'cordis.patch.yml'),options.profilePatch);
  await layer('home',home && host.joinPath(home,'cordis.patch.yml'),options.homePatch);
  for (const path of overlayPaths) await layer('overlay',path,undefined,true);
  let entries = [];
  try {
    entries = await host.composePackEntries(markLayers(layers,unresolved));
  } catch {
    unresolved.add('Cordis 配置组合');
  }
  const result = summarize(entries,unresolved);
  return {...result,unresolved:[...unresolved],unresolvedBundles,missingBundles};
}
