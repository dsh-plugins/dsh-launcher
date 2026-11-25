const { execFileSync } = require('node:child_process');
const { realpathSync } = require('node:fs');
const { createRequire } = require('node:module');
const { pathToFileURL } = require('node:url');

function activeProvider(entries, id) {
  for (const entry of entries) {
    if (entry.disabled === true) {
      continue;
    }
    if (entry.id === id && entry.name === '@deepseek-ai/dsh-' + id) {
      return entry;
    }
    if (entry.group && Array.isArray(entry.config)) {
      const nested = activeProvider(entry.config, id);
      if (nested) {
        return nested;
      }
    }
  }
}

async function main() {
  const [bin, profile, ...overlays] = process.argv.slice(1);
  const id = process.platform === 'win32' ? 'cua-windows' : 'cua-macos';
  const requireFromCli = createRequire(realpathSync(bin));
  const include = requireFromCli.resolve('@deepseek-ai/cordis-plugin-include');
  const { parseEntryYaml, stringifyEntryYaml } = await import(pathToFileURL(include).href);
  const dump = execFileSync(process.execPath, [
    bin, '--profile', profile, ...overlays.flatMap(file => ['--patch', file]), '--dump-config',
  ], { encoding: 'utf8', timeout: 10000, maxBuffer: 8 * 1024 * 1024, windowsHide: true,
    stdio: ['ignore', 'pipe', 'ignore'] });
  const row = activeProvider(parseEntryYaml(dump) ?? [], id);
  if (!row || (row.config && Object.hasOwn(row.config, 'helperPath'))) {
    process.stdout.write('S');
    return;
  }
  const config = row.config ?? {};
  if (typeof config !== 'object' || Array.isArray(config)) {
    throw new Error();
  }

  // 仅在此时请求助手。显式的 helperPath（包括 !!js）无需访问启动器的数据目录。
  process.stdout.write('N');
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  const helper = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  let merged;
  if (typeof config.__jsExpr === 'string') {
    // 原表达式只在 Cordis 真实的上下文中求值一次，保留动态生成的显式路径。
    const source = JSON.stringify(config.__jsExpr);
    const fallback = JSON.stringify(helper);
    merged = { __jsExpr: '(v => v != null && Object.prototype.hasOwnProperty.call(v, "helperPath") ? v : ({...v, helperPath: ' +
      fallback + '}))(eval(' + source + '))' };
  } else {
    merged = { ...config, helperPath: helper };
  }
  process.stdout.write(stringifyEntryYaml([{ id, name: row.name, config: merged }]));
}

// 解析器的异常可能包含用户配置片段，父进程分别报告失败阶段。
main().catch(() => { process.exitCode = 1; });
