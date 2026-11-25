import { isMap, isSeq, parseDocument, visit } from 'yaml';
import { localized } from './extension-files.js';
import { portableMcpTransport, validMcpArgumentPrefix } from './mcp-transport.js';

const providerModule = '@deepseek-ai/dsh-llm-pi-ai';
const mcpModule = '@deepseek-ai/dsh-mcp-client';
const scopedSkillId = 'dsh-launcher-pack-skills';
const scopedSkillModule = '@deepseek-ai/dsh-skill-filesystem';
const refPattern = /^[A-Za-z_][A-Za-z_0-9]*$/;
const sensitive = /(?:api.?key|secret|password|passwd|token|credential|authorization|cookie|signature|private.?key)/i;
const secretValue = /(?:^bearer\s|^basic\s|sk-[A-Za-z0-9_-]{8}|gh[pousr]_|-----BEGIN .*PRIVATE KEY)/i;

function docOf(raw) {
  const doc = parseDocument(raw?.trim() ? raw : '[]\n');
  if (doc.errors.length || !isSeq(doc.contents)) throw new Error('cordis.patch.yml 需为有效的 YAML 数组');
  return doc;
}

function refName(text) {
  const ref = text.toUpperCase().replace(/[^A-Z0-9_]+/g,'_');
  return refPattern.test(ref) ? ref : '_' + ref;
}

function malformedKey(key) {
  return ['__proto__','constructor','prototype'].includes(key);
}

function masked(value, id, path = [], refs = new Set()) {
  const key = String(path.at(-1) ?? '');
  const parent = path.at(-2);
  if (Array.isArray(value)) {
    if (id.startsWith('MCP_') && key === 'args' && path.length === 2 && path[0] === 'config') return value;
    return value.map((v,i) => masked(v,id,[...path,i],refs));
  }
  if (value && typeof value === 'object') {
    const out = {};
    for (const [k,v] of Object.entries(value)) {
      if (malformedKey(k) || k === '$credential') throw new Error('配置包含保留字段');
      out[k] = masked(v,id,[...path,k],refs);
    }
    return out;
  }
  if (typeof value !== 'string' || !value) return value;
  if (parent === 'oauth' && key === 'credentialKey' && /^mcp-client\/[a-z][a-z0-9-]*$/.test(value)) return value;
  if (key === 'apiKeyEnv' && refPattern.test(value)) return value;
  if (id.startsWith('PROVIDER_') && parent === 'headers' &&
      (/^(accept|content-type)$/i.test(key) && /^[\w/+*,;. =-]+$/.test(value) ||
       /^x-trace$/i.test(key) && /^(0|1|true|false)$/.test(value))) return value;
  const nativeRef = /^(Bearer |Basic )?\$\{credential:([A-Za-z_][A-Za-z_0-9]*)\}$/i.exec(value);
  if (nativeRef) {
    refs.add(nativeRef[2]);
    return nativeRef[1] ? {$credential:nativeRef[2],prefix:nativeRef[1]} : {$credential:nativeRef[2]};
  }
  const isPrivateUrl = /^(?:https?|ssh):\/\//i.test(value) &&
    (() => { try { const u = new URL(value); return !!(u.username || u.password || [...u.searchParams.keys()].some(k=>sensitive.test(k))); } catch { return true; } })();
  if (parent === 'headers' || parent === 'env' || sensitive.test(key) || secretValue.test(value) || isPrivateUrl) {
    const ref = refName(id + '_' + path.join('_'));
    refs.add(ref);
    const prefix = /^(Bearer |Basic )/i.exec(value)?.[1];
    return prefix ? {$credential:ref,prefix} : {$credential:ref};
  }
  return value;
}

const deepseek = '@deepseek-ai/dsh-llm-deepseek-api-key';
const manager = '@deepseek-ai/dsh-mcp-manager';

function eachRow(doc) {
  const rows = [];
  for (const row of doc.contents.items) {
    if (!isMap(row)) continue;
    rows.push({row,list:doc.contents});
    const insert = row.get('insert',true);
    if (isSeq(insert)) for (const child of insert.items) if (isMap(child)) rows.push({row:child,list:insert});
  }
  return rows;
}

/** 注册此外部技能目录使用单独的提供方，以保留用户配置的技能目录。 */
export function registerScopedSkillPatch(raw, profile) {
  const doc = docOf(raw);
  const entry = doc.createNode({ insert: [{ id: scopedSkillId, name: scopedSkillModule,
    config: { includeDefaultRoots: false, customSkillDirs: ['process.env.DSH_HOME + ' + JSON.stringify('/.dsh-pack/skills/' + profile)] } }] });
  const row = entry.get('insert',true).items[0];
  row.getIn(['config','customSkillDirs'],true).items[0].tag = 'tag:yaml.org,2002:js';
  const previous = eachRow(doc).find(({row}) => row.get('id') === scopedSkillId);
  if (previous) {
    if (previous.row.get('name') !== scopedSkillModule || previous.list === doc.contents) throw new Error('配置档已占用整合包技能提供方标识');
    previous.list.items[previous.list.items.indexOf(previous.row)] = row;
  } else {
    doc.contents.items.push(entry);
  }
  return doc.toString();
}

function withoutLocalScopedSkill(raw) {
  if (!raw.includes(scopedSkillId)) return raw;
  const doc = docOf(raw);
  const removed = new Set();
  for (const {row,list} of eachRow(doc)) {
    if (row.get('id') === scopedSkillId && row.get('name') === scopedSkillModule && list !== doc.contents) {
      list.items.splice(list.items.indexOf(row),1);
      removed.add(list);
    }
  }
  doc.contents.items = doc.contents.items.filter(row => {
    const list = isMap(row) ? row.get('insert',true) : null;
    return !removed.has(list) || row.items.length !== 1 || list.items.length !== 0;
  });
  return doc.toString();
}

function patchNodes(doc) {
  const nodes = [];
  for (const {row,list} of eachRow(doc)) {
    const module = row.get('name');
    const owner = row.get('id');
    const insertOwner = list !== doc.contents;
    const providers = row.getIn(['config','providers'],true);
    if ((module === providerModule || owner === 'llm-pi-ai') && isMap(providers)) {
      for (const pair of providers.items) nodes.push({kind:'provider',id:String(pair.key.value),node:pair.value,
        owner:owner || 'llm-pi-ai',insertOwner,remove:()=>providers.delete(pair.key.value)});
    }
    if (module === deepseek || owner === 'llm-deepseek') {
      const node = row.get('config',true);
      if (isMap(node)) nodes.push({kind:'provider',id:'deepseek-official',binding:'deepseek',owner:owner || 'llm-deepseek',
        insertOwner,node,remove:()=>row.delete('config')});
    }
    const servers = row.getIn(['config','servers'],true);
    if ((module === manager || owner === 'mcp-manager' || owner === 'mcp') && isMap(servers)) {
      for (const pair of servers.items) nodes.push({kind:'mcp',id:String(pair.key.value),binding:'manager',
        owner:owner || 'mcp-manager',insertOwner,node:pair.value,remove:()=>{
          servers.delete(pair.key.value);
          if (!servers.items.length) {
            row.deleteIn(['config','servers']);
            if (!row.get('config',true).items.length) row.delete('config');
          }
        }});
    }
    if (module === mcpModule) {
      const id = owner || row.getIn(['config','serverName']);
      if (!id) throw new Error('MCP 配置缺少条目 ID 与 serverName');
      nodes.push({kind:'mcp',id:String(id),node:row,remove:()=>list.items.splice(list.items.indexOf(row),1)});
    }
  }
  return nodes;
}

export function splitPatch(raw, scope) {
  if (!raw?.trim()) return {items:[],patch:raw || ''};
  raw = withoutLocalScopedSkill(raw);
  if (!/llm-pi-ai|llm-deepseek|mcp/.test(raw)) return {items:[],patch:raw};
  const doc = docOf(raw);
  const nodes = patchNodes(doc);
  if (!nodes.length) return {items:[],patch:raw};
  const items = [];
  for (const {kind,id,node,remove,binding,owner,insertOwner} of nodes) {
    if (!id || malformedKey(id)) throw new Error('无效的配置标识');
    // 选中的行将输出成 JSON；不可执行、展开或悄悄丢弃 YAML 表达式与别名。
    let unsupported = false;
    visit(node,{Alias() { unsupported = true; },Node(_key,n) { if (n?.tag) unsupported = true; }});
    let original = node?.toJSON();
    if (binding === 'manager') original = {name:mcpModule,id,config:{...original,serverName:id}};
    let inlineKey = false;
    if (kind === 'provider' && original && typeof original === 'object') {
      original = {...original};
      if (Object.hasOwn(original,'apiKey')) { inlineKey = !!original.apiKey; delete original.apiKey; }
      if (original.headers && typeof original.headers === 'object') {
        original.headers = {...original.headers};
        for (const [name,header] of Object.entries(original.headers)) {
          if (/^x-api-key$/i.test(name) || (/^authorization$/i.test(name) && /^Bearer /i.test(header))) {
            inlineKey = true;
            const ref = /\$\{credential:([a-zA-Z_][a-zA-Z_0-9]*)\}/.exec(header);
            if (ref) original.apiKeyEnv ??= ref[1];
            delete original.headers[name];
          }
        }
        if (!Object.keys(original.headers).length) delete original.headers;
      }
      if (binding === 'deepseek') original.apiKeyEnv ??= 'DEEPSEEK_API_KEY';
    }
    const refs = new Set();
    const prefix = refName(kind+'_'+id);
    if (kind === 'mcp') {
      try { original = {...original,config:portableMcpTransport(original?.config,prefix,refs)}; }
      catch {
        // 无法移植的条目也从原始补丁剥离，发现结果仅保留公开标识。
        unsupported = true;
        refs.clear();
        original = {name:mcpModule,id,config:{serverName:id}};
      }
    }
    const value = masked(original,prefix,[],refs);
    if (kind === 'provider') {
      // pi-ai 仅在 apiKeyEnv 解析凭据，其余字段中的敏感值无法无损还原。
      if (refs.size) unsupported = true;
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Provider 配置必须为对象');
      if (inlineKey) value.apiKeyEnv ??= refName(id) + '_API_KEY';
      if (value.apiKeyEnv != null) {
        if (!refPattern.test(value.apiKeyEnv)) throw new Error('Provider 密钥引用名无效');
        refs.add(value.apiKeyEnv);
      }
    }
    const details = kind === 'provider' ? value : value.config;
    items.push({kind,id,scope,key:kind+':'+scope+':'+id,name:localized(details?.displayName,details?.serverName || id),
      desc:localized(details?.description,''),value,credentialRefs:[...refs],unsupported,binding,owner,insertOwner,
      destination:typeof details?.baseURL === 'string' ? details.baseURL : typeof details?.baseUrl === 'string' ? details.baseUrl : typeof details?.url === 'string' ? details.url : details?.command || ''});
    remove();
  }
  return {items,patch:doc.toString()};
}

export function credentialRefs(item) {
  const refs = new Set();
  function walk(value,path = []) {
    if (!value || typeof value !== 'object') return;
    if (Object.hasOwn(value,'$credential')) {
      const arg = item.kind === 'mcp' && path.length === 3 && path[0] === 'config' && path[1] === 'args';
      if (!refPattern.test(value.$credential) || Object.keys(value).some(k=>!['$credential','prefix'].includes(k)) ||
          (value.prefix != null && !(arg ? validMcpArgumentPrefix(value.prefix) : /^(Bearer |Basic )$/i.test(value.prefix)))) throw new Error('凭据占位格式无效');
      refs.add(value.$credential);
    } else for (const [key,v] of Object.entries(value)) {
      if (malformedKey(key)) throw new Error('配置包含保留字段');
      walk(v,[...path,key]);
    }
  }
  walk(item.value);
  if (item.kind === 'provider' && item.value?.apiKeyEnv != null) {
    if (!refPattern.test(item.value?.apiKeyEnv)) throw new Error('Provider 缺少有效凭据引用');
    refs.add(item.value.apiKeyEnv);
  }
  return [...refs];
}

export function fillSlots(value, values) {
  if (Array.isArray(value)) return value.map(v=>fillSlots(v,values));
  if (!value || typeof value !== 'object') return value;
  if (Object.hasOwn(value,'$credential')) return (value.prefix || '') + values[value.$credential];
  return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,fillSlots(v,values)]));
}

function matching(doc,item) {
  return patchNodes(doc).filter(n => item.kind === n.kind && (item.id === n.id ||
    (item.kind === 'mcp' && (n.binding === 'manager' ? n.id : n.node.getIn(['config','serverName'])) === item.value?.config?.serverName)));
}

export function mcpNamespaceBindings(raw) {
  if (!raw || !raw.includes('mcp')) return [];
  return patchNodes(docOf(raw)).filter(n=>n.kind === 'mcp').map(n=>({
    server:n.binding === 'manager' ? n.id : n.node.getIn(['config','serverName']),
    owner:n.binding === 'manager' ? 'manager:'+n.owner : 'client:'+n.id,
  }));
}

export function patchConflict(raw,item) {
  if (!raw?.trim()) return false;
  const doc = docOf(raw);
  if (matching(doc,item).length) return true;
  if (item.kind !== 'mcp') return false;
  return doc.contents.items.some(row => isMap(row) && isSeq(row.get('insert',true)) &&
    row.get('insert',true).items.some(other => isMap(other) && other.get('id') === item.id));
}

export function materializeMcp(item) {
  if (item.kind !== 'mcp' || item.binding !== 'manager') return item;
  const original = item.value.config;
  const server = original.serverName;
  const id = 'dspack-mcp-' + server;
  const transport = original.transport || 'stdio';
  const config = {transport,serverName:server,expandTemplates:true,failOnStartupError:false,
    startupTimeoutMs:original.startupTimeoutMs ?? 30000,toolLoading:original.toolLoading ?? 'eager'};
  const keys = transport === 'stdio' ? ['command','args','env','cwd'] : ['url','headers','oauth'];
  for (const key of [...keys,'toolCallTimeoutMs','closeTimeoutMs','reconnect']) {
    if (original[key] !== undefined) config[key] = original[key];
  }
  const row = {id,name:mcpModule,config};
  if (original.enabled === false) row.disabled = true;
  if (item.credentialRefs?.length || original.oauth) row.inject = ['credentials'];
  return {...item,id,binding:undefined,fromManager:true,value:row};
}

export async function baselineOwnsMcp(host,tree,id,server) {
  if (!tree?.fields) return false;
  const hash = tuple => host.sha256(new TextEncoder().encode(JSON.stringify(tuple)));
  const [idKey,desired,configKey,serversKey,serverKey,insertKey] = await Promise.all([
    hash(['key','id']),hash(['value',id]),hash(['key','config']),hash(['key','servers']),hash(['key',server]),hash(['key','insert']),
  ]);
  function contains(list) {
    for (const row of Object.values(list?.fields ?? {})) {
      const fields = row.fields;
      if (fields?.[idKey]?.hash === desired &&
          (server === undefined || fields[configKey]?.fields?.[serversKey]?.fields?.[serverKey])) return true;
      if (fields?.[insertKey] && contains(fields[insertKey])) return true;
    }
    return false;
  }
  return contains(tree);
}

export async function preserveLocalManagerServers(host,current,staged,tree) {
  if (!current?.includes('mcp')) return staged;
  let result = staged;
  for (const n of patchNodes(docOf(current))) {
    if (n.binding !== 'manager' || await baselineOwnsMcp(host,tree,n.owner,n.id)) continue;
    const value = {id:n.id,name:mcpModule,config:{...n.node.toJSON(),serverName:n.id}};
    const item = {...n,value};
    if (!matching(docOf(result),item).length) result = mergePatch(result,item,value);
  }
  return result;
}

/** 用户明确选择替换时，仅移除同一目标补丁里的对应服务。空表仍保留该层原有的覆盖语义。 */
export function removeEditableManagerServer(staged,current,owner,server) {
  const doc = docOf(staged);
  const source = eachRow(docOf(current)).findLast(({row})=>row.get('id') === owner && row.hasIn(['config','servers',server]));
  if (!source) throw new Error('本地 MCP 配置已变化，请重新预览：'+server);
  let target = eachRow(doc).findLast(({row})=>row.get('id') === owner)?.row;
  if (!target) {
    target = source.row.clone();
    const sourceRow = patchNodes(docOf(current)).find(n=>n.binding === 'manager' && n.owner === owner && n.id === server);
    if (sourceRow?.insertOwner) {
      const insertion = doc.createNode({insert:[]});
      insertion.get('insert',true).add(target);
      doc.contents.add(insertion);
    } else doc.contents.add(target);
  }
  if (!target.hasIn(['config','servers'])) target.setIn(['config','servers'],doc.createNode({}));
  target.deleteIn(['config','servers',server]);
  return doc.toString();
}

export function mergePatch(raw,item,value) {
  const doc = docOf(raw);
  for (const node of matching(doc,item)) {
    if (item.fromManager && !item.binding && (node.binding === 'manager' || node.id !== item.id)) continue;
    node.remove();
  }
  if (item.kind === 'provider' || item.binding === 'manager') {
    const module = item.binding === 'deepseek' ? deepseek : item.binding === 'manager' ? manager : providerModule;
    const id = item.owner || (item.binding === 'deepseek' ? 'llm-deepseek' : item.binding === 'manager' ? 'mcp-manager' : 'llm-pi-ai');
    let row = eachRow(doc).find(({row})=>row.get('id') === id || row.get('name') === module)?.row;
    if (!row) {
      if (item.insertOwner) {
        const insertion = doc.createNode({insert:[{id,name:module}]});
        doc.contents.add(insertion);
        row = insertion.getIn(['insert',0],true);
      } else {
        row = doc.createNode({id,name:module});
        doc.contents.add(row);
      }
    }
    if (item.binding === 'deepseek') row.set('config',doc.createNode(value));
    else if (item.binding === 'manager') {
      const config = {...value.config};
      delete config.serverName;
      row.setIn(['config','servers',item.id],doc.createNode(config));
    } else row.setIn(['config','providers',item.id],doc.createNode(value));
  } else {
    const occupied = item.fromManager
      ? eachRow(doc).some(({row})=>row.get('id') === item.id)
      : patchConflict(doc.toString(),item);
    if (occupied) throw new Error('MCP 条目 ID 已被其他插件使用：' + item.id);
    doc.contents.add(doc.createNode({insert:[value]}));
  }
  return doc.toString();
}

export function openCredentialStore(raw) {
  const doc = parseDocument(raw ?? 'version: 1\nrefs: {}\n');
  if (doc.errors.length || !isMap(doc.contents) || doc.get('version') !== 1 ||
      doc.contents.items.some(p=>!['version','refs','records'].includes(p.key.value)) ||
      (doc.has('refs') && !isMap(doc.get('refs',true)))) throw new Error('凭据库格式无效，已保留原文件');
  return doc;
}

function ordered(value) {
  if (Array.isArray(value)) return value.map(ordered);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.keys(value).sort().map(k=>[k,ordered(value[k])]));
}

export function copyLocalPatchItem(source,destination,record) {
  if (!source?.trim()) return destination;
  const present = matching(docOf(source),record)[0];
  if (!present) return destination;
  const local = {...record,id:present.id,binding:present.binding,owner:present.owner,insertOwner:present.insertOwner};
  let config = present.node.toJSON();
  if (local.binding === 'manager') config = {id:local.id,name:mcpModule,config:{...config,serverName:local.id}};
  local.value = config;
  return mergePatch(destination,local,config);
}

export function hasInstalledConfig(raw,record,expected) {
  if (!raw?.trim()) return false;
  const node = matching(docOf(raw),record).find(match=>match.id === record.id && match.binding === record.binding);
  if (!node) return false;
  let target = expected;
  if (record.binding === 'manager') { target = {...expected.config}; delete target.serverName; }
  return JSON.stringify(ordered(node.node.toJSON())) === JSON.stringify(ordered(target));
}

export function mcpClientInPatch(raw,id) {
  if (!raw?.trim()) return null;
  const clients = eachRow(docOf(raw)).filter(({row})=>row.get('id') === id && row.get('name') === mcpModule);
  return clients.length === 1 ? clients[0].row.toJSON() : null;
}
