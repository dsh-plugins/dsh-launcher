import { readMcpComposition } from './mcp-composition.js';
import { findLauncherInstallAnchor } from './discovery.js';
import { baselineOwnsMcp, mcpClientInPatch } from './extension-patch.js';

const client = '@deepseek-ai/dsh-mcp-client';
const notice = (item,message) => { item.notice = item.notice ? item.notice+'。'+message : message; };

export async function checkMcpInstall(host,opts,actions) {
  const candidates = actions.filter(a=>a.mcp);
  if (!candidates.length) return async () => {};
  const home = opts.credentialHome || opts.home;
  const installAnchor = opts.installAnchor === undefined
    ? await findLauncherInstallAnchor(host,home,opts.manifest.dshVersion)
    : opts.installAnchor;
  const whole = opts.manifest.type === 'dshhome';
  const profilesRoot = host.joinPath(home,opts.profilesDir || 'profiles');
  const profiles = new Set(whole ? Object.keys(opts.manifest.profiles) : [opts.profileName]);
  if (whole && candidates.some(a=>a.mcp.scope === null)) {
    for (const entry of await host.readdir(profilesRoot) || []) if (entry.type === 'dir') profiles.add(entry.name);
  }
  const targetProfiles = mcp => mcp.scope === null ? profiles : [mcp.scope];
  const layer = mcp => mcp.scope === null ? 'home' : 'profile';
  const scopesOverlap = (a,b) => a.scope === null || b.scope === null || a.scope === b.scope;
  const sameIdentity = (a,b) => a.server === b.server || a.oauthKey && a.oauthKey === b.oauthKey;
  const chosen = [];
  const priority = action => opts.decisions?.[action.item.key] === 'replace' ? 0 : 1;
  async function composition(name,final,anchor = installAnchor) {
    const profileDir = !whole && (opts.credentialProfileDir || !opts.staging && opts.profileDir) || host.joinPath(profilesRoot,name);
    const unit = whole ? opts.manifest.profiles[name] : opts.manifest;
    return readMcpComposition(host,{home,profileDir,installAnchor:anchor,overlayPaths:opts.overlayPaths,
      bundles:final ? undefined : unit?.bundles});
  }
  const views = new Map();
  for (const name of profiles) views.set(name,await composition(name,false));
  for (const action of [...candidates].sort((a,b)=>priority(a)-priority(b))) {
    const {mcp,item} = action;
    const choice = opts.decisions?.[item.key];
    const scopes = targetProfiles(mcp);
    let collision = false, foreign = false;
    const unresolved = new Set();
    for (const profile of scopes) {
      const view = views.get(profile);
      for (const reason of view.unresolved) unresolved.add(reason);
      for (const row of view.rows.filter(row=>row.id === mcp.id)) {
        if (row.scope !== layer(mcp) || row.name !== client || row.configScope !== layer(mcp)) foreign = true;
      }
      if (view.servers.some(b=>b.owner === mcp.owner && b.server !== mcp.server && !mcp.owned)) foreign = true;
      for (const binding of view.servers.filter(other=>sameIdentity(mcp,other))) {
        const local = binding.scope === layer(mcp);
        if (local && binding.owner === mcp.owner && binding.rowScope === layer(mcp)) {
          if (!mcp.owned) collision = true;
        } else if (local && mcp.legacy && binding.owner.startsWith('manager:') && binding.server === mcp.server) {
          const owner = binding.owner.slice(8);
          const owned = await baselineOwnsMcp(host,mcp.tree,owner,mcp.server);
          if (!owned) {
            collision = true;
            if (choice === 'replace') mcp.replaceOwner = owner;
          }
        } else foreign = true;
      }
    }
    if (unresolved.size) notice(item,'部分 MCP 配置需在依赖安装后核验：'+[...unresolved].join('、'));
    if (foreign || collision) {
      item.conflict = true;
      item.canReplace = !foreign;
      notice(item,foreign ? '服务名称、OAuth 记录或配置行已由其他来源使用，请改名或跳过' : '目标补丁已有同名 MCP 服务，可保留或替换该项');
      if (foreign && choice === 'replace') throw new Error('无法覆盖其他来源或另一作用域的 MCP：'+mcp.server);
      if (foreign || choice !== 'replace') item.skipped = true;
    }
    if (item.skipped) continue;
    const other = chosen.find(a=>scopesOverlap(mcp,a.mcp) && (mcp.id === a.mcp.id || sameIdentity(mcp,a.mcp)));
    if (other) {
      item.conflict = item.skipped = true;
      notice(item,'包内另一个已选条目使用相同名称或 OAuth 记录');
      if (choice === 'replace' && priority(other) === 0) throw new Error('所选 MCP 条目使用相同名称或 OAuth 记录：'+mcp.server);
    } else chosen.push(action);
  }

  return async ({allowUnresolved=false,finalInstallAnchor}={}) => {
    const cache = new Map();
    for (const {item,mcp} of chosen) {
      if (item.skipped) continue;
      const local = mcpClientInPatch(await host.readTextFile(mcp.actualPatch),mcp.id);
      if (!local && mcp.owned) {
        item.skipped = true;
        notice(item,'三方更新保留了本地删除，此服务未启用');
        continue;
      }
      const expected = local ? {server:local.config?.serverName,oauthKey:local.config?.oauth?.credentialKey,enabled:local.disabled !== true} : mcp;
      if (local && (expected.server !== mcp.server || expected.enabled !== mcp.enabled)) {
        notice(item,'三方更新采用了该客户端的本地配置');
      }
      for (const name of targetProfiles(mcp)) {
        if (!cache.has(name)) cache.set(name,await composition(name,true,finalInstallAnchor || installAnchor));
        const view = cache.get(name);
        if (view.unresolved.length) {
          const missing = new Set(view.missingBundles ?? []);
          const pending = view.unresolved.every(id=>missing.has(id) &&
            (allowUnresolved || !(finalInstallAnchor || installAnchor) && id.startsWith('@deepseek-ai/')));
          if (!pending) {
            throw new Error('无法核验 '+name+' 的 MCP 配置：'+view.unresolved.join('、'));
          }
          notice(item,'已写入客户端，尚无法定位的宿主依赖须在实际启动环境核验');
        }
        const rows = view.rows.filter(r=>r.id === mcp.id);
        const names = view.servers.filter(b=>sameIdentity(expected,b));
        if (rows.length !== 1 || rows[0].name !== client || rows[0].scope !== layer(mcp) || rows[0].configScope !== layer(mcp) ||
            names.length !== 1 || names[0].owner !== mcp.owner || names[0].enabled !== expected.enabled) {
          throw new Error('MCP 客户端被其他配置覆盖、同名或未按预期启用：'+mcp.server+'（'+name+'）');
        }
      }
    }
  };
}
