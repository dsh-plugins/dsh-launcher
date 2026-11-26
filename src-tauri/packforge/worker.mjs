import fs from 'node:fs/promises';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { installPack, verifyIntegrity } from './vendor/core/src/install.js';
import { inspectPack } from './vendor/core/src/inspect.js';
import { recoverPackUpdate } from './vendor/core/src/update.js';
import { LauncherHost } from './worker-host.mjs';
import { previewIcons, publicManifest, validateArchive } from './worker-archive.mjs';
import { runExport } from './worker-export.mjs';

const maximumLine = 8 * 1024 * 1024;
const compressedLimit = 64 * 1024 * 1024;
const signal = new AbortController();
const pending = new Map();
const input = createInterface({ input: process.stdin, terminal: false, crlfDelay: Infinity });
let nextId = 0, started = false, completed = false;

function send(value) {
  if (!process.stdout.destroyed) process.stdout.write(JSON.stringify(value) + '\n');
}

function cancel() {
  signal.abort(new Error('已取消整合包操作'));
  for (const { reject } of pending.values()) reject(signal.signal.reason);
  pending.clear();
}

function request(message) {
  signal.signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    send({ ...message, id });
  });
}

function secretsIn(value) {
  if (typeof value === 'string' && value.length) return [value];
  if (value && typeof value === 'object') return Object.values(value).flatMap(secretsIn);
  return [];
}

function safeText(value, secrets, max = 900) {
  let text = String(value).split(/\r?\n/)[0];
  for (const secret of secrets) text = text.split(secret).join('[凭据]');
  return text.replace(/((?:https?|socks5h?):\/\/)[^\s@/]+@/gi, '$1[凭据]@')
    .replace(/(https?:\/\/[^\s?#]+)[?#][^\s]*/gi, '$1')
    .replace(/\b(?:Bearer|Basic)\s+[A-Za-z0-9+\/=_-]+/gi, '[凭据]')
    .slice(0, max);
}

function cleanResult(result, secrets) {
  if (!result || typeof result !== 'object') return result;
  if (result.manifest) result.manifest = publicManifest(result.manifest);
  for (const item of result.items ?? []) {
    if (item.destination) item.destination = safeText(item.destination, secrets);
  }
  for (const group of Object.values(result.candidates ?? {})) {
    for (const item of group) {
      for (const key of ['destination', 'endpoint', 'notice']) {
        if (item[key]) item[key] = safeText(item[key], secrets);
      }
    }
  }
  return result;
}

async function snapshot(host, opts) {
  if (!opts.source || !path.isAbsolute(opts.source) || await host.packEntryType(opts.source) !== 'file') {
    throw new Error('启动器未提供经校验的本地整合包');
  }
  if ((await fs.stat(opts.source)).size > compressedLimit) throw new Error('整合包大小超过 64 MiB');
  await verifyIntegrity(host, opts.source, opts);
}

async function operation(kind, raw, secrets) {
  const opts = { ...raw };
  delete opts.baselineOnly;
  const host = new LauncherHost(opts, signal.signal, request);
  if (kind === 'inspectExport' || kind === 'export') return runExport(host, opts, kind === 'inspectExport');
  if (kind === 'recover') {
    if (!opts.home || !path.isAbsolute(opts.home) || ['file', 'symlink', 'other'].includes(await host.packEntryType(opts.home))) {
      throw new Error('启动器未提供有效的 DSH_HOME');
    }
    await recoverPackUpdate(host, opts.home, opts.profilesDir);
    return { recovered: true };
  }
  if (!['inspect', 'resources', 'preview', 'apply'].includes(kind)) throw new Error('不支持的整合包操作');
  await snapshot(host, opts);
  if (kind === 'inspect') {
    const inspected = await inspectPack(host, opts.source, { includeIcons: true,
      validateArchive: (manifest, entries) => validateArchive(host, manifest, entries) });
    if (opts.expectedSha256 && inspected.sha256 !== opts.expectedSha256.toLowerCase()) {
      throw new Error('读取时归档内容已变化，请重新预览');
    }
    return { manifest: inspected.manifest, sha256: inspected.sha256, size: inspected.size,
      containerVersion: inspected.containerVersion, valid: inspected.valid, validation: inspected.validation,
      icons: previewIcons(inspected.icons ?? {}) };
  }

  if (kind === 'apply' && !opts.previewToken) throw new Error('请先确认整合包预览');
  if (kind === 'apply' && !opts.sourceIdentity) throw new Error('缺少整合包来源标识');
  if (opts.previousSource) {
    if (!path.isAbsolute(opts.previousSource) || !opts.previousSha256) throw new Error('旧整合包需要可核验的本地文件和摘要');
    await snapshot(host, { source: opts.previousSource, expectedSha256: opts.previousSha256 });
  }
  const target = opts.home ?? (opts.profilesRoot && path.dirname(opts.profilesRoot));
  if (!target || !path.isAbsolute(target) || ['file', 'symlink', 'other'].includes(await host.packEntryType(target))) {
    throw new Error('整合包目标不是普通目录');
  }
  if (opts.name) opts.targetProfileName ??= opts.name;
  if (opts.credentialEnvironment) {
    const environment = opts.credentialEnvironment;
    opts.credentialEnvironment = key => Object.hasOwn(environment,key) ? environment[key] : null;
  }
  return installPack(host, {
    ...opts, dryRun: kind !== 'apply', listResourcesOnly: kind === 'resources', noInstall: false,
    validateArchive: (manifest, entries) => validateArchive(host, manifest, entries, opts),
    recoverInterrupted: false,
    onProgress: (_stage, message) => {
      signal.signal.throwIfAborted();
      send({ kind: 'progress', message: safeText(message, secrets) });
    },
    beforePublish: kind === 'apply' ? async (details) => {
      signal.signal.throwIfAborted();
      if (opts.publish !== false) await request({ kind: 'publish', ...details });
    } : undefined,
  });
}

async function run(message) {
  const opts = message.options ?? {};
  const secrets = secretsIn(opts.credentials).sort((a,b) => b.length - a.length);
  try {
    const result = await operation(message.operation, opts, secrets);
    send({ kind: 'result', value: cleanResult(result, secrets) });
  } catch (error) {
    send({ kind: 'error', message: safeText(signal.signal.aborted ? signal.signal.reason.message : (error?.message ?? '整合包操作失败'), secrets) });
    process.exitCode = 1;
  } finally {
    completed = true;
    input.close();
    process.stdin.destroy();
  }
}

input.on('line', line => {
  let message;
  try {
    if (line.length > maximumLine) throw new Error();
    message = JSON.parse(line);
    if (!message || typeof message !== 'object') throw new Error();
  } catch {
    if (started) cancel();
    else {
      send({ kind: 'error', message: '整合包进程请求无效' });
      input.close();
      process.stdin.destroy();
      process.exitCode = 1;
    }
    return;
  }
  if (!started) {
    started = true;
    void run(message);
  } else if (message.kind === 'cancel') {
    cancel();
  } else {
    const operation = pending.get(message.id);
    if (!operation) return;
    pending.delete(message.id);
    if (message.error) operation.reject(new Error('启动器未能完成整合包请求'));
    else operation.resolve(message);
  }
});
input.on('close', () => { if (!completed) cancel(); });
process.stdout.on('error', cancel);
process.on('SIGINT', cancel);
process.on('SIGTERM', cancel);
