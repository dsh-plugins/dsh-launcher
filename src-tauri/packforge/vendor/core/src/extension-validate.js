import { localized, expandTemplate, protectedTarget, relativePath, segment } from './extension-files.js';
import { checkSkillSource } from './extension-skills.js';
import { credentialRefs } from './extension-patch.js';
import { portableMcpTransport } from './mcp-transport.js';

const decoder = new TextDecoder('utf-8',{fatal:true});
const sha256 = /^[a-f0-9]{64}$/;
const sha = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/;
const fileKinds = {'path.json':'path','provider.json':'provider','mcp.json':'mcp','skill.json':'skill'};
const own = (v,k)=>Object.hasOwn(v ?? {},k);

export function validatedItems(manifest,entries) {
  const items = [], keys = new Set(), blobs = new Set(), targets = new Set(), emptyDirectories = new Set();
  const check = (ok,context) => { if (!ok) throw new Error('扩展资源格式无效：'+context); };
  function blob(file,kind) {
    check(sha256.test(file?.sha256) && file.entry === 'extension-data/'+kind+'/'+file.sha256 &&
      own(entries,file.entry) && entries[file.entry] instanceof Uint8Array,'文件引用');
    blobs.add(file.entry);
  }
  function register(item,kind,plugin) {
    check(item?.kind === kind && typeof item.key === 'string' && !keys.has(item.key),'资源类型或重复标识');
    check(typeof item.id === 'string' && item.id.length > 0 && !['__proto__','prototype','constructor'].includes(item.id),'id');
    check(typeof item.scope === 'string' && (item.scope === 'home' || item.scope.startsWith('profile:')),'scope');
    const profile = item.scope === 'home' ? manifest.profileName : segment(item.scope.slice(8));
    if (manifest.type === 'dshhome' && item.scope !== 'home') check(own(manifest.profiles,profile),'未知 profile');
    check(item.name != null && localized(item.name) != null,'name');
    localized(item.desc,'');
    keys.add(item.key);
    if (kind === 'path') {
      check(typeof plugin === 'string' && /^(?:@[a-z0-9._-]+\/)?[a-z0-9][a-z0-9._-]*$/i.test(plugin) && item.plugin === plugin &&
        item.key === [kind,item.scope,plugin,item.id].join(':'),'plugin');
      check(Array.isArray(item.paths) && item.paths.length && Array.isArray(item.files),'paths/files');
      check(item.directories === undefined || Array.isArray(item.directories),'directories');
      const roots = item.paths.map(p=>expandTemplate(p,profile));
      for (const root of roots) check(!protectedTarget(root),'路径占用系统配置');
      const directories = item.directories ?? [];
      const fileTargets = new Set(item.files);
      for (const f of [...item.files,...directories]) {
        check(Number.isInteger(f?.index) && own(roots,f.index) && typeof f.tail === 'string','路径规则编号');
        if (f.tail) relativePath(f.tail);
        const target = roots[f.index]+(f.tail ? '/'+f.tail : '');
        check(f.path === target && !protectedTarget(target) && !targets.has(target.toLowerCase()),'插件文件目标');
        targets.add(target.toLowerCase());
        if (fileTargets.has(f)) blob(f,'path');
        else {
          check(Object.keys(f).every(k=>['index','tail','path'].includes(k)),'空目录记录');
          emptyDirectories.add(target.toLowerCase());
        }
      }
    } else {
      check(item.key === kind+':'+item.scope+':'+item.id,'key');
      if (kind === 'skill') {
        segment(item.id);
        check((item.scope === 'home' || manifest.type === 'dshhome') && sha256.test(item.digest),'技能作用域与摘要');
        if (item.mode === 'embedded') {
          check(Array.isArray(item.files) && item.files.length,'技能文件清单');
          const paths = new Set();
          for (const f of item.files) {
            const p = relativePath(f.path).toLowerCase();
            check(!paths.has(p) && p !== '.dsh-skill.json','技能文件重复');
            paths.add(p); blob(f,'skill');
          }
          check(item.flat ? item.files.length === 1 && item.files[0].path === item.id : paths.has('skill.md'),'技能入口');
        } else {
          checkSkillSource(item);
          check(!item.flat && item.files == null && sha.test(item.commit) && (item.mode !== 'fixed' || item.commit === item.ref),'远程技能来源');
        }
      } else {
        check(item.value && typeof item.value === 'object' && !Array.isArray(item.value),'配置对象');
        if (kind === 'mcp') {
          const config = item.value.config;
          check(item.value.name === '@deepseek-ai/dsh-mcp-client' && item.value.id === item.id &&
            typeof config?.serverName === 'string' && /^[a-z0-9_-]{1,32}$/i.test(config.serverName),'MCP 标识');
          check(['stdio','streamable-http'].includes(config.transport || 'stdio'),'MCP transport');
          portableMcpTransport(config,'MCP',new Set(),true);
          for (const map of [config.env,config.headers]) {
            if (map == null) continue;
            check(typeof map === 'object' && !Array.isArray(map),'MCP 变量表');
            for (const v of Object.values(map)) check(v === '' || v?.$credential ||
              (typeof v === 'string' && /^\$\{credential:[A-Za-z_][A-Za-z0-9_]*\}$/.test(v)),'MCP 字段含未脱敏值');
          }
        }
        const refs = credentialRefs(item);
        check(Array.isArray(item.credentialRefs) && refs.length === item.credentialRefs.length &&
          refs.every(r=>item.credentialRefs.includes(r)),'凭据引用清单');
      }
    }
    items.push(item);
  }
  for (const [name,kind] of Object.entries(fileKinds)) {
    if (!own(entries,name)) continue;
    let data;
    try { data = JSON.parse(decoder.decode(entries[name])); } catch { throw new Error('扩展清单无法解码：'+name); }
    if (kind === 'path') {
      check(data?.plugins && typeof data.plugins === 'object' && !Array.isArray(data.plugins),name);
      for (const [plugin,rows] of Object.entries(data.plugins)) {
        check(Array.isArray(rows),name); rows.forEach(item=>register(item,kind,plugin));
      }
    } else { check(Array.isArray(data?.items),name); data.items.forEach(item=>register(item,kind)); }
  }
  for (const path of targets) {
    let parent = path;
    while (parent.includes('/')) {
      parent = parent.slice(0,parent.lastIndexOf('/'));
      check(!targets.has(parent),'文件或空目录含有其他目标');
    }
  }
  for (const path of Object.keys(entries)) {
    if (path.startsWith('extension-data/')) check(blobs.has(path),'未被声明的扩展文件');
    if (!emptyDirectories.size || path.endsWith('/')) continue;
    let target;
    if (path.startsWith('home/') && manifest.type !== 'dshhome') target = path.slice(5);
    if (path.startsWith('overrides/')) target = (manifest.type === 'dshhome' ? '' : 'profiles/'+manifest.profileName+'/') + path.slice(10);
    if (target) {
      const parts = target.toLowerCase().split('/');
      while (parts.length) {
        check(!emptyDirectories.has(parts.join('/')),'空目录与快照文件重复');
        parts.pop();
      }
    }
  }
  return items;
}

export function validateExtensions(manifest,entries) {
  validatedItems(manifest,entries);
}
