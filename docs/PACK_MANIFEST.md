# DSH 整合包 Manifest 规范

启动器**导出**仍为 **manifestVersion 4** + **`.dspack`（pack-structure v2）**；**导入**兼容：manifestVersion 2-5（含 v5 的 `profile` / `dshhome` 两种形态）与 `.dspack` 容器 v2 / v3，以及旧格式 `.tgz`（pack-structure v1）+ manifestVersion 2 / 3。

## manifestVersion 5 与 pack-structure v3（导入）

v5 是统一版本，`type` 分两种形态，由 `.dspack` v3 容器（根 `dspack.json` 为 `{"format":"dspack","version":3}`）承载；manifest v5 必须搭配 v3 容器（配对校验）。

- **`type: "profile"`**：v4 契约 + 可选 `profileName`；容器可额外携带 `home/` 目录（`home/xxx` 安装时覆盖到 `$DSH_HOME/xxx`，用于随包分发全局 skill / `.agent-presets` / `AGENTS.md`）。
- **`type: "dshhome"`**：整个 `$DSH_HOME` 快照。新增字段：`defaultProfile`（必需）、`profiles`（必需，`name → { bundles, dependencies, patch? }`，至少 1 个且不得含 `web` / `headless`）、`presets`（可选索引）、`skills`（可选，`{ path, sha256?, size?, urls? }`，重技能按需下载）、`instructions`（可选，缺省 `AGENTS.md`）。容器内 `overrides/` 平铺到 home 根（`overrides/profiles/<name>/` 落对应 profile，其余落 home 根）。

dshhome 导入流程：校验 → 确保 `dshVersion` 已安装（未装自动安装）→ 新建专用 HOME 与 web/headless 基线 → 逐 profile 落盘 + `pnpm install`（package.json 由 manifest 权威重建）→ home 级 overrides 落盘 → `files[]` / 重 `skills[]` 下载校验 → 注册实例并设 `defaultProfile` 为默认。**任一失败回滚**：整个新建 HOME 目录与记录一并删除。dshhome 形态仅支持新建实例（不能导入进现有实例）。

## 容器：`.dspack`（pack-structure v2）

`.dspack` 是**标准 ZIP**（文件头即 `PK\x03\x04`，压缩软件可直接打开），内部布局：

```
<name>-<version>.dspack
└── ZIP 内:
    ├── dspack.json         # ★ 容器标记：{"format": "dspack", "version": 2}
    ├── manifest.json       # ★ 索引（manifest v4）
    ├── package.json        # 可选快照（导入时由 manifest 权威重建）
    ├── pnpm-workspace.yaml # 可选
    ├── pnpm-lock.yaml      # 可选（导入时优先 --frozen-lockfile）
    └── overrides/          # 用户文件，导入时整文件覆盖到 profile 根
        └── cordis.patch.yml
```

导入端判定：按 ZIP 打开 → 读根 `dspack.json`（缺失 / `format` ≠ `"dspack"` / `version` 不在 `2-3` → 拒载）→ 读 `manifest.json` 校验版本与容器配对。

`overrides/` 是文件级复制替换（同名盖掉），不做字段级合并；真正的「合并式覆盖」由 `cordis.patch.yml` 补丁层在安装后完成。

## manifestVersion 4

v4 在 v3 全部硬约束（依赖坐标钉死精确版本 / commit sha、`dshVersion` 精确、`displayName` / `description` 多语言）之上新增：

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `manifestVersion` | number | ✅ | 固定为 `4` |
| `type` | string | ✅ | `"profile"`（现行唯一值）；`"collection"` 预留（导入报「暂未支持」）；缺省按 `"profile"` 兜底 |
| `files` | array | 否 | 重内容下载清单，条目为 `{ path, sha256, size, urls[] }`，见下 |

### files[] 下载清单

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `path` | string | ✅ | 安装后落在 profile 内的相对路径（`/` 分隔） |
| `sha256` | string | ✅ | 64 位十六进制哈希 |
| `size` | number | ✅ | 字节数，正整数 |
| `urls` | string[] | ✅ | 下载源（可多个镜像，依次尝试） |

`files[]` 只承载模型、数据、非 npm/git 二进制等重内容；常规插件/依赖仍走 `bundles` + `dependencies`。若 `files[].path` 与 `overrides/` 内文件冲突，以 `files[]` 下载产物为准。

### 示例

```json
{
  "manifestVersion": 4,
  "type": "profile",
  "name": "all-about-whales",
  "version": "1.0.0",
  "displayName": {
    "en-US": "All About Whales",
    "zh-CN": "大肥鱼套装"
  },
  "description": {
    "en-US": "Make your DSH smell like big fat whales (beautify webUI with DeepSeek mascot theme)",
    "zh-CN": "让你的DSH充满大肥鱼的味道（用DeepSeek吉祥物主题美化webUI）"
  },
  "author": "hxh230802",
  "icon": "",
  "dshVersion": "0.1.1-rc.2",
  "profileName": "all-about-whales",
  "bundles": [
    "@deepseek-ai/dsh-base",
    "@deepseek-ai/dsh-web-app",
    "dafy-whale-theme",
    "dsh-whale-widget",
    "dsh-reasoning-effort",
    "dsh-pet"
  ],
  "dependencies": {
    "github:DViridescent/dafy-whale-theme": "99e8c57",
    "dsh-pet": "0.2.0",
    "github:HanaAyane/dsh-reasoning-effort": "83bc8c5",
    "github:MeteorNOX/DeepSeek-Balance-Whale-Widget": "4448c61"
  },
  "files": [
    {
      "path": "data/models/whale-onboard.bin",
      "sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
      "size": 5242880,
      "urls": [
        "https://github.com/hxh230802/dsh-modpacks/releases/download/all-about-whales-1.0.0/whale-onboard.bin"
      ]
    }
  ]
}
```

v4 的 `dependencies` 转换规则与 v3 相同：

- `"dsh-pet": "0.2.0"` → `"dsh-pet": "0.2.0"`（精确版本）
- `"github:owner/repo": "<sha>"` → `"repo": "github:owner/repo#<sha>"`
- `"github:owner/repo#path:/pkg": "<sha>"` → `"pkg": "github:owner/repo#<sha>&path:pkg"`

## 旧格式：`.tgz` + manifestVersion 3 / 2（兼容导入）

旧整合包是 `.tgz` 文件，压缩包**根目录**扁平放置：

| 文件 | 必需 | 说明 |
| --- | --- | --- |
| `manifest.json` | ✅ | 整合包元数据与依赖坐标（manifestVersion 3） |
| `package.json` | ✅ | 可直接 `pnpm install` 的 profile 清单（由 `dependencies` 转换而来） |
| `cordis.patch.yml` | 否 | profile 的 patch 层；缺失时回退到 manifest 的 `patch` 字段 |
| `pnpm-lock.yaml` | 否 | 锁定传递依赖版本（导入时优先 `--frozen-lockfile`） |
| `pnpm-workspace.yaml` | 否 | profile 的 pnpm 设置（hoist、allowBuilds 等） |

### manifestVersion 3 字段定义

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `manifestVersion` | number | ✅ | 固定为 `3` |
| `name` | string | ✅ | 整合包标识（kebab-case），也是打包文件名前缀 |
| `version` | string | ✅ | 整合包自身版本号（semver） |
| `displayName` | string \| object | 否 | 显示名。字符串，或以语言代码为键的 map（如 `{"zh-CN": "...", "en-US": "..."}`)；导入时按当前界面语言作为默认实例名 |
| `description` | string \| object | 否 | 描述，形式同 `displayName` |
| `author` | string | 否 | 作者 |
| `icon` | string | 否 | 图标（URL 或归档内相对路径） |
| `dshVersion` | string | 否 | **固定的精确版本号**（如 `0.1.1-rc.2`)，即导出该整合包时所用的 DSH 版本；导入时安装该版本。缺省时使用本机最新已装版本 |
| `profileName` | string | 否 | 导入时创建的 profile 名；缺省为 `pack`（保持 `web` profile 干净） |
| `bundles` | string[] | ✅ | profile 的 `dsh.profile.bundles` 层栈（按序挂载） |
| `dependencies` | object | ✅ | **坐标 → 固定版本**：npm 包为 `"包名": "精确版本"`;git 包为 `"github:owner/repo": "commit sha"`，monorepo 子目录用 `"github:owner/repo#path:/子目录"` |
| `patch` | string | 否 | `cordis.patch.yml` 的内联内容（v4 起由 `overrides/cordis.patch.yml` 携带，文件优先） |

### manifestVersion 2（兼容）

v2 是 [ModPack-CLI](https://github.com/DSH-PackForge/ModPack-CLI) 写出的原始格式，与 v3 的差异：

| 字段 | v2 行为 |
| --- | --- |
| `displayName` / `description` | 仅字符串 |
| `dshVersion` | semver 范围（如 `>=0.1.0`)；导入时取其下限版本 |
| `dependencies` | 值为 pnpm 原始 spec(`^0.2.0`、`git+https://github.com/owner/repo.git` 等），键为包名；导入时原样透传 |

## 导入行为

1. 校验容器（`.dspack` 校验根 `dspack.json`；旧 `.tgz` 直接解包）→ 读 `manifest.json`；`.dspack` 要求 `manifestVersion === 4` 且 `type` 为 `profile`（`collection` 报「暂未支持」）。
2. 读取 `manifest.json` 后弹出确认框：实例名（默认当前语言的 `displayName`)、profile 名（默认 `profileName`，缺省 `pack`)。
3. 为整合包新建实例与专属 DSH_HOME（或导入到同版本线的现有实例）;`web` profile 保持纯净，整合包内容只进入 pack profile。
4. 安装 `dshVersion` 指定的 DSH 版本（未安装则自动下载；GitHub-only 标签走源码构建）。
5. 写入 `package.json` / `cordis.patch.yml` / `pnpm-lock.yaml` / `pnpm-workspace.yaml` 后执行 `pnpm install`（优先 `--frozen-lockfile`，失配时回退普通安装）。
6. `.dspack` 的 `overrides/` 覆盖到 profile 根（文件级替换）。
7. 逐条 `files[]` 下载到 `path`，逐个校验 `sha256` + `size`；任一失败删除已下文件并整体回滚。
8. pack profile 设为该实例的默认 profile。
