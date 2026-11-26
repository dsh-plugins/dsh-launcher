import { checkoutSkill, checkSkillSource } from './extension-skills.js';
import { hasInstalledConfig, copyLocalPatchItem, credentialRefs, baselineOwnsMcp, fillSlots, materializeMcp, mergePatch, openCredentialStore, patchConflict, preserveLocalManagerServers, removeEditableManagerServer, registerScopedSkillPatch } from './extension-patch.js';
import { checkMcpInstall } from './mcp-install.js';
import { digestFiles, expandTemplate, protectedTarget, readJson, readTree, relativePath, safePath, segment } from './extension-files.js';
import { validatedItems } from './extension-validate.js';

const has = (obj,key) => Object.hasOwn(obj ?? {},key);
const marker = ref => '$'+'{credential:'+ref+'}';

function resolvedScope(item, opts) {
  if (item.scope === 'home') return opts.manifest.type === 'dshhome' ? null : opts.profileName;
  if (!item.scope.startsWith('profile:')) throw new Error('无效的配置作用域');
  const name = segment(item.scope.slice(8));
  if (opts.manifest.type !== 'dshhome') return opts.profileName;
  if (!has(opts.manifest.profiles,name)) throw new Error('扩展条目引用未知 profile：'+name);
  return name;
}

function scopeRoot(host, opts, name, actual = false) {
  const home = actual ? (opts.credentialHome || opts.home) : opts.home;
  if (opts.manifest.type !== 'dshhome') return (actual && opts.credentialProfileDir) || opts.profileDir || host.joinPath(home,'profiles',segment(opts.profileName));
  return name == null ? home : host.joinPath(home,'profiles',name);
}

async function checkedBytes(host,entries,kind,file) {
  if (!file || !/^[a-f0-9]{64}$/.test(file.sha256) || file.entry !== 'extension-data/'+kind+'/'+file.sha256 ||
      !(entries[file.entry] instanceof Uint8Array) || await host.sha256(entries[file.entry]) !== file.sha256) {
    throw new Error('资源文件缺失或摘要不符');
  }
  return entries[file.entry];
}

function dotenv(text,ref) {
  for (const line of (text ?? '').split(/\r?\n/)) {
    const match = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/.exec(line);
    if (match?.[1] === ref) {
      const value = match[2].trim();
      return ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) ? value.slice(1,-1) : value;
    }
  }
  return null;
}

async function existingSecret(host,home,opts,ref) {
  const raw = await host.readTextFile(host.joinPath(home,'.credentials.yaml'));
  if (raw != null) {
    const v = openCredentialStore(raw).getIn(['refs',ref]);
    if (typeof v === 'string' && v) return v;
  }
  const configured = opts.credentialEnvironment?.(ref);
  if (configured) return configured;
  const env = host.env?.(ref);
  if (env) return env;
  for (const dir of new Set([opts.credentialCwd ?? host.cwd?.(),home].filter(Boolean))) {
    const v = dotenv(await host.readTextFile(host.joinPath(dir,'.env')),ref);
    if (v) return v;
  }
  return null;
}


export async function planExtensionInstall(host, opts) {
  const {entries,manifest,home} = opts;
  const realHome = opts.credentialHome || home;
  const actions = [];
  const targetFiles = new Set();
  const patchIds = new Set();
  const pending = [];
  const preparedManagerPatches = new Set();
  const preservePaths = [];
  const directories = [];
  const executablePaths = [];
  const scopedSkillProfiles = new Set();
  function homeRel(abs) {
    const prefix = host.resolvePath(home).replace(/\\/g,'/')+'/';
    const path = host.resolvePath(abs).replace(/\\/g,'/');
    return path.toLowerCase().startsWith(prefix.toLowerCase()) ? path.slice(prefix.length) : null;
  }
  const records = validatedItems(manifest,entries);
  const useCounts = new Map();
  for (const record of records) for (const ref of record.credentialRefs ?? []) useCounts.set(ref,(useCounts.get(ref) || 0)+1);
  for (const record of records) {
    const scope = resolvedScope(record,opts);
    const root = scopeRoot(host,opts,scope);
    const actualRoot = scopeRoot(host,opts,scope,true);
    const item = {key:record.key,id:record.id,kind:record.kind,scope:record.scope,name:record.name,desc:record.desc,
      destination:'',conflict:false,credentialRefs:[],missingCredentials:[],availableCredentials:[]};
    let run, preserve;
    let duplicate = false;
    let mcp;
    if (record.kind === 'provider' || record.kind === 'mcp') {
      if (record.kind === 'mcp' && (record.value?.name !== '@deepseek-ai/dsh-mcp-client' ||
          record.value?.id !== record.id || !record.value.config?.serverName)) throw new Error('MCP 配置与条目 ID 不符');
      const patch = host.joinPath(root,'cordis.patch.yml');
      const actualPatch = host.joinPath(actualRoot,'cordis.patch.yml');
      const current = await host.readTextFile(actualPatch);
      const targetRecord = materializeMcp(record);
      const manager = record.binding === 'manager';
      const relPatch = (scope === null ? '' : (opts.profilesDir || 'profiles')+'/'+scope+'/')+'cordis.patch.yml';
      const tree = opts.baselineFiles?.[relPatch]?.tree;
      if (record.kind === 'mcp') {
        const id = targetRecord.id;
        const config = targetRecord.value.config;
        mcp = {scope,id,actualPatch,server:config.serverName,owner:'client:'+id,legacy:manager ? record.owner || 'mcp-manager' : null,tree,
          oauthKey:config.oauth?.credentialKey,enabled:targetRecord.value.disabled !== true,
          owned:await baselineOwnsMcp(host,tree,id)};
        if (manager) item.notice = '此服务将由独立 MCP 客户端运行，可在该客户端的配置行管理；MCP 管理器不再列出此条目';
      }
      const config = record.kind === 'mcp' ? record.value.config : record.value;
      if (record.kind === 'provider') {
        const providerType = record.binding === 'deepseek' ? 'DeepSeek' : config.api;
        if (typeof providerType === 'string' && /^[a-z][a-z\d._-]{0,63}$/i.test(providerType)) {
          item.providerType = providerType;
        }
      }
      item.destination = [config.url,config.baseURL,config.baseUrl,config.command].find(v=>typeof v === 'string') || record.id;
      const ids = [actualPatch+'/'+record.kind+'/'+record.id];
      if (record.kind === 'mcp') ids.push(actualPatch+'/server/'+record.value.config.serverName);
      duplicate = ids.some(id=>patchIds.has(id));
      item.conflict = duplicate || (manager ? false : !mcp?.owned && patchConflict(current,record));
      preserve = async () => {
        if (!opts.staging || !current) return;
        const staged = await host.readTextFile(patch);
        const kept = copyLocalPatchItem(current,staged,targetRecord);
        if (kept !== staged) await host.writeTextFile(patch,kept);
      };
      ids.forEach(id=>patchIds.add(id));
      const values = Object.create(null), activeRefs = Object.create(null);
      item.credentialRefs = credentialRefs(record);
      for (const ref of item.credentialRefs) {
        const scopedInput = opts.credentials?.[record.key];
        const explicit = opts.confirmedCredentials?.[record.key] === true;
        let supplied = scopedInput && typeof scopedInput === 'object' && has(scopedInput,ref) ? scopedInput[ref] : undefined;
        if (supplied === undefined && (useCounts.get(ref) === 1 || explicit) && has(opts.credentials,ref)) supplied = opts.credentials[ref];
        if (supplied && (typeof supplied !== 'string' || !/^[\x21-\x7e]+$/.test(supplied))) throw new Error('凭据含无效字符：'+ref);
        activeRefs[ref] = ref;
        const existing = await existingSecret(host,realHome,opts,ref);
        if (existing) item.availableCredentials.push(ref);
        if (supplied) {
          if ((existing && existing !== supplied) || useCounts.get(ref)>1) {
            // 共享引用不能让新输入的密钥被另一个用途直接消费。
            const hash = await host.sha256(new TextEncoder().encode(JSON.stringify([actualRoot,record.key,item.destination,ref])));
            activeRefs[ref] = 'DSPACK_'+hash.slice(0,16).toUpperCase()+'_'+ref;
            const old = await existingSecret(host,realHome,opts,activeRefs[ref]);
            if (old && old !== supplied) throw new Error('此用途已有独立凭据，请先在 DSH 中更新：'+activeRefs[ref]);
          }
          values[activeRefs[ref]] = supplied;
        } else if (existing && explicit) values[ref] = existing;
        else item.missingCredentials.push(ref);
      }
      run = async () => {
        const tokens = Object.fromEntries(item.credentialRefs.map(ref=>[ref,marker(activeRefs[ref])]));
        const value = fillSlots(targetRecord.value,tokens);
        if (record.kind === 'provider' && value.apiKeyEnv) value.apiKeyEnv = activeRefs[value.apiKeyEnv];
        if (record.kind === 'mcp' && record.binding !== 'manager' && item.credentialRefs.length) value.config.expandTemplates = true;
        let staged = await host.readTextFile(patch);
        if (mcp && opts.staging && !preparedManagerPatches.has(patch)) {
          staged = await preserveLocalManagerServers(host,current,staged,tree);
          preparedManagerPatches.add(patch);
        }
        if (mcp?.replaceOwner) staged = removeEditableManagerServer(staged,current,mcp.replaceOwner,mcp.server);
        await host.writeTextFile(patch,mergePatch(staged,targetRecord,value));
        pending.push({record:targetRecord,values,expected:value,actualPatch});
      };
    } else if (record.kind === 'path') {
      if (!Array.isArray(record.paths) || !Array.isArray(record.files)) throw new Error('插件路径声明无效');
      const files = [], emptyDirs = [];
      const dirRecords = new Set(record.directories ?? []);
      for (const file of [...record.files,...dirRecords]) {
        if (!Number.isInteger(file.index) || !has(record.paths,file.index) || typeof file.tail !== 'string') throw new Error('插件文件不属于路径规则');
        if (file.tail) relativePath(file.tail);
        const suffix = file.tail ? '/'+file.tail : '';
        const sourceProfile = record.scope.startsWith('profile:') ? record.scope.slice(8) : manifest.profileName;
        const sourceRel = expandTemplate(record.paths[file.index],sourceProfile)+suffix;
        if (sourceRel !== file.path || protectedTarget(sourceRel)) throw new Error('插件数据目标路径超出声明范围');
        const rel = expandTemplate(record.paths[file.index],scope)+suffix;
        const scoped = manifest.type !== 'dshhome' && scope && rel.startsWith('profiles/'+scope+'/');
        const tail = scoped ? rel.slice(('profiles/'+scope+'/').length) : rel;
        const dest = await safePath(host,scoped ? root : home,tail);
        const real = await safePath(host,scoped ? actualRoot : realHome,tail);
        if (targetFiles.has(real.toLowerCase())) throw new Error('插件数据目标路径重复');
        targetFiles.add(real.toLowerCase());
        if (dirRecords.has(file)) {
          const existing = await host.packEntryType(real);
          if (existing && existing !== 'dir') item.conflict = true;
          emptyDirs.push(dest);
          continue;
        }
        const data = await checkedBytes(host,entries,'path',file);
        if (await host.stat(real) && await host.sha256File(real) !== file.sha256) item.conflict=true;
        files.push({dest,data});
      }
      item.destination = record.paths.join(', ');
      preserve = async () => {
        for (const f of files) { const rel = homeRel(f.dest); if (rel) preservePaths.push(rel); }
        for (const dest of emptyDirs) { const rel = homeRel(dest); if (rel) preservePaths.push(rel); }
      };
      run = async () => {
        for (const dest of emptyDirs) {
          await host.mkdir(dest);
          const rel = homeRel(dest);
          if (rel) directories.push(rel);
        }
        for (const f of files) await host.writeFile(f.dest,f.data);
      };
    } else {
      const id = segment(record.id);
      const scopedSlot = manifest.type === 'dshhome' && record.scope !== 'home';
      const skillRoot = scopedSlot ? host.joinPath(home,'.dsh-pack','skills',scope) : opts.skillRoot || host.joinPath(home,'skills');
      const dest = await safePath(host,skillRoot,id);
      const realSkillRoot = scopedSlot ? host.joinPath(realHome,'.dsh-pack','skills',scope)
        : opts.actualSkillRoot || (opts.credentialHome ? host.joinPath(realHome,'skills') : skillRoot);
      const actual = await safePath(host,realSkillRoot,id);
      item.destination = actual;
      preserve = async () => { const rel = homeRel(dest); if (rel) preservePaths.push(rel); };
      const stat = await host.stat(actual);
      const profileSlot = scopedSlot ? scope : opts.actualSkillRoot &&
        host.resolvePath(opts.actualSkillRoot) === host.joinPath(realHome,'.dsh-pack','skills',opts.profileName)
          ? opts.profileName : null;
      let localDigest;
      if (stat?.isDirectory) localDigest = await digestFiles(host,await readTree(host,actual));
      if (stat?.isFile) localDigest = await digestFiles(host,new Map([[id,await host.readFile(actual)]]));
      let baseline = record.digest;
      if (record.mode !== 'embedded' && stat?.isDirectory) {
        const origin = await readJson(host,host.joinPath(actual,'.dsh-skill.json'));
        if (origin?.repo === record.repository+(record.subdir ? '#/'+record.subdir : '') && origin.digest) baseline = origin.digest;
      }
      item.conflict = !!stat && localDigest !== baseline;
      if (record.mode !== 'embedded') checkSkillSource(record);
      if (record.mode === 'embedded' && !Array.isArray(record.files)) throw new Error('内嵌技能缺少文件');
      const embedded = new Map();
      for (const f of record.files ?? []) {
        relativePath(f.path);
        if ([...embedded.keys()].some(k=>k.toLowerCase() === f.path.toLowerCase())) throw new Error('技能文件路径重复');
        embedded.set(f.path,await checkedBytes(host,entries,'skill',f));
      }
      if (record.mode === 'embedded' && (!embedded.size || await digestFiles(host,embedded) !== record.digest ||
        (record.flat ? !embedded.has(id) || embedded.size !== 1 : !embedded.has('SKILL.md')))) throw new Error('内嵌技能文件清单与摘要不符');
      run = async () => {
        const remote = record.mode === 'embedded' ? null : await checkoutSkill(host,record,id);
        if (record.mode === 'fixed' && remote.digest !== record.digest) throw new Error('固定提交的技能内容与归档摘要不符');
        const files = remote?.files || embedded;
        const nonce = await host.mkdtemp('dspack-skill-install-');
        const temp = host.joinPath(skillRoot,'.'+host.basename(nonce));
        await host.rm(nonce,{recursive:true,force:true});
        if (await host.stat(temp)) throw new Error('技能暂存目录已存在');
        await host.mkdir(temp);
        const staged = host.joinPath(temp,'new'), old = host.joinPath(temp,'old');
        try {
          if (record.flat) await host.writeFile(staged,files.get(id));
          else {
            for (const [path,data] of files) {
              const file = host.joinPath(staged,...path.split('/'));
              await host.writeFile(file,data);
              if (remote?.executables.has(path)) {
                await host.chmodPackFile(file,0o755);
                executablePaths.push(homeRel(host.joinPath(dest,...path.split('/'))));
              }
            }
            if (remote) {
              const origin = {repo:remote.repository+(remote.subdir ? '#/'+remote.subdir : ''),commit:remote.commit,ref:remote.ref,mode:remote.mode,digest:remote.digest};
              await host.writeTextFile(host.joinPath(staged,'.dsh-skill.json'),JSON.stringify(origin,null,2)+'\n');
              item.commit = remote.commit;
            }
          }
          const wasPresent = !!await host.stat(dest);
          if (wasPresent) await host.move(dest,old);
          try { await host.move(staged,dest); }
          catch (e) { if (wasPresent) await host.move(old,dest); throw e; }
          if (profileSlot) scopedSkillProfiles.add(profileSlot);
        } finally { await host.rm(temp,{recursive:true,force:true}); }
      };
      if (stat && profileSlot) {
        const keep = preserve;
        preserve = async () => { await keep(); scopedSkillProfiles.add(profileSlot); };
      }
    }
    const decision = opts.decisions?.[item.key];
    item.skipped = decision === 'skip' || decision === 'keep' || item.missingCredentials.length > 0 ||
      ((duplicate || item.conflict && !opts.staging) && decision !== 'replace');
    actions.push({item,run,preserve,mcp});
  }
  const verifyMcp = await checkMcpInstall(host,opts,actions);
  return {
    items:actions.map(a=>a.item), preservePaths, directories, executablePaths, verifyMcp,
    async apply() {
      for (const {item,run,preserve} of actions) {
        if (item.skipped) { await preserve(); continue; }
        await run();
      }
      for (const profile of scopedSkillProfiles) {
        const patch = host.joinPath(home,'profiles',profile,'cordis.patch.yml');
        const existing = await host.readTextFile(patch);
        await host.writeTextFile(patch,registerScopedSkillPatch(existing,profile));
      }
    },
    async commitCredentials() {
      for (const {record,values,expected,actualPatch} of pending) {
        const raw = await host.readTextFile(actualPatch);
        if (!hasInstalledConfig(raw,record,expected)) continue;
        for (const ref of Object.keys(values)) {
          const value = values[ref];
          const existing = await existingSecret(host,realHome,opts,ref);
          if (existing === value) continue;
          if (existing) throw new Error('凭据已在预览后更改，请重新预览：'+ref);
          if (typeof value !== 'string' || !/^[\x21-\x7e]+$/.test(value)) throw new Error('凭据含无效字符：'+ref);
          const dest = host.joinPath(realHome,'.credentials.yaml');
          const store = openCredentialStore(await host.readTextFile(dest));
          store.setIn(['refs',ref],value);
          if (typeof host.writeCredentialStore !== 'function') throw new Error('宿主无法安全保存凭据');
          await host.writeCredentialStore(dest,store.toString());
        }
      }
    },
  };
}
