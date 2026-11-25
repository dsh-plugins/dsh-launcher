// 工作区配置（.dshpkcfg）：导出工作区的本地快照（规范见 specs/workspace-config/v1.md）。
// 供 GUI / CLI / AI 工具 / DSH 插件共用的读取与「按配置导出」入口，避免各写一份 readTextFile + JSON.parse。
import { packProfile, packHome } from './pack.js';
import { exportRepo } from './repo.js';

/** 读取某个目录下的 .dshpkcfg；不存在/非法 → null。 */
export async function loadWorkspaceConfig(host, dir) {
  if (!dir) return null;
  const raw = await host.readTextFile(host.joinPath(dir, '.dshpkcfg'));
  if (!raw) return null;
  try { return JSON.parse(raw); } catch { return null; }
}

/**
 * 按工作区配置导出：config 打底，overrides 覆盖；按 mode 分流 dspack / repo。
 * @param {Host} host
 * @param {{name:string,dir:string}} profile
 * @param {object} overrides 显式覆盖（CLI flag / AI 工具参数 / GUI 表单）；undefined/null 视为「未设置」不覆盖
 * @returns packProfile 或 exportRepo 的结果
 */
export async function exportFromWorkspace(host, profile, overrides = {}) {
  const cfg = (await loadWorkspaceConfig(host, profile.dir)) ?? {};
  const opts = { ...cfg };
  for (const [k, v] of Object.entries(overrides)) {
    if (v !== undefined && v !== null) opts[k] = v;
  }
  if (opts.mode === 'repo') {
    // repo 形态下 force 等价 replaceRelease（覆盖同版本 release 产物）
    if (opts.force === true) opts.replaceRelease = true;
    return exportRepo(host, profile, opts);
  }
  return packProfile(host, profile, opts);
}

/**
 * 按工作区配置导出 DSH_HOME（dshhome）：config 打底、overrides 覆盖；把 exportContent 布尔开关映射为 exclude 前缀。
 * @param {Host} host
 * @param {{name:string,dir:string}} home
 * @param {object} overrides 显式覆盖（CLI flag / AI 参数 / Remote 请求）
 * @returns packHome 的结果
 */
export async function exportHomeFromWorkspace(host, home, overrides = {}) {
  const cfg = (await loadWorkspaceConfig(host, home.dir)) ?? {};
  const opts = { ...cfg };
  for (const [k, v] of Object.entries(overrides)) {
    if (v !== undefined && v !== null) opts[k] = v;
  }
  // exportContent（{skill,preset,instruction,data}）→ exclude 前缀（未勾选即排除）
  const ec = opts.exportContent;
  if (ec && typeof ec === 'object') {
    const excludes = [];
    if (ec.skill === false) excludes.push('skills/');
    if (ec.preset === false) excludes.push('.agent-presets/');
    if (ec.instruction === false) excludes.push('AGENTS.md');
    if (ec.data === false) excludes.push('data/');
    if (excludes.length) opts.exclude = excludes;
  }
  return packHome(host, home, opts);
}
