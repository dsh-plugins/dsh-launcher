// 特殊目录识别（整合包「关注 + 展示」用，不影响打包布局）。
// 依据 DSH 目录契约：
//   - skill（dsh-skill-filesystem）：skills/<name>.md（平铺）或 skills/<name>/SKILL.md（目录 bundle）
//   - agent-preset（dsh-agent-presets）：.agent-presets/<id>/agent.cordis.yml（组装文件；目录内还可有 preset.yml、自附 skill 资产）
//   - icon：icons|icon/<…>.<图片扩展名> 或根 logo.<图片扩展名>（manifest.icon 指向的相对路径）
// 这些目录随扫描进 overrides/（skill / agent-preset / icon 都在 profile 根，安装后覆盖回 profile 根）。

const IMG_EXT = /\.(png|jpe?g|webp|ico|svg)$/i;

/**
 * 从 scan.files 里识别特殊目录，返回 { skills, agentPresets, icons }。
 * @param {{rel:string}[]} files scanProfile 的 files（rel 用 '/' 分隔）
 */
export function summarizeSpecial(files) {
  const skills = [];
  const agentPresets = [];
  const icons = [];

  for (const f of files ?? []) {
    const rel = String(f.rel || '').replace(/\\/g, '/');
    if (!rel) continue;
    const seg = rel.split('/');

    if (seg[0] === '.agent-presets') {
      // 以 agent.cordis.yml 判定一个 preset 存在；目录内其它资产（自附 skill 等）不再单独归类
      if (seg.length >= 3 && seg[2] === 'agent.cordis.yml') {
        agentPresets.push({ id: seg[1], file: rel });
      }
      continue;
    }

    if (seg[0] === 'skills') {
      if (seg.length === 2 && seg[1].endsWith('.md')) {
        skills.push({ name: seg[1].slice(0, -3), file: rel });
      } else if (seg.length >= 3 && seg[2] === 'SKILL.md') {
        skills.push({ name: seg[1], file: rel });
      }
      continue;
    }

    if (IMG_EXT.test(rel)) {
      // icons/xxx.png 或 icon/xxx.png（任意深度），或根 logo.png
      if ((seg[0] === 'icons' || seg[0] === 'icon') || (seg.length === 1 && /^logo\./i.test(seg[0]))) {
        icons.push(rel);
      }
    }
  }

  return { skills, agentPresets, icons };
}