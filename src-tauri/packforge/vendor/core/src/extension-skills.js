import { parse } from 'yaml';
import { digestFiles, localized, readJson, readTree, relativePath, safePath, segment } from './extension-files.js';

const sha = /^(?:[a-f\d]{40}|[a-f\d]{64})$/i;

function repository(value) {
  const raw = String(value ?? '');
  const [address, fragment = ''] = raw.split('#');
  // SSH 用户名可以发布，HTTP 地址中的认证信息、查询参数不可进入清单。
  let url;
  if (/^git@[A-Za-z0-9.-]+:[A-Za-z0-9._/-]+$/.test(address)) url = address;
  else {
    let parsed;
    try { parsed = new URL(address); } catch { throw new Error('技能仓库地址无效'); }
    if (!['https:','ssh:'].includes(parsed.protocol) || parsed.password || parsed.search ||
      (parsed.protocol === 'https:' && parsed.username)) throw new Error('技能仓库须使用无内嵌凭据的 HTTPS 或 SSH 地址');
    url = parsed.href;
  }
  const subdir = fragment.replace(/^\/|\/$/g, '');
  if (subdir) relativePath(subdir);
  return { repository: url, subdir };
}

export function checkSkillSource(source) {
  const parsed = repository(source.repository);
  const subdir = source.subdir || parsed.subdir;
  if (subdir) relativePath(subdir);
  const ref = source.ref || (source.mode === 'fixed' ? source.commit : 'HEAD');
  if (source.mode === 'fixed') {
    if (!sha.test(ref)) throw new Error('固定技能要求完整的 Git commit SHA');
  } else if (source.mode !== 'track' || typeof ref !== 'string' ||
    !/^[a-zA-Z0-9][a-zA-Z0-9._/-]*$/.test(ref) || /(?:\.\.|\/\/|\/\.|\.lock$|[/.]$)/.test(ref)) {
    throw new Error('跟踪技能需要有效的分支名或标签');
  }
  return { mode: source.mode, repository: parsed.repository, subdir, ref };
}

async function findSkill(host, root, subdir, id) {
  const base = subdir ? await safePath(host, root, subdir) : root;
  if ((await host.readdir(base))?.some(e => e.type === 'file' && e.name === 'SKILL.md')) return base;
  const matches = [];
  async function visit(dir) {
    for (const e of await host.readdir(dir) ?? []) {
      if (e.type !== 'dir' || e.name === '.git' || e.name === 'node_modules') continue;
      if (e.name === id && (await host.readdir(e.abs))?.some(f => f.type === 'file' && f.name === 'SKILL.md')) matches.push(e.abs);
      else await visit(e.abs);
    }
  }
  await visit(base);
  if (matches.length !== 1) throw new Error('仓库内无法唯一定位技能，请指定 subdir：' + id);
  return matches[0];
}

export async function checkoutSkill(host, source, id) {
  const config = checkSkillSource(source);
  if (typeof host.git !== 'function') throw new Error('宿主无法运行 Git');
  const temp = await host.mkdtemp('dspack-skill-');
  try {
    async function git(args) {
      const r = await host.git(args, {cwd:temp});
      if (r.status !== 0) throw new Error('技能仓库操作失败：' + args[0]);
      return r.stdout.trim();
    }
    await git(['init', '--quiet']);
    await git(['config','core.autocrlf','false']);
    await git(['config','core.eol','lf']);
    await git(['fetch','--quiet','--depth=1','--no-tags','--',config.repository,config.ref]);
    const commit = await git(['rev-parse','--verify','FETCH_HEAD^{commit}']);
    if (!sha.test(commit) || (config.mode === 'fixed' && commit.toLowerCase() !== config.ref.toLowerCase())) {
      throw new Error('技能仓库返回的 SHA 与指定版本不符');
    }
    await git(['checkout','--quiet','--detach',commit]);
    const dir = await findSkill(host, temp, config.subdir, id);
    const files = await readTree(host, dir, {rejectLinks:true});
    const tree = await git(['-C',dir,'ls-tree','-r','-z',commit,'--','.']);
    const executables = new Set();
    for (const entry of tree.split('\0')) {
      const tab = entry.indexOf('\t');
      const path = entry.slice(tab+1);
      if (entry.startsWith('100755 blob ') && tab !== -1 && files.has(path)) executables.add(path);
    }
    return { ...config, commit, files, executables, digest: await digestFiles(host, files) };
  } finally { await host.rm(temp, {recursive:true,force:true}); }
}

export async function skillRoots(host,home,profiles,wholeHome) {
  const roots = new Map();
  if (!home) return roots;
  const store = host.joinPath(home,'.dsh-pack','skills');
  const slots = new Set((await host.readdir(store) || []).filter(e=>e.type === 'dir').map(e=>e.name));
  const common = (await host.readdir(home) || []).find(e=>e.name === 'skills');
  if (!wholeHome && profiles.length === 1 && slots.has(profiles[0].name)) {
    roots.set('home',host.joinPath(store,segment(profiles[0].name)));
    return roots;
  }
  if (common?.type === 'dir') roots.set('home',common.abs);
  if (wholeHome) for (const p of profiles) {
    const slot = slots.has(p.name) ? p.name : (p.name === 'desktop' && slots.has('default') ? 'default' : null);
    if (slot) roots.set('profile:'+segment(p.name),host.joinPath(store,slot));
  }
  return roots;
}

export async function discoverSkills(host,root,scope) {
  const result = [];
  for (const e of await host.readdir(root) ?? []) {
    if (e.name.startsWith('.') || e.type === 'symlink') continue;
    if (!(e.type === 'dir' || (e.type === 'file' && e.name.endsWith('.md')))) continue;
    const text = await host.readTextFile(e.type === 'dir' ? host.joinPath(e.abs, 'SKILL.md') : e.abs);
    if (text == null) continue;
    const header = /^---\r?\n([\s\S]*?)\r?\n---/.exec(text);
    let name=e.name,desc='',warning;
    if (header) {
      try {
        const meta = parse(header[1]);
        name = localized(meta?.name,e.name);
        desc = localized(meta?.description,'');
      } catch { warning = '技能头信息无法解析，使用目录名显示'; }
    }
    const entry = { kind:'skill', key:'skill:'+scope+':' + e.name, scope, id:segment(e.name),
      name,desc,flat:e.type === 'file' };
    if (warning) entry.sourceWarning=warning;
    if (e.type === 'dir') {
      let origin;
      try { origin = await readJson(host, host.joinPath(e.abs, '.dsh-skill.json')); }
      catch { entry.sourceWarning = '来源记录无法解析，可内嵌文件或重新指定仓库'; }
      if (origin?.repo) {
        try {
          const parsed = repository(origin.repo);
          entry.source = { ...parsed, commit:sha.test(origin.commit) ? origin.commit : '', ref:origin.ref || origin.tag || 'HEAD' };
          Object.assign(entry, parsed, {commit:entry.source.commit,ref:origin.ref || origin.tag || ''});
        } catch { entry.sourceWarning = '来源地址含凭据或格式不受支持，请填写无凭据的仓库地址'; }
      }
    }
    result.push(entry);
  }
  return result;
}

export async function exportSkill(host, root, candidate, choice) {
  const id = segment(candidate.id);
  const dir = root ? host.joinPath(root,id) : null;
  const exists = dir ? await host.stat(dir) : null;
  let local = null;
  if (exists?.isDirectory) local = await readTree(host,dir);
  if (exists?.isFile) local = new Map([[id,await host.readFile(dir)]]);
  const mode = choice.mode || 'embedded';
  if (mode === 'embedded') {
    if (!local?.size) throw new Error('内嵌技能没有本地文件：' + id);
    return { mode, files:local, digest:await digestFiles(host,local) };
  }
  const source = { ...candidate.source, ...Object.fromEntries(Object.entries(choice).filter(([,v])=>v !== '' && v != null)), mode };
  if (mode === 'fixed') source.ref = choice.ref || source.commit;
  const fetched = await checkoutSkill(host,source,id);
  if (local) {
    let baseline = fetched.digest;
    if (candidate.source?.commit && candidate.source.commit !== fetched.commit) {
      baseline = (await checkoutSkill(host,{...candidate.source,mode:'fixed',ref:candidate.source.commit},id)).digest;
    }
    if (await digestFiles(host,local) !== baseline) throw new Error('本地技能内容与来源不同，请选择内嵌：' + id);
  }
  const {executables, ...metadata} = fetched;
  return { ...metadata, files:null };
}
