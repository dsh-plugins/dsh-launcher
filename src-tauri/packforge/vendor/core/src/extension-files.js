import { isExcluded } from './security.js';

const invalidSegment = /[\\:\x00-\x1f<>|?*"]|[. ]$/;
const reserved = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i;
const forbidden = new Set(['.git', '.dsh-pack', 'node_modules', '.credentials.yaml', 'settings.yaml']);

export function relativePath(path) {
  if (typeof path !== 'string' || !path || path.length > 1024) throw new Error('扩展资源路径无效');
  for (const part of path.split('/')) {
    if (!part || part === '.' || part === '..' || invalidSegment.test(part) || reserved.test(part) ||
        forbidden.has(part.toLowerCase()) || isExcluded(part.toLowerCase())) {
      throw new Error('扩展资源包含禁止的路径片段');
    }
  }
  return path;
}

export function segment(id) {
  if (relativePath(id).includes('/')) throw new Error('扩展资源标识不能包含路径分隔符');
  return id;
}

export function localized(value, fallback) {
  if (value == null) return fallback;
  if (typeof value === 'string') return value;
  if (!value || Array.isArray(value) || typeof value !== 'object' || !Object.entries(value).every(
    ([key, text]) => /^[a-zA-Z0-9_-]+$/.test(key) && typeof text === 'string',
  )) throw new Error('名称和描述必须是文本或语言映射');
  return value;
}

export function expandTemplate(template, profile) {
  if (typeof template !== 'string') throw new Error('paths 需要字符串路径');
  let path = template.replace(/^\.\//, '');
  if (path.startsWith('<profile>/')) path = 'profiles/' + path;
  if (path.includes('<profile>')) {
    segment(profile);
    path = path.split('/').map(part => part === '<profile>' ? profile : part).join('/');
  }
  return relativePath(path.replace(/\/$/, ''));
}

// 读取每级目录项而非仅检查最终拼接路径，避免沿符号链接离开宿主给定的根。
export async function safePath(host, root, rel) {
  relativePath(rel);
  let dir = root;
  for (const part of rel.split('/')) {
    const entries = await host.readdir(dir);
    const entry = entries?.find(e => e.name.toLowerCase() === part.toLowerCase());
    if (entry?.type === 'symlink') throw new Error('扩展资源路径包含符号链接：' + rel);
    dir = host.joinPath(dir, part);
  }
  return dir;
}

export async function readTree(host, root, options = {}) {
  const files = new Map();
  async function visit(dir, prefix) {
    const list = await host.readdir(dir);
    if (!list) throw new Error('无法读取扩展资源目录');
    if (!list.length && options.emptyDirs) options.emptyDirs.push(prefix.replace(/\/$/, ''));
    for (const entry of list) {
      const path = prefix + entry.name;
      if (entry.name === '.git' || entry.name === '.dsh-skill.json' || isExcluded(path.toLowerCase())) continue;
      relativePath(path);
      if (entry.type === 'symlink') {
        if (options.rejectLinks) throw new Error('远程技能含符号链接：' + path);
        continue;
      }
      if (entry.type === 'dir') await visit(entry.abs, path + '/');
      if (entry.type === 'file') {
        const data = await host.readFile(entry.abs);
        if (!data) throw new Error('读取资源失败：' + path);
        files.set(path, data);
      }
    }
  }
  await visit(root, '');
  return files;
}

export async function digestFiles(host, files) {
  const digests = [];
  for (const path of [...files.keys()].sort()) digests.push([path, await host.sha256(files.get(path))]);
  return host.sha256(new TextEncoder().encode(JSON.stringify(digests)));
}

export async function readJson(host, path) {
  const raw = await host.readTextFile(path);
  if (raw == null) return null;
  try { return JSON.parse(raw); } catch { throw new Error('JSON 格式错误：' + path); }
}

export function protectedTarget(rel) {
  const lower = rel.toLowerCase();
  const parts = lower.split('/');
  const rootEntry = parts[0] === 'profiles' ? parts[2] : parts[0];
  return ['cordis.patch.yml','package.json','pnpm-workspace.yaml','pnpm-lock.yaml'].includes(rootEntry) ||
    parts.includes('.dsh-skill.json') || parts[0] === 'attachments' ||
    parts[0] === 'skills' || parts[0] === '.agent-presets' ||
    ['provider.json','mcp.json','path.json','skill.json','plugins.json'].includes(lower);
}
