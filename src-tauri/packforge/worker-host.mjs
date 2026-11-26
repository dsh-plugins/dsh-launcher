import fs from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { stripVTControlCharacters } from 'node:util';
import { NodeHost } from './vendor/host-node/src/index.js';

const downloadLimit = 512 * 1024 * 1024;

function terminate(child) {
  if (!child.pid) return;
  if (process.platform === 'win32') {
    const system = process.env.SystemRoot || 'C:\\Windows';
    const killer = spawn(path.join(system, 'System32', 'taskkill.exe'),
      ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    killer.on('error', () => child.kill());
    killer.on('exit', () => { if (child.exitCode == null) child.kill(); });
  } else {
    try { process.kill(-child.pid, 'SIGKILL'); } catch (error) {
      if (error.code !== 'ESRCH') child.kill();
    }
  }
}

async function run(program, args, { cwd, env, timeoutMs, signal, pnpm = false }) {
  signal.throwIfAborted();
  return new Promise(resolve => {
    const child = spawn(program, args, {
      cwd, env, windowsHide: true, shell: false, detached: process.platform !== 'win32',
      stdio: ['ignore', 'pipe', pnpm ? 'pipe' : 'ignore'],
    });
    const output = { stdout: '', stderr: '' };
    let interrupted;
    const stop = reason => {
      interrupted ??= reason;
      terminate(child);
    };
    const onAbort = () => stop('已取消整合包操作');
    const timer = setTimeout(() => stop('整合包子进程执行超时'), timeoutMs);
    signal.addEventListener('abort', onAbort, { once: true });
    for (const stream of ['stdout', 'stderr']) {
      child[stream]?.setEncoding('utf8').on('data', chunk => {
        const text = output[stream] + chunk;
        if (pnpm) {
          output[stream] = text.slice(-64 * 1024);
        } else if (text.length > 1024 * 1024) {
          stop('Git 输出超过大小限制');
        } else {
          output[stream] = text;
        }
      });
    }
    child.once('error', () => { interrupted ??= '无法启动整合包子进程'; });
    child.once('close', status => {
      clearTimeout(timer);
      signal.removeEventListener('abort', onAbort);
      resolve({ status: interrupted ? null : status, error: interrupted, stdout: output.stdout.trim(), stderr: output.stderr.trim() });
    });
  });
}

function pnpmDiagnostics(stdout, stderr) {
  const diagnostics = [];
  for (const line of `${stdout}\n${stderr}`.split(/\r?\n/)) {
    if (!line.startsWith('{')) {
      continue;
    }
    let event;
    try {
      event = JSON.parse(line);
    } catch {
      continue;
    }
    if (event?.level !== 'error' || event.name !== 'pnpm' || typeof event.err?.message !== 'string') {
      continue;
    }
    const { code, message } = event.err;
    const prefix = typeof code === 'string' && /^[A-Z][A-Z0-9_]+$/.test(code) ? `[${code}] ` : '';
    // pnpm 的构建错误会把完整脚本命令写入 message，命令参数可能含凭据。
    if (event.stage || code === 'ELIFECYCLE') {
      const pkg = typeof event.pkgname === 'string' && /^(?:@[\w.-]+\/)?[\w.-]+$/.test(event.pkgname) ? event.pkgname : '';
      const stage = typeof event.stage === 'string' && /^[\w:-]{1,40}$/.test(event.stage) ? event.stage : '';
      const target = [pkg, stage].filter(Boolean).join(' ');
      const exit = Number.isInteger(event.errno) ? `（退出码 ${event.errno}）` : '';
      diagnostics.push(`${prefix}运行${target ? ` ${target} ` : ''}脚本失败${exit}`);
      continue;
    }
    diagnostics.push(...`${prefix}${stripVTControlCharacters(message)}`.split(/\r?\n/).filter(line => line.trim()));
  }
  return [...new Set(diagnostics)].slice(0, 16);
}

function inside(parent, entry) {
  const relative = path.relative(parent, entry);
  return relative && !path.isAbsolute(relative) && relative !== '..' && !relative.startsWith('..' + path.sep);
}

function downloadUrl(value) {
  const url = new URL(value);
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.hash) {
    throw new Error('整合包下载地址无效');
  }
  return url;
}

export class LauncherHost extends NodeHost {
  #directories = new Set();
  #options;
  #signal;
  #request;

  constructor(options, signal, request) {
    super();
    this.#options = options;
    this.#signal = signal;
    this.#request = request;
  }

  /** npm 和 Git 子进程不得继承配置中的 Node 加载参数。 */
  #childEnvironment(overrides = {}) {
    const env = { ...process.env, ...overrides };
    if (this.#options.nodeBin && path.isAbsolute(this.#options.nodeBin)) {
      const key = Object.keys(env).find(name => /^path$/i.test(name)) ?? 'PATH';
      env[key] = this.#options.nodeBin + path.delimiter + (env[key] || '');
    }
    for (const name of Object.keys(env)) {
      if (/^(NODE_OPTIONS|NODE_PATH)$/i.test(name)) delete env[name];
    }
    return env;
  }

  async mkdtemp(prefix) {
    const root = this.#options.workspace;
    if (!root || !path.isAbsolute(root) || !(await fs.lstat(root)).isDirectory()) {
      throw new Error('整合包临时目录无效');
    }
    const dir = await fs.mkdtemp(path.join(root, prefix));
    this.#directories.add(dir);
    return dir;
  }

  async writeNewFile(file, bytes) {
    this.#signal.throwIfAborted();
    await fs.writeFile(file, bytes, { flag: 'wx', mode: 0o600 });
  }

  async exec(command, args, opts = {}) {
    if (command !== 'pnpm') throw new Error('整合包工具请求了不支持的命令');
    const pnpm = this.#options.pnpm;
    if (!pnpm || !path.isAbsolute(pnpm) || /\.(?:cmd|bat|ps1)$/i.test(pnpm)) {
      throw new Error('启动器未提供可直接执行的 pnpm');
    }
    const script = /\.[cm]?js$/i.test(pnpm);
    const { status, error, stdout, stderr } = await run(script ? process.execPath : pnpm, [
      ...(script ? [pnpm] : []), ...args, '--ignore-pnpmfile', '--reporter=ndjson',
    ], {
      cwd: opts.cwd, env: this.#childEnvironment(this.#options.pnpmEnv),
      timeoutMs: opts.timeoutMs || 600_000, signal: this.#signal, pnpm: true,
    });
    return { status, error, diagnostics: status === 0 ? [] : pnpmDiagnostics(stdout, stderr) };
  }

  async git(args, opts = {}) {
    const env = this.#childEnvironment({ ...this.#options.pnpmEnv, GIT_TERMINAL_PROMPT: '0', GIT_ALLOW_PROTOCOL: 'https:ssh' });
    return run('git', args, { cwd: opts.cwd, env, timeoutMs: 120_000, signal: this.#signal });
  }

  /** 下载由启动器执行。逐跳重新限制认证，禁止将凭据带至明文或其他源。 */
  async download(input, destination, { headers = {}, redirectHeaders, maxBytes = downloadLimit } = {}) {
    const dest = path.resolve(destination);
    const directory = [...this.#directories].find(dir => inside(dir, dest));
    if (!directory || path.dirname(dest) !== directory || !(await fs.lstat(directory)).isDirectory()) {
      throw new Error('下载目标不在整合包临时目录');
    }
    let url = downloadUrl(input);
    const origin = url.origin;
    for (let i = 0; i <= 8; i++) {
      this.#signal.throwIfAborted();
      const authorization = url.protocol === 'https:' && url.origin === origin
        ? headers.authorization ?? headers.Authorization : undefined;
      const reply = await this.#request({ kind: 'download', url: url.href, path: dest,
        headers: authorization ? { authorization } : {}, maxBytes: Math.min(maxBytes, downloadLimit) });
      if (reply.status === 200) return;
      if (reply.status >= 300 && reply.status < 400 && reply.location) {
        const next = downloadUrl(new URL(reply.location, url).href);
        headers = next.protocol === 'https:' && next.origin === origin && redirectHeaders
          ? await redirectHeaders(next.href) : {};
        url = next;
        continue;
      }
      throw Object.assign(new Error(`整合包下载失败（HTTP ${reply.status || '请求失败'}）`), { status: reply.status });
    }
    throw new Error('整合包下载重定向次数超限');
  }
}
