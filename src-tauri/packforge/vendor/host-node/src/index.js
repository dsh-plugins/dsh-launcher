// Node 宿主：实现 @dsh-packforge/core 的 Host 契约（以 node:fs 等内建能力注入）。
// 供 CLI / Electron / server 使用。
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import http from 'node:http';
import https from 'node:https';
import { applyEntryPatches } from '@deepseek-ai/cordis-plugin-include';
import { execFile, spawn, spawnSync } from 'node:child_process';

export class NodeHost {
  #rootCAs;

  joinPath(...parts) {
    return path.join(...parts);
  }

  /** 以随本工具分发的 Cordis 实现组合独立的补丁副本。 */
  composePackEntries(patches) {
    const result = applyEntryPatches([], patches, () => {});
    return Array.isArray(result) ? result : result.data;
  }

  resolvePath(...parts) {
    return path.resolve(...parts);
  }

  cwd() {
    return process.cwd();
  }

  homedir() {
    return os.homedir();
  }

  env(name) {
    return process.env[name] ?? null;
  }

  basename(abs) {
    return path.basename(abs);
  }

  async readTextFile(abs) {
    try {
      return await fsp.readFile(abs, 'utf8');
    } catch {
      return null;
    }
  }

  async writeTextFile(abs, text) {
    await fsp.mkdir(path.dirname(abs), { recursive: true });
    await fsp.writeFile(abs, text, 'utf8'); // 无 BOM
  }

  async readFile(abs) {
    try {
      return new Uint8Array(await fsp.readFile(abs));
    } catch {
      return null;
    }
  }

  async writeFile(abs, data) {
    await fsp.mkdir(path.dirname(abs), { recursive: true });
    await fsp.writeFile(abs, data);
  }

  async stat(abs) {
    try {
      const s = await fsp.stat(abs);
      return {
        size: s.size,
        isFile: s.isFile(),
        isDirectory: s.isDirectory(),
        isSymbolicLink: s.isSymbolicLink(),
      };
    } catch {
      return null;
    }
  }

  async readdir(abs) {
    try {
      const entries = await fsp.readdir(abs, { withFileTypes: true });
      return entries.map((e) => ({
        name: e.name,
        abs: path.join(abs, e.name),
        type: e.isSymbolicLink() ? 'symlink' : e.isDirectory() ? 'dir' : e.isFile() ? 'file' : 'other',
      }));
    } catch {
      return null;
    }
  }

  async mkdir(abs) {
    await fsp.mkdir(abs, { recursive: true });
  }

  async rm(abs, opts = {}) {
    await fsp.rm(abs, { recursive: opts.recursive !== false, force: opts.force !== false });
  }

  async mkdtemp(prefix) {
    return await fsp.mkdtemp(path.join(os.tmpdir(), prefix));
  }

  async sha256(data) {
    return crypto.createHash('sha256').update(data).digest('hex');
  }

  async sha256File(abs) {
    try {
      return await new Promise((resolve, reject) => {
        const hash = crypto.createHash('sha256');
        const stream = fs.createReadStream(abs);
        stream.on('data', (chunk) => hash.update(chunk));
        stream.on('end', () => resolve(hash.digest('hex')));
        stream.on('error', reject);
      });
    } catch {
      return null;
    }
  }

  async writeCredentialStore(dest, text) {
    await fsp.mkdir(path.dirname(dest), {recursive:true});
    let file, created = false;
    try {
      file = await fsp.open(dest, 'wx+', 0o600);
      created = true;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      file = await fsp.open(dest, 'r+');
    }
    try {
      if (process.platform !== 'win32') await file.chmod(0o600);
      else if (created) {
        const current = spawnSync('whoami.exe', ['/user','/fo','csv','/nh'], {encoding:'utf8',windowsHide:true,timeout:5000});
        const sid = current.stdout?.match(/S-1-(?:\d+-)+\d+/i)?.[0];
        const acl = sid && spawnSync('icacls.exe', [dest,'/inheritance:r','/grant:r','*'+sid+':F'], {windowsHide:true,timeout:5000});
        if (!acl || acl.status !== 0) throw new Error('无法限制凭据文件的 Windows 访问权限');
      }
      await file.truncate(0);
      await file.writeFile(text, 'utf8');
    } finally { await file.close(); }
  }

  async git(args, { cwd } = {}) {
    return new Promise((resolve) => {
      execFile('git', args, {
        cwd, windowsHide: true, timeout: 120000, maxBuffer: 1024 * 1024,
        env: { ...process.env, GIT_TERMINAL_PROMPT: '0', GIT_ALLOW_PROTOCOL: 'https:ssh' },
      }, (error, stdout) => resolve({ status: error ? 1 : 0, stdout: String(stdout).trim() }));
    });
  }

  async exec(cmd, args, opts = {}) {
    // 用异步 spawn（而非 spawnSync）：避免在 Electron 主进程同步阻塞导致界面卡死；
    // stdin 置 ignore 防止 pnpm/git 在无人输入时卡在交互提示；可选 timeoutMs 兜底防永久卡住。
    return await new Promise((resolve) => {
      let child;
      try {
        child = spawn(cmd, args, {
          cwd: opts.cwd,
          stdio: ['ignore', 'inherit', 'inherit'],
          shell: process.platform === 'win32',
          windowsHide: true,
        });
      } catch (err) {
        return resolve({ status: null, error: err.message });
      }

      let settled = false;
      let timer = null;
      const finish = (status, error) => {
        if (settled) return;
        settled = true;
        if (timer) clearTimeout(timer);
        resolve({ status, error });
      };

      if (opts.timeoutMs > 0) {
        timer = setTimeout(() => {
          // 结束整个进程树：Windows 下 shell:true 时 child 是 cmd.exe，需 taskkill /T
          if (child.pid) {
            if (process.platform === 'win32') {
              try { spawnSync('taskkill', ['/pid', String(child.pid), '/T', '/F'], { windowsHide: true }); } catch { /* 忽略 */ }
            } else {
              try { process.kill(child.pid, 'SIGTERM'); } catch { /* 忽略 */ }
            }
          }
          finish(null, `命令超时（${opts.timeoutMs}ms）：${cmd} ${(args ?? []).join(' ')}`);
        }, opts.timeoutMs);
      }

      child.on('error', (err) => finish(null, err.message));
      child.on('close', (code) => finish(code ?? 0, undefined));
    });
  }

  async download(url, destAbs, { headers = {}, redirectHeaders } = {}) {
    try {
      await this.#downloadOnce(url, destAbs, null, headers, redirectHeaders);
    } catch (e) {
      // Windows 上部分站点（如 github.com）的证书链只认系统根 CA，Node bundled CA 认不到时
      // 自动加载系统根 CA 重试一次（信任系统信任存储，而非禁用校验）。
      if (this.#isCertError(e)) {
        const cas = await this.#systemRootCAs();
        if (cas && cas.length) {
          await this.#downloadOnce(url, destAbs, cas, headers, redirectHeaders);
          return;
        }
      }
      throw e;
    }
  }

  async #downloadOnce(url, destAbs, extraCa, headers, redirectHeaders, redirects = 0) {
    await new Promise((resolve, reject) => {
      let u;
      try {
        u = new URL(url);
      } catch {
        return reject(new Error(`无效的下载地址`));
      }
      const lib = u.protocol === 'https:' ? https : u.protocol === 'http:' ? http : null;
      if (!lib) return reject(new Error(`仅支持 HTTP 或 HTTPS`));
      const options = { headers: { ...headers, 'user-agent': 'dspack/0.1.0' } };
      if (extraCa) options.ca = extraCa;
      const req = lib.get(url, options, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          if (redirects >= 8) return reject(new Error('下载重定向次数超限'));
          let next;
          try { next = new URL(res.headers.location, u); } catch { return reject(new Error('下载重定向地址无效')); }
          if (next.username || next.password) return reject(new Error('下载地址不得包含凭据'));
          const sameOrigin = next.origin === u.origin;
          const authorize = sameOrigin ? redirectHeaders : undefined;
          const nextHeaders = authorize ? authorize(next.href) : sameOrigin && next.pathname === u.pathname ? headers : {};
          return resolve(Promise.resolve(nextHeaders).then(scoped =>
            this.#downloadOnce(next.href, destAbs, extraCa, scoped, authorize, redirects + 1),
          ));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(Object.assign(new Error(`下载失败：HTTP ${res.statusCode}`), { status: res.statusCode }));
        }
        const out = fs.createWriteStream(destAbs);
        res.pipe(out);
        out.on('finish', () => out.close(() => resolve()));
        out.on('error', reject);
      });
      req.on('error', reject);
      req.setTimeout(30_000, () => req.destroy(new Error('下载超时（30s）')));
    });
  }

  #isCertError(e) {
    const m = String(e?.code ?? '') + ' ' + String(e?.message ?? '');
    return /UNABLE_TO_VERIFY|SELF_SIGNED|CERT_HAS_EXPIRED|UNABLE_TO_GET_ISSUER|verify the first certificate|ERR_TLS_CERT/i.test(m);
  }

  async #systemRootCAs() {
    if (this.#rootCAs !== undefined) return this.#rootCAs;
    this.#rootCAs = null;
    if (process.platform !== 'win32') return this.#rootCAs;
    try {
      const script = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Get-ChildItem Cert:\\LocalMachine\\Root, Cert:\\CurrentUser\\Root | ForEach-Object { '-----BEGIN CERTIFICATE-----'; [System.Convert]::ToBase64String($_.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert), 'InsertLineBreaks'); '-----END CERTIFICATE-----' }";
      const r = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
        encoding: 'utf8', timeout: 15000, windowsHide: true, maxBuffer: 16 * 1024 * 1024,
      });
      const cas = [...String(r.stdout ?? '').matchAll(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g)].map((m) => m[0]);
      if (cas.length) this.#rootCAs = cas;
    } catch {
      this.#rootCAs = null;
    }
    return this.#rootCAs;
  }

  /** 竞争者分别登记进程和随机标识，崩溃遗留的登记由下一次操作清理。 */
  async withPackLock(directory, action) {
    const locks = path.join(directory, 'locks');
    await fsp.mkdir(locks, { recursive: true, mode: 0o700 });
    const id = String(process.pid) + '-' + crypto.randomUUID();
    const claim = path.join(locks, id);
    await fsp.writeFile(claim, '', { flag: 'wx', mode: 0o600 });
    try {
      for (const peer of await fsp.readdir(locks)) {
        if (peer === id) {
          continue;
        }
        const match = /^([1-9][0-9]*)-[\da-f-]{36}$/.exec(peer);
        if (!match) {
          throw new Error('更新锁目录中存在无法识别的记录');
        }
        try {
          process.kill(Number(match[1]), 0);
        } catch (error) {
          if (error.code === 'ESRCH') {
            await fsp.rm(path.join(locks, peer), { force: true });
            continue;
          }
          if (error.code !== 'EPERM') {
            throw error;
          }
        }
        throw new Error('另一项整合包操作正在使用此 DSH_HOME，请稍后重试');
      }
      return await action();
    } finally {
      await fsp.rm(claim, { force: true });
    }
  }

  /** 更新前必须区分路径缺失、权限错误与符号链接。 */
  /** 返回普通文件的 POSIX 权限；Windows 无法表达 Git 执行位。 */
  async packFileMode(abs) {
    if (process.platform === 'win32') return null;
    const stat = await fsp.lstat(abs);
    if (!stat.isFile()) throw new Error('权限目标必须是普通文件');
    return stat.mode & 0o777;
  }

  /** 仅在具有 POSIX 权限的文件系统设置普通文件的访问位。 */
  async chmodPackFile(abs, mode) {
    if (!Number.isInteger(mode) || mode < 0 || mode > 0o777) throw new Error('无效的文件权限');
    if (process.platform === 'win32') return;
    if (!(await fsp.lstat(abs)).isFile()) throw new Error('权限目标必须是普通文件');
    await fsp.chmod(abs, mode);
  }

  async packEntryType(abs) {
    try {
      const entry = await fsp.lstat(abs);
      if (entry.isSymbolicLink()) {
        return 'symlink';
      }
      return entry.isFile() ? 'file' : entry.isDirectory() ? 'dir' : 'other';
    } catch (error) {
      if (error.code === 'ENOENT') {
        return null;
      }
      throw error;
    }
  }

  /** 回滚时只移除本次创建且仍为空的目录。 */
  async removePackDirIfEmpty(abs) {
    try {
      await fsp.rmdir(abs);
    } catch (error) {
      if (!['ENOENT', 'ENOTEMPTY', 'EEXIST'].includes(error.code)) {
        throw error;
      }
    }
  }

  /** 复制受管文件时保留普通文件权限，凭据备份仅向当前用户开放。 */
  async copyPackFile(from, to, privateCopy = false) {
    await fsp.mkdir(path.dirname(to), { recursive: true, mode: 0o700 });
    await fsp.copyFile(from, to);
    if (privateCopy) {
      await fsp.chmod(to, 0o600);
    }
  }

  async move(from, to) {
    await fsp.mkdir(path.dirname(to), { recursive: true });
    await fsp.rename(from, to);
  }
}
