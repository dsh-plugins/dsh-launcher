// 仅把可直接交给 DSH 凭据模板解析器的参数转换为引用。遇到脚本或不透明的端点时整条 MCP 留在选择列表外。
const secretKey = /api[-_.]?key|secret|pass(?:word|wd)?|token|credential|auth(?:orization)?|cookie|signature|private[-_.]?key|^(?:sig|key|pwd|user|u)$/i;
const token = /(?:\b(?:Bearer|Basic)\s+|\bsk-[\w-]{8,}|\bgh[pousr]_|-----BEGIN .*PRIVATE KEY)/i;
const reference = /^\$\{credential:([A-Za-z_][A-Za-z_0-9]*)\}$/;
const flag = /^(--?[A-Za-z][\w.-]*=)([\s\S]*)$/;
const envAssignment = /^([A-Za-z_][\w]*=)([\s\S]*)$/;
const header = /^([A-Za-z][A-Za-z0-9-]*:[ \t]*)([\s\S]*)$/;
const headerFlag = /^(?:-H|--header|--headers)$/i;
const urlStart = /^[A-Za-z][\w+.-]*:\/\//;
const unsupported = () => { throw new Error('MCP 传输字段含无法安全移植的脚本、参数或地址'); };
const optionName = (key) => key.replace(/^-+|=$/g,'');

function checkUrl(value) {
  let url;
  try { url = new URL(value); } catch { unsupported(); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.hash ||
      /\$\{|[\s\\]/.test(value)) unsupported();
  // 签名查询、回调和各类私有连接串无法在不暴露凭据的同时向用户展示真实目的地。
  if ([...url.searchParams].some(([k,v]) => !(
    /^(?:v|version|page|limit|offset)$/i.test(k) && /^[\d.]{1,16}$/.test(v) ||
    /^(?:lang|locale|format)$/i.test(k) && /^[a-z]{2,8}(?:[-_][a-z]{2,8})?$/i.test(v)
  ))) unsupported();
  let path;
  try { path = decodeURIComponent(url.pathname); } catch { unsupported(); }
  if (token.test(path) || /\b(?:hooks?|webhooks?|services)\/.{12,}|[\w-]{28,}/i.test(path)) unsupported();
}

function shellSnippet(command, args) {
  const words = [command,...args];
  for (let i = 0; i < words.length; i++) {
    if (typeof words[i] !== 'string') continue;
    const executable = words[i].split(/[\\/]/).at(-1).toLowerCase().replace(/\.(?:exe|cmd|bat)$/, '');
    const rest = words.slice(i+1).filter(s => typeof s === 'string');
    if (/^(?:sh|bash|dash|zsh|ksh|fish|ash|cmd|pwsh|powershell)$/.test(executable) &&
        rest.some(s => /^(?:-[a-z]*c[a-z]*|\/[ck]|-command(?:withargs)?|-e(?:nc(?:oded(?:command|arguments))?)?)$/i.test(s))) return true;
    if (/^(?:node|deno|bun|python[\d.]*|ruby|perl|php)$/.test(executable) &&
        rest.some(s => /^(?:-[cep]|--eval(?:=|$)|--print(?:=|$)|eval$)/i.test(s))) return true;
  }
  return false;
}

function readPrefix(prefix) {
  let rest = prefix;
  const assigned = (flag.exec(rest) || envAssignment.exec(rest));
  if (assigned) {
    if (!secretKey.test(optionName(assigned[1])) && !headerFlag.test(assigned[1].slice(0,-1))) return false;
    rest = assigned[2];
  } else if (rest.startsWith('-H')) rest = rest.slice(2);
  const pair = header.exec(rest);
  if (pair) rest = pair[2];
  return rest === '' || /^(Bearer|Basic) $/i.test(rest);
}

export function validMcpArgumentPrefix(prefix) {
  return typeof prefix === 'string' && prefix.length <= 160 && !/[\r\n]/.test(prefix) && readPrefix(prefix);
}

function maskArg(arg, previous, ref, refs, archived) {
  if (archived && arg && typeof arg === 'object' && !Array.isArray(arg) && arg.$credential) {
    if (arg.prefix !== undefined && !validMcpArgumentPrefix(arg.prefix)) unsupported();
    return arg;
  }
  if (typeof arg !== 'string' || /[\0\r\n]/.test(arg)) unsupported();
  const inline = flag.exec(arg) || envAssignment.exec(arg);
  const attachedHeader = !inline && /^-H[A-Za-z][A-Za-z0-9-]*:/.test(arg);
  let prefix = inline?.[1] || (attachedHeader ? '-H' : '');
  let value = inline?.[2] ?? (attachedHeader ? arg.slice(2) : arg);
  const takesSecret = name => name.startsWith('-') && !name.includes('=') && secretKey.test(optionName(name));
  const previousName = typeof previous === 'string' ? previous : '';
  const isHeader = attachedHeader || headerFlag.test(inline ? inline[1].slice(0,-1) : previousName);
  const pair = header.exec(value);
  let hide = !!inline && secretKey.test(optionName(inline[1])) || !inline && takesSecret(previousName);
  if (isHeader || pair && secretKey.test(pair[1].split(':')[0])) {
    if (!pair) unsupported();
    prefix += pair[1];
    value = pair[2];
    hide = true;
  }
  if (urlStart.test(value)) checkUrl(value);
  if (/^(?:Bearer|Basic)\s/i.test(value)) {
    const scheme = /^(?:Bearer|Basic) /i.exec(value);
    if (!scheme) unsupported();
    prefix += scheme[0];
    value = value.slice(scheme[0].length);
    hide = true;
  }
  const native = reference.exec(value);
  if (hide || native || token.test(value)) {
    if (archived || !validMcpArgumentPrefix(prefix)) unsupported();
    const key = native?.[1] || ref;
    refs.add(key);
    return prefix ? {$credential:key,prefix} : {$credential:key};
  }
  if (/\$\{|[;&|]\s*(?:\w|\$)|(?:https?|ssh|postgres(?:ql)?):\/\//i.test(value) && !urlStart.test(value) ||
      /(?:^|\s)--?[\w.-]*(?:key|pass|token|secret|auth|cookie)[\w.-]*(?:=|\s)/i.test(value)) unsupported();
  return arg;
}

export function portableMcpTransport(config, id, refs, archived = false) {
  if (!config || Array.isArray(config) || typeof config !== 'object') unsupported();
  const result = {...config};
  if (config.oauth !== undefined && (config.transport !== 'streamable-http' ||
      !/^mcp-client\/[a-z][a-z0-9-]*$/.test(config.oauth?.credentialKey))) unsupported();
  if (config.enabled !== undefined && typeof config.enabled !== 'boolean') unsupported();
  if (config.url !== undefined) {
    if (typeof config.url !== 'string') unsupported();
    checkUrl(config.url);
  }
  if (config.command !== undefined && (typeof config.command !== 'string' ||
      /[;&|\r\n]|\$\{|\s--?[\w-]+(?:\s|=|$)/.test(config.command))) unsupported();
  if (config.args !== undefined) {
    if (!Array.isArray(config.args) || shellSnippet(config.command || '',config.args)) unsupported();
    result.args = config.args.map((arg,index) => maskArg(arg,config.args[index-1],id+'_ARG_'+index,refs,archived));
  }
  return result;
}
