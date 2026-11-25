import { isMap, isSeq, parseDocument } from 'yaml';

const missing = Symbol('缺少成员');
const encoder = new TextEncoder();

function plain(value) {
  return value !== null && value !== missing && typeof value === 'object' && !Array.isArray(value);
}

function itemId(value) {
  if (typeof value === 'string') {
    return ['text', value];
  }
  if (plain(value)) {
    for (const field of ['id', 'name', 'key']) {
      if (typeof value[field] === 'string') {
        return [field, value[field]];
      }
    }
    const keys = Object.keys(value);
    if (keys.length === 1) {
      const op = keys[0];
      const payload = value[op];
      if (Array.isArray(payload) && payload.length === 1 && plain(payload[0])) {
        return [op, itemId(payload[0])];
      }
      if (typeof payload === 'string') {
        return [op, payload];
      }
      return ['operation', op];
    }
  }
  return null;
}

function config(path) {
  return /\.(json|ya?ml)$/i.test(path) && !path.endsWith('/pnpm-lock.yaml');
}

function parse(path, text) {
  if (/\.json$/i.test(path)) {
    return { value: JSON.parse(text) };
  }
  const doc = parseDocument(text, { uniqueKeys: true });
  if (doc.errors.length || doc.warnings.length) {
    return null;
  }
  return { value: doc.toJS({ maxAliasCount: 32 }), doc };
}

// 基线仅保存结构和摘要。键名、数组标识与标量值均不作为明文持久化。
async function describe(host, value) {
  const hash = (data) => host.sha256(encoder.encode(JSON.stringify(data)));
  if (plain(value)) {
    const fields = {};
    const names = {};
    for (const key of Object.keys(value).sort()) {
      const id = await hash(['key', key]);
      fields[id] = await describe(host, value[key]);
      names[id] = key;
    }
    const digest = await hash(['map', Object.entries(fields).map(([key, v]) => [key, v.hash])]);
    return { hash: digest, fields, names, value, type: 'map' };
  }
  if (Array.isArray(value)) {
    const fields = {};
    const order = [];
    for (const entry of value) {
      const identity = itemId(entry);
      if (identity === null) {
        return { hash: await hash(['value', value]), value };
      }
      const id = await hash(identity);
      if (Object.hasOwn(fields, id)) {
        return { hash: await hash(['value', value]), value };
      }
      fields[id] = await describe(host, entry);
      order.push(id);
    }
    return { type: 'list', fields, order, value, hash: await hash(['list', order.map(id => [id, fields[id].hash])]) };
  }
  return { hash: await hash(['value', value]), value };
}

function baseline(node) {
  const out = { hash: node.hash };
  if (node.type) {
    out.type = node.type;
    out.fields = Object.fromEntries(Object.entries(node.fields).map(([k, child]) => [k, baseline(child)]));
  }
  if (node.order) {
    out.order = node.order;
  }
  return out;
}

/** 为可按成员合并的配置生成摘要树，无法无损解析的文件按普通文件比较。 */
export async function configBaseline(host, path, text) {
  if (!config(path)) {
    return undefined;
  }
  let parsed;
  try {
    parsed = parse(path, text);
  } catch {
    return undefined;
  }
  if (!parsed || !plain(parsed.value) && !Array.isArray(parsed.value)) {
    return undefined;
  }
  return baseline(await describe(host, parsed.value));
}

function sameOrder(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

// 以选定一侧的顺序为主，另一侧新增的成员仍跟随其前后的原有成员。
function combinedOrder(primary, secondary) {
  const result = [...primary];
  let prev;
  for (const id of secondary) {
    if (!result.includes(id)) {
      result.splice(prev === undefined ? 0 : result.indexOf(prev) + 1, 0, id);
    }
    prev = id;
  }
  return result;
}

function memberChoice(context, trail, label) {
  const key = JSON.stringify([context.path, ...trail]);
  context.fields.push({key, label: label.join(' / ') || context.path});
  return context.resolutions[key] ?? context.choice;
}

function merge(base, local, upstream, context, trail = [], label = []) {
  if (local?.hash === upstream?.hash || upstream?.hash === base?.hash) {
    return local ? local.value : missing;
  }
  if (local?.hash === base?.hash) {
    return upstream ? upstream.value : missing;
  }
  if (local?.type && local.type === upstream?.type && (!base || local.type === base.type)) {
    const values = new Map();
    for (const id of new Set([...Object.keys(local.fields), ...Object.keys(upstream.fields)])) {
      const entry = local.fields[id] || upstream.fields[id];
      const title = local.type === 'map' ? (local.names[id] ?? upstream.names[id]) :
        (entry.value?.id || entry.value?.name || entry.value?.key || '条目 ' + (Math.max(local.order.indexOf(id), upstream.order.indexOf(id)) + 1));
      const value = merge(base?.fields?.[id], local.fields[id], upstream.fields[id], context, [...trail, id], [...label, title]);
      if (value !== missing) {
        values.set(id, value);
      }
    }
    if (local.type === 'map') {
      return Object.fromEntries([...values].map(([id, val]) => [local.names[id] ?? upstream.names[id], val]));
    }
    const common = new Set((base?.order ?? []).filter(id => local.fields[id] && upstream.fields[id]));
    const before = (base?.order ?? []).filter(id => common.has(id));
    const here = local.order.filter(id => common.has(id));
    const next = upstream.order.filter(id => common.has(id));
    const localMoved = !sameOrder(before, here);
    const upstreamMoved = !sameOrder(before, next);
    const orderConflict = localMoved && upstreamMoved && !sameOrder(here, next);
    const orderChoice = orderConflict ? memberChoice(context, [...trail, '@order'], [...label, '排列顺序']) : null;
    const useUpstream = upstreamMoved && (!localMoved || orderChoice === 'upstream');
    const order = useUpstream ? combinedOrder(upstream.order, local.order) : combinedOrder(local.order, upstream.order);
    return order.filter(id => values.has(id)).map(id => values.get(id));
  }
  const selected = memberChoice(context, trail, label) === 'upstream' ? upstream : local;
  return selected ? selected.value : missing;
}

// 尽量复用本地 YAML 节点，保留未编辑配置的注释、引号和集合样式。
function editYaml(doc, node, old, value) {
  if (JSON.stringify(old) === JSON.stringify(value)) {
    return node;
  }
  if (isMap(node) && plain(old) && plain(value)) {
    for (const key of Object.keys(old)) {
      if (!Object.hasOwn(value, key)) {
        node.delete(key);
      }
    }
    for (const [key, next] of Object.entries(value)) {
      node.set(key, Object.hasOwn(old, key) ? editYaml(doc, node.get(key, true), old[key], next) : doc.createNode(next));
    }
    return node;
  }
  if (isSeq(node) && Array.isArray(old) && Array.isArray(value)) {
    const byId = new Map(old.map((v, i) => [JSON.stringify(itemId(v)), i]));
    if (!byId.has('null') && byId.size === old.length) {
      node.items = value.map(v => {
        const i = byId.get(JSON.stringify(itemId(v)));
        return i === undefined ? doc.createNode(v) : editYaml(doc, node.items[i], old[i], v);
      });
      return node;
    }
  }
  return doc.createNode(value);
}

/** 比较两侧配置与上次安装的结构摘要，冲突选择仅覆盖发生冲突的成员。 */
export async function mergeConfig(host, path, baseTree, localText, nextText, choice, resolutions = {}) {
  if (!baseTree || !config(path)) {
    return null;
  }
  let local;
  let next;
  try {
    local = parse(path, localText);
    next = parse(path, nextText);
  } catch {
    return null;
  }
  if (!local || !next) {
    return null;
  }
  const [here, upstream] = await Promise.all([describe(host, local.value), describe(host, next.value)]);
  const fields = [];
  const value = merge(baseTree, here, upstream, { path, choice, resolutions, fields });
  if (value === missing) {
    return null;
  }
  let text;
  if (local.doc) {
    local.doc.contents = editYaml(local.doc, local.doc.contents, local.value, value);
    text = String(local.doc);
  } else {
    const indent = localText.match(/\n([ \t]+)"/)?.[1] ?? 2;
    text = JSON.stringify(value, null, indent) + '\n';
  }
  return { text, conflicts: fields.length, fields };
}
