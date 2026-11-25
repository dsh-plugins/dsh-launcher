import { validatePluginAssets } from './plugin-metadata.js';
import { validateExtensions } from './extension-validate.js';
// .dspack 容器 = 标准 ZIP（无自定义魔数头，压缩软件可直接打开）。
// 格式识别由根 dspack.json（容器标记）+ 根 manifest.json（manifestVersion/type）共同承担，
// 见 DSH-PackForge/specs/pack-structure/v4.md（统一覆盖 profile 与 dshhome）。
import { zipSync, unzipSync } from 'fflate';

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** 容器标记固定 format 值。 */
export const DSPACK_FORMAT = 'dspack';

/** 容器版本：统一为 4（pack-structure v4，覆盖 profile / dshhome 两种形态）。 */
export const DSPACK_CONTAINER_VERSION = 4;

/** 生成根标记文件 dspack.json 的内容。 */
export function dspackMarker(version) {
  return { format: DSPACK_FORMAT, version };
}

/**
 * 打包：entries 为 { rel: Uint8Array }（rel 用 '/' 分隔）→ 标准 ZIP 字节。
 * 纯 ZIP，不关心标记；调用方（pack.js）负责把 dspack.json / manifest.json 放进 entries。
 * 整包 sha256 覆盖全部 ZIP 字节。
 */
export function buildDspack(entries) {
  return zipSync(entries);
}

/**
 * 解包：标准 ZIP 字节 → { entries, marker }。
 * 非 ZIP（或空内容）抛错；marker 为根 dspack.json（format 必须是 "dspack"），
 * 缺标记时 marker = null（兼容旧包 / 测试包，靠 manifest 识别）。
 * @returns {{ entries: Record<string, Uint8Array>, marker: {format:string,version:number}|null }}
 */
export function parseDspack(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length === 0) {
    throw new Error('不是有效的 .dspack 文件（空内容）');
  }
  let entries;
  try {
    entries = unzipSync(bytes);
  } catch {
    throw new Error('不是有效的 .dspack 文件（无法按 ZIP 解压）');
  }
  const marker = parseMarker(entries);
  let manifest;
  if (entries['manifest.json']) {
    try {
      manifest = JSON.parse(decodeText(entries['manifest.json']));
    } catch {
      throw new Error('manifest.json 不是有效 JSON');
    }
  }
  if (marker && (!Number.isInteger(marker.version) || marker.version < 1 || marker.version > DSPACK_CONTAINER_VERSION)) {
    throw new Error('不支持的 dspack.json 容器版本');
  }
  if ((manifest?.manifestVersion === 6) !== (marker?.version === 4)) {
    throw new Error('manifest v6 必须与容器 v4 配套');
  }
  if (manifest?.manifestVersion === 6) {
    validatePluginAssets(manifest, entries);
    validateExtensions(manifest, entries);
  }
  return { entries, marker };
}

/** 读根 dspack.json：缺失 → null；format 非 "dspack" → 拒载（外来 ZIP / 损坏）。 */
function parseMarker(entries) {
  const raw = entries['dspack.json'];
  if (!raw) return null;
  let m;
  try {
    m = JSON.parse(decodeText(raw));
  } catch {
    throw new Error('dspack.json 不是有效 JSON');
  }
  if (!m || typeof m !== 'object' || Array.isArray(m) || m.format !== DSPACK_FORMAT) {
    throw new Error('dspack.json 不是有效的容器标记（format 必须为 "dspack"）');
  }
  return m;
}

/** 文本 → UTF-8 字节（不落盘用）。 */
export function encodeText(s) {
  return encoder.encode(s);
}

/** UTF-8 字节 → 文本。 */
export function decodeText(u8) {
  return decoder.decode(u8);
}
