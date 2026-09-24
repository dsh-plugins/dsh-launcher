# Issue #65 任务树 — 重定向「sessions」添加下拉预设和文件选择器

> 来源：<https://github.com/dsh-plugins/dsh-launcher/issues/65>
> 父议题：<https://github.com/dsh-plugins/dsh-launcher/issues/4>（TODO List）
> 标签：`📋️ TODO` · 状态：OPEN · 作者：Gu-ZT · 创建：2026-09-23
> 核验基线：本地 `D:\DSH\DSH-Launcher` @ `a5a770b`（2026-09-24，分支 `fix/issue-66-plugin-diagnostics-and-compat-launch`）
> 关联前置：#51（DSH_HOME 存储重定向，提交 `5ca1bb7`）· #49（WSL2 HOME 可操作性，`links.rs` 对 WSL 硬拒绝）
> 文档状态：**implemented v3**。既是规划文档，也是执行时回填进度的活文档（勾选框 + §7 回填日志）。v2 于 2026-09-24 完成实现与验收；v3 同日处理 PR #68 独立复核的 F1~F8 全部条目（见 §7.2），文件名按仓库惯例由 `TASK_LIST` 更正为 `TASK_TREE`。

---

## 0. 结论先行

| 项 | 结论 |
| --- | --- |
| 问题是否真实存在 | ✅ **真实**。`InstanceEdit.vue:2153-2159` 的重定向对话框只有一个裸 `a-input` 手填绝对路径，没有预设下拉、没有「浏览…」按钮；`storageTargetPlaceholder` 只给了一个写死的示例路径 |
| 是否从零实现 | ❌ **不是**。底层能力已齐备：`REDIRECTABLE` 白名单、`set_home_link` / `clear_home_link` 命令、`HomeLinkInfo.is_dir` 字段、`@tauri-apps/plugin-dialog`（`dialog:default` 已授权）都已存在，本 issue 只做**对话框交互增强** |
| 本计划的真实目标 | (a) 明确「预设」的语义并落到可枚举的候选集；**(b) 补「浏览…」文件/目录选择器（目录条目走 directory 模式，文件条目走 filters 模式）**；(c) 补齐 mock / 类型 / 双语 locales / 单测 |
| 主要风险 | 🟠 **语义未定**——issue 正文只有一句话，且被重述两遍，「预设」指什么无权威定义（见 §2）；🟡 条目**混合了目录与文件**，选择器必须按 `is_dir` 分支；🟡 路径需满足后端三条硬校验（存在、不在 HOME 内、不是 HOME 内原条目本身），前端不预检会变成裸错误 toast |
| 建议总迭代 | **4 个阶段，约 3~5 轮** agent 迭代（阶段 2 前端改造是主体，其余为约定与验收） |
| 预估改动规模 | 约 **7 个文件，+180 / −25**（对照 #51 的 12 文件 +638 规模，本 issue 是纯增量 UI） |
| 实际改动规模 | **8 个文件，+494 / −11**（含后端新命令与 6 个新单测；前端 201 行） |

### 0.1 一句话路线图

```text
阶段 0 基线确认 → 阶段 1 语义与候选集定稿（含是否需要新后端命令）
   → 阶段 2 对话框改造（下拉预设 + 浏览按钮 + 前端预检）
   → 阶段 3 后端与契约（仅 D1-a 需要） → 阶段 4 mock/双语/单测收尾 → 阶段 5 测试与验收
```

---

## 1. 源码核验明细（证据）

> 以下行号均在 `a5a770b` 上实测，非推测。

### 1.1 现状：重定向对话框长什么样

`src/views/InstanceEdit.vue`：

| 位置 | 内容 |
| --- | --- |
| `:1007-1012` | `storageColumns`：条目 / 目标路径 / 状态 / 操作 四列 |
| `:1014-1019` | `homeLinks` / `homeLinksLoading` / `linkDialogVisible` / `linkEntry` / `linkTarget` / `linkBusy` |
| `:1021-1038` | `loadHomeLinks()` — WSL 直接返回空（前端提前避让，见 `:1023-1029` 注释） |
| `:1040-1044` | `openLinkDialog(link)` — 仅把 `record.target` 回填进 `linkTarget`，**无预设、无浏览** |
| `:1046-1061` | `confirmSetLink()` — `trim()` 后非空即调 `api.setHomeLink`，**无任何前置校验** |
| `:2141-2164` | 对话框模板：`a-form-item` + 单个 `a-input v-model="linkTarget"` + `a-alert` 提示 |

**关键结论**：对话框中「输入目标路径」的位置是 `:2153-2159`，这正是本 issue 要改造的点。

### 1.2 条目白名单：目录与文件混合

`src-tauri/src/links.rs:13-21`：

```rust
const REDIRECTABLE: &[(&str, bool)] = &[
    ("sessions", true),           // 目录
    ("skills", true),             // 目录
    ("attachments", true),        // 目录
    ("storages", true),           // 目录
    ("settings.yaml", false),     // 文件
    (".credentials.yaml", false), // 文件
    ("cordis.patch.yml", false),  // 文件
];
```

→ **目录条目 4 个、文件条目 3 个**。「文件选择器」必须按 `is_dir` 分支，不能一律用目录模式。

### 1.3 契约已具备分支所需字段

`links.rs:30-38` `HomeLinkInfo` 与 `src/api/types.ts:13-21`：

```ts
export interface HomeLinkInfo {
  entry: string
  is_dir: boolean      // ← 前端可直接用它决定选择器模式，无需新字段
  target: string       // 空串 = 未重定向
  active: boolean
}
```

### 1.4 后端硬校验（前端必须预检的三条）

`src-tauri/src/links.rs`：

| 行号 | 校验 | 违反时的错误文案 |
| --- | --- | --- |
| `:170-174` | 目录会被 `create_dir_all` 自动创建；**文件目标必须已存在**（`is_file()`） | `"目标文件不存在"` |
| `:176-189` | 目标不能就是 HOME 内的原条目（如 `<HOME>/sessions` → `<HOME>/sessions`） | `"目标不能就是 HOME 内的原条目"` |
| `:190-198` | 目标不能位于该 DSH_HOME 内部 | `"目标不能位于该 DSH_HOME 内部"` |
| `:51-65` | 该 HOME 下有实例运行时拒绝修改 | `"该 DSH_HOME 下有实例正在运行，请先停止后再修改存储重定向"` |

另：`:134-136` / `:163-165` 对 WSL HOME 直接 `Err`，前端已在 `InstanceEdit.vue:1907-1909` 用 `a-alert` 提前避让。

### 1.5 选择器能力已就绪

- `src-tauri/src/lib.rs:108` — `.plugin(tauri_plugin_dialog::init())` 已注册。
- `src-tauri/capabilities/default.json:16` — `"dialog:default"` 已授权。
- 前端包 `@tauri-apps/plugin-dialog@^2.4.2`（`package.json`），且**已有 8 处在用**，范式统一为动态 import：

```ts
const { open } = await import('@tauri-apps/plugin-dialog')
const dir  = await open({ directory: true,  multiple: false })                            // 目录
const file = await open({ multiple: false, filters: [{ name: '…', extensions: [...] }] }) // 文件
```

参考实现：`Settings.vue:372-384`（目录，含 `api.isTauri` 分支 + `browserPickHint` 降级提示）、`InstanceEdit.vue:170-189 / 490-506 / 602-617`（文件）。

> ⚠️ **范式要求**：浏览器预览态（非 Tauri）必须走 `Message.info(t('settings.browserPickHint'))` 降级，不能抛异常。现有 `InstanceEdit.vue` 的 4 处文件选择**没有**这层防护，新代码应照 `Settings.vue` 写。

### 1.6 Mock 与本地化同步面

| 文件 | 现状 |
| --- | --- |
| `src/api/index.ts:65-73` | `MOCK_LINK_KINDS` 镜像 `REDIRECTABLE`（含 bool） |
| `src/api/index.ts:325-351` | `list_home_links` / `set_home_link` / `clear_home_link` 三个 mock 分支 |
| `src/api/index.ts:1333-1337` | `api.listHomeLinks / setHomeLink / clearHomeLink` |
| `src/locales/zh-CN.json:395-413` | `instanceEdit.storage*` 共 19 键 |
| `src/locales/en-US.json:395-413` | 同键英文，行号与中文一一对应 |

---

## 2. 需求澄清：issue 正文只有一句话

issue 正文两段都以完全相同的一句话填充：

> 重定向「sessions」添加下拉预设和文件选择器

即：**「下拉预设」的确切含义没有权威定义**。这是本 issue 第一优先要解决的问题，也是阶段 1 的产出物。以下三种解释都能自洽，需在动工前定夺：

### 解释 A（推荐）：条目级「目标根目录」快捷预设

在对话框路径输入框上方加一个 `a-select`，列出若干候选根目录，选中后自动拼出 `<root>\<entry>`（或 `<root>\<home-slug>\<entry>`）作为目标路径。

- **候选集**（建议）：启动器数据目录下的 `data/`、HOME 所在盘的 `<盘符>:\dsh-data`、用户上次使用过的根（持久化）。
- **优点**：改动最小、语义直观、与现有 `storageTargetPlaceholder` 的示例 `D:\dsh-data\sessions` 完全吻合。
- **落点**：纯前端即可实现，或由后端提供候选（见阶段 1 决策 D1）。

### 解释 B：HOME 级「数据根」一次性预设

先在存储页顶部选一个「数据根」，所有已启用的条目自动映射为 `<root>/<entry>`，一次性批量生效。

- **优点**：最省事，用户不用逐条目点。
- **代价**：需要批量写入能力（`set_home_link` 目前逐个调用），且「批量迁移」的失败回滚语义复杂。

### 解释 C：按条目类型的推荐值

不同条目给不同默认建议（`sessions` → 大盘目录；`settings.yaml` → 便携配置目录；…）。

- **代价**：需要维护一张「条目 → 建议根」表，收益相对 A 提升有限。

> **建议**：采纳 **A**，并把 C 作为 A 的候选集生成规则（即候选集按条目类型排序/过滤）。B 若确实想要，另开 issue，避免本 issue 膨胀。

> **阶段 1 定稿（2026-09-24）**：采纳 **解释 A**，叠加 C 的「按条目生成」思路——候选集是**根目录**，由前端把条目名拼上去（`joinLinkPath`），因此一套候选服务全部 7 个条目，无需为每个条目维护一张表。解释 B（HOME 级批量重定向）不在本 issue 范围，如需另开 issue。
>
> 本机无可用 WSL 发行版时，WSL 相关行为无法覆盖，§5.2 中已按要求标注。

### 待决问题清单（阶段 1 必须回答）

| ID | 问题 | 建议默认 | 定稿 |
| --- | --- | --- | --- |
| D1 | 候选集由前端硬编码，还是后端新命令提供？ | 后端提供（可感知盘符与存在性），前端硬编码作为 mock 兜底 | ✅ **采纳建议**：新增只读命令 `suggest_home_link_targets`（方案 D1-a），mock 镜像一份 |
| D2 | 是否需要「记住上次使用的根目录」？ | 是，写入 `LauncherSettings`（新增可选字段，向后兼容） | ✅ **采纳**：`last_link_root: Option<String>`，`set_home_link` 成功后写父目录，`openLinkDialog` 时读 |
| D3 | 预设与手填是「二选一」还是「预设填充 + 可继续编辑」？ | 后者：选中预设只写入输入框，用户仍可手改 | ✅ **采纳**：预设只填充 `linkTarget`，输入框始终可编辑 |
| D4 | 目标已存在且非空时的提示 | 沿用 `storageSetHint` 原文案（会备份为 `.bak-时间戳`），不新增分支 | ✅ **采纳**：文案未改 |
| D5 | 是否复用现有对话框，还是改成独立组件？ | 复用 `InstanceEdit.vue` 内联对话框（与 #51 一致，避免无谓拆分） | ✅ **采纳**：仍为内联 `a-modal`，宽度 560→620px |

---

## 3. 方案设计（按解释 A 展开）

### 3.1 交互稿

```text
┌─ 重定向「sessions」 ─────────────────────────────┐
│  目标位置                                        │
│  ( ) 使用预设   [ 启动器数据目录 ▾ ]             │
│  ( ) 自定义                                      │
│                                                  │
│  目标路径 *                                      │
│  [ D:\dsh-data\sessions          ] [浏览…]   │
│                                                  │
│  ⓘ 若条目已存在且目标目录为空，现有内容将迁移到   │
│    目标；否则备份为 .bak-时间戳。修改前请停止使用 │
│    该 HOME 的实例。                               │
└──────────────────────────────────────────────────┘
```

要点：

1. **下拉预设**：`a-select` + `allow-clear`，选项形如 `{ label: '启动器数据目录', value: '<root>' }`。选中即把 `linkTarget` 设为 `join(root, entry)`（拼接规则由 D1 定稿）。
2. **浏览按钮**：紧贴输入框右侧，`link.is_dir === true` → `open({ directory: true, multiple: false, defaultPath })`；`is_dir === false` → `open({ multiple: false, filters: [{ name: …, extensions: [...] }], defaultPath })`。
   - 文件条目的 filters 建议：`settings.yaml` / `.credentials.yaml` / `cordis.patch.yml` → `extensions: ['yaml', 'yml']`。
   - `defaultPath` 传 `linkTarget`（非空时）或 HOME 路径，让对话框从有意义的位置打开。
3. **前端预检**（在 `confirmSetLink` 内，调 API 之前）：
   - 目标为空 → 按钮已 disabled，无需处理；
   - 目标等于/位于 HOME 内部 → 直接 `Message.error(t('instanceEdit.storageTargetInsideHome'))`，不发请求；
   - 文件条目且目标不存在 → **建议交给后端**判定（前端对 UNC/相对路径不可靠），前端只做 HOME 包含关系这一条确定性校验。
4. **Tauri 降级**：`if (!api.isTauri) { Message.info(t('settings.browserPickHint')); return }`。

### 3.2 候选集来源（D1 定稿后二选一）

**方案 D1-a — 后端新命令**（推荐）

```rust
// src-tauri/src/links.rs
#[derive(Clone, Debug, serde::Serialize)]
pub struct HomeLinkSuggestion {
    pub id: String,        // 稳定标识，如 "launcher-data" / "drive-d" / "last-used"
    pub label_key: String, // 由前端按 id/键翻译，避免后端硬编码语言
    pub path: String,      // 候选根目录
    pub exists: bool,      // 目标根当前是否存在
}

#[tauri::command]
pub async fn suggest_home_link_targets(
    state: State<'_, AppState>,
    home_id: String,
    entry: String,
) -> Result<Vec<HomeLinkSuggestion>, String>
```

- **只读**，不改任何配置、不创建目录；**不做可写性副作用探测**（临时文件探测会在用户目录留下痕迹，收益不值）。
- 候选项：启动器 `data_dir` 下的数据目录、HOME 所在盘根、`LauncherSettings.last_link_root`（D2）。
- mock 侧镜像一份固定 3 项，保证浏览器预览可用。

**方案 D1-b — 纯前端硬编码**

不新增命令，前端按 HOME 路径的盘符拼一个 `<盘符>:\dsh-data`，再加一个固定项。改动面最小，但无法感知真实存在性。

### 3.3 需要改动的文件清单

| # | 文件 | 改动 |
| --- | --- | --- |
| 1 | `src/views/InstanceEdit.vue` | 对话框加 `a-select` 预设 + 「浏览…」按钮；新增 `linkPresets` / `linkPresetId` / `pickLinkTarget()` / `applyPreset()`；`confirmSetLink` 加 HOME 包含预检 |
| 2 | `src-tauri/src/links.rs` | （仅 D1-a）`HomeLinkSuggestion` + `suggest_home_link_targets` + 单测 |
| 3 | `src-tauri/src/lib.rs` | （仅 D1-a）注册新命令 |
| 4 | `src/api/index.ts` | 新增 `suggestHomeLinkTargets`；mock 分支 |
| 5 | `src/api/types.ts` | 新增 `HomeLinkSuggestion` |
| 6 | `src/locales/zh-CN.json` / `en-US.json` | 新增预设与浏览相关键（见 §3.4） |
| 7 | `src-tauri/src/config.rs` | （仅 D2）`LauncherSettings` 加 `last_link_root: Option<String>` |

### 3.4 新增 locale 键（中英各一份，键名对齐现有 `storage*` 前缀）

| 键 | zh-CN | en-US |
| --- | --- | --- |
| `storagePreset` | 预设位置 | Preset location |
| `storagePresetNone` | 不使用预设（手动填写） | No preset (enter manually) |
| `storagePresetLauncherData` | 启动器数据目录 | Launcher data directory |
| `storagePresetSameDrive` | 与 DSH_HOME 同盘 | Same drive as DSH_HOME |
| `storagePresetLastUsed` | 上次使用的位置 | Last used location |
| `storageBrowse` | 浏览… | Browse… |
| `storageBrowseDirTitle` | 选择目标目录 | Select target directory |
| `storageBrowseFileTitle` | 选择目标文件 | Select target file |
| `storageTargetInsideHome` | 目标不能位于该 DSH_HOME 内部 | The target must not live inside this DSH_HOME |
| `storagePresetWillCreate` | 尚无此目录，提交时自动创建 | Created automatically on submit |
| `storageFileEntryHint` | 文件条目需要一个已存在的目标文件，请用「浏览…」选择（预设仅适用于目录条目）。 | A file entry needs an existing target file — use Browse (presets apply to directory entries only). |

> 现有键 `storageTargetPlaceholder`（`例如 D:\dsh-data\sessions`）在加了浏览按钮后**建议保留**，作为手填模式的提示。
>
> **v3 变更**：原拟的 `storagePresetUnavailable`（「该位置当前不可用」）**已删除**——`exists=false` 对目录条目是首次安装的正常态而非不可用（复核 F2），改由 `storagePresetWillCreate` 承载准确语义，并新增 `storageFileEntryHint` 说明文件条目为何没有预设下拉（复核 F1）。

---

## 4. 阶段任务树

### 阶段 0：基线确认（0.5 轮）

- [x] 确认工作分支：从 `main`（`f2843e0`）另开 `feat/issue-65-storage-link-preset-picker`，与 #66 的 `fix/issue-66-…` 分离，避免混进同一个 PR。
- [x] 重新核验 §1 全部行号（基线为 `f2843e0`，§1 记录的行号在 `a5a770b` 上仍成立；`InstanceEdit.vue` 的对话框模板实测在 `:2108-2131`，比 §1 记的 `:2141-2164` 略前，属 #66 已改动区段，改动点本身无歧义）。
- [x] 确认工具链可用：`cargo 1.97.0` ✅ / `node v24.15.0` ✅ / `pnpm 11.17.0` ✅。
- [x] `pnpm install --frozen-lockfile`（`Already up to date`）与 `cargo check`（`Finished dev profile`）各跑一次，基线干净。

### 阶段 1：语义与候选集定稿（1 轮，**阻塞后续**）

- [x] 在本文档 §2 记录最终解释：**采纳 A**（叠加 C 的生成规则）。
- [x] 回答 D1~D5 五个待决问题，写回 §2 表格。
- [x] 选 D1-a：`HomeLinkSuggestion { id, label_key, path, exists }`；**不做可写性副作用探测**（仅 `Path::exists()` 只读探测，不创建目录）。
- [x] 选 D2：`LauncherSettings.last_link_root` 写入时机 = `set_home_link` 成功落盘时记录目标父目录；读取时机 = 新命令 `suggest_home_link_targets` 内。
- [x] 产出：本文档 §2 定稿 + 交互稿（§3.1 草稿即为实现稿，宽度 560→620px）。

### 阶段 2：前端对话框改造（1.5 轮，主体工作）

- [x] `InstanceEdit.vue` 新增状态：`linkPresets`、`linkPresetsLoading`、`linkPresetId`、`linkIsDir`、`linkPicking`。
- [x] `openLinkDialog(link)` 改造：按需拉取候选集（`linkPresetCache` 以 `<homeId>::<entry>` 为键缓存）；按 D2 由后端回填「上次使用的位置」。
- [x] 新增 `applyLinkPreset(id)`：把拼接结果写入 `linkTarget`，**允许用户继续编辑**（D3）。
- [x] 新增 `pickLinkTarget()`：按 `is_dir` 分支调用 `plugin-dialog`；带 `api.isTauri` 判断与 `browserPickHint` 降级；带 `try/catch` + `Message.error`。
- [x] `confirmSetLink()` 增加 HOME 包含关系预检（`storageTargetInsideHome`），失败时不发请求。
- [x] 模板改造：`a-form-item` 内加 `a-select` + `a-input-group`（`a-input` + `a-button`）；`ok-button-props.disabled` 逻辑不变；宽度 560→620px。
- [x] 选择器返回的路径做 `trim()`；Windows 下 `\\?\` 长路径前缀**剥离后再写入**（`normalizeLinkPath`）。
- [x] 浏览期间也置位 `linkPicking`，并计入 `ok-button-props.loading`，防止重复点击。

### 阶段 3：后端与契约（仅 D1-a / D2 需要，1 轮）

- [x] `links.rs` 新增 `HomeLinkSuggestion` 与 `suggest_home_link_targets`；命令**只读**，不创建目录、不改配置；候选项数量极少，同步 `exists()` 可接受。
- [x] `lib.rs` 注册命令（紧跟 `list_home_links` / `set_home_link` / `clear_home_link` 之后，保持分组可读）。
- [x] `config.rs` 给 `LauncherSettings` 加 `last_link_root: Option<String>`，带 `#[serde(default, skip_serializing_if = "Option::is_none")]` 保证老配置可加载。
- [x] `links.rs` 单测：候选集在 HOME 不存在 / 条目非法 / WSL HOME 时的行为（WSL 已由 `:134-136` 拒绝，新命令保持一致；新增 6 个单测覆盖只读性、去重、上次使用、盘符根、向后兼容）。

### 阶段 4：mock / 类型 / 双语 / 单测收尾（0.5 轮）

- [x] `api/types.ts` 新增 `HomeLinkSuggestion`（带注释指向 `links.rs`）+ `LauncherSettings.last_link_root`。
- [x] `api/index.ts` 新增 `suggestHomeLinkTargets` + mock 分支（镜像真实命令：去重 + 「上次使用」跟随 `last_link_root`），保证浏览器预览可点。
- [x] 两个 locale 文件补齐 §3.4 全部键；**中英键集合实测完全一致**（v2 各 637 键、v3 各 638 键，零漂移，脚本比对）。
- [x] 未引入新行为文案变更（D2 的「上次使用」由新键 `storagePresetLastUsed` 承载），`storageDesc` / `storageSetHint` 无需改动。

### 阶段 5：测试与验收（1 轮）

- [x] `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` ✅
- [x] `cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings` ✅
- [x] `cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked` ✅ 187 passed / 0 failed（含新增 6 项）
- [x] `pnpm build`（= `vue-tsc --noEmit && vite build`）✅
- [x] 手工冒烟矩阵见 §5（浏览器预览态逐项走查，见 §7 回填日志）。

---

## 5. 验收标准与冒烟矩阵

### 5.1 功能验收

- [x] 打开「重定向『sessions』」对话框，**能看到下拉预设**且至少有一个可选项（实测 3 项）。
- [x] 选择预设后，目标路径输入框被正确填充（`<root>\<entry>`），且**仍可手动修改**。
- [x] **文件条目（`settings.yaml` / `.credentials.yaml` / `cordis.patch.yml`）不渲染预设下拉**（v3/F1），改为 `storageFileEntryHint` 提示，目标经「浏览…」选择；候选根改作文件选择器的 `defaultPath`。
- [x] 点「浏览…」弹出**目录选择器**（`sessions` 是目录条目）——`is_dir=true` 分支已实现，**桌面端调用未在本机执行**（无 Tauri 运行态，见下方说明）。
- [x] 对**文件条目**（`settings.yaml` / `.credentials.yaml` / `cordis.patch.yml`）点「浏览…」弹出**文件选择器**且过滤 `yaml` ——`is_dir=false` 分支已实现并经代码路径核对，桌面端弹窗未实测。
- [x] 浏览器预览态（非 Tauri）点「浏览…」→ 提示 `settings.browserPickHint`（实测文案「浏览器预览模式不支持系统目录选择，请手动输入路径」），**不抛异常、不刷控制台错误**。
- [x] 手填 HOME 内部路径 → 提交前即被拦截并给出 `storageTargetInsideHome` 文案，**不发请求**（实测 mock 的 `home.links.sessions` 未被写入）。
- [x] 手填已存在的合法外部路径 → 正常生效，表格状态变为「已生效」，Toast 为 `storageSetDone`（实测 `D:\dsh-data-external\sessions` 写入成功）。
- [x] 已重定向后再次打开对话框 → 输入框回显当前 `target`，且「上次使用的位置」预设出现（实测 `D:\dsh-data-external`）。
- [x] 该 HOME 下有实例运行时 → 后端拒绝逻辑**未改动**（`ensure_no_running_instance` 原样保留），无回归。

### 5.2 冒烟矩阵

| 维度 | 取值 |
| --- | --- |
| 运行形态 | Tauri 桌面 / 浏览器 mock 预览 |
| 条目类型 | 目录（`sessions`）/ 文件（`cordis.patch.yml`） |
| HOME 形态 | 本地 Windows HOME / WSL HOME（后者应显示 `storageWslUnsupported` 提示，**不渲染对话框**） |
| 目标状态 | 不存在（目录，后端创建）/ 存在且为空（迁移）/ 存在且非空（备份 `.bak-`）/ 文件不存在（后端拒绝） |
| 语言 | zh-CN / en-US |

- [x] 上表**代码路径**逐项走查完成。
- [x] **实际执行覆盖**：浏览器 mock 预览（zh-CN + en-US）全流程：目录条目 `sessions` 打开对话框 → 预设下拉（3 项，缺失根标注「尚无此目录，提交时自动创建」）→ 选预设填充 `…\dsh-data\sessions` → 浏览按钮降级提示 → HOME 内路径被拦截且无写入 → 合法外部路径写入成功、表格转「已生效」、`last_link_root` 记录 → 重新打开文件条目 `cordis.patch.yml`：**无预设下拉**、显示新提示 → 目录条目下拉出现「上次使用的位置」（v3 实测）。
- [x] 双语静态核验：两个 locale 各 **638** 键、键集合完全一致；后端 3 个 `label_key` 在 zh/en 均可解析出非空标签（脚本比对）。
- [ ] **未覆盖（明确声明，不得声称通过）**：
  - **Tauri 桌面运行态**：本机未启动 launcher 桌面窗口，`plugin-dialog` 的真实目录/文件弹窗、`defaultPath` 生效、`\\?\` 前缀剥离的**真实返回值**均未实测，仅有代码路径核对。
  - **WSL HOME**：本机无可用 WSL 发行版，`storageWslUnsupported` 提前避让未实测。
  - **后端真实文件系统副作用**：目录创建、空目录迁移、非空目录 `.bak-` 备份、文件不存在拒绝，仅由既有单测与代码路径保证，浏览器 mock 不做文件系统操作。

### 5.3 回归红线

- [x] `REDIRECTABLE` 白名单**未扩大**（7 项原样）。
- [x] `set_home_link` / `clear_home_link` 的**后端校验一条都未放宽**——前端预检只是体验优化，后端仍是唯一权威。
- [x] `links.rs` 现有单测（`whitelist_rejects_unknown_entries` 等）全部通过。
- [x] 新增命令 `suggest_home_link_targets` 是**只读**的（无创建、无写入；单测断言探测不落盘）。

---

## 6. 开放问题（需维护者确认）

1. **「预设」到底是 A / B / C 哪一种？** issue 正文只有一句话且重述两遍，无任何补充说明。本计划默认按 A 展开；若维护者意指 B（HOME 级数据根批量重定向），改动面与风险等级都会显著不同，需重新估算。
2. **「文件选择器」是否包含文件条目？** 若原意只是「给 sessions 加一个选目录的按钮」（`sessions` 本身是目录），则 §3.4 的部分键与 `is_dir` 分支可以砍掉，工作量下降约 30%。
3. **是否值得为此新增后端命令？** 若倾向「零后端改动」，则走 D1-b 纯前端方案，但会失去「上次使用位置」与「真实存在性」两项能力（D2 也随之作废）。
4. **与 #66 的分支关系**：本机当前工作分支正在做 #66，本 issue 是否要等 #66 合入后再从 `main` 切出？若并行，`InstanceEdit.vue` 是两者共同触及的文件，**存在冲突风险**（#66 主要在 `plugins` 标签页，冲突概率低但非零）。

---

## 7. 回填日志

> 执行阶段每完成一项，在此追加一行；勾选框同步回 §4。

| 日期 | 阶段 | 动作 | 结果 |
| --- | --- | --- | --- |
| 2026-09-24 | 规划 | 核验 `a5a770b` 基线，产出本文件 draft v1 | ✅ 完成，等待 §6 开放问题答复 |
| 2026-09-24 | 阶段 0 | 从 `main`（`f2843e0`）切出 `feat/issue-65-storage-link-preset-picker`；`pnpm install --frozen-lockfile` + `cargo check` 基线干净 | ✅ |
| 2026-09-24 | 阶段 1 | 采纳解释 A（叠加 C 的生成规则）；D1~D5 全部按建议定稿并写回 §2 | ✅ |
| 2026-09-24 | 阶段 3 | `links.rs` 新增 `HomeLinkSuggestion` + 只读 `suggest_home_link_targets` + `same_volume_root`；`record_link` 记 `last_link_root`；`lib.rs` 注册；`config.rs` 加向后兼容字段；6 个新单测 | ✅ |
| 2026-09-24 | 阶段 4 | `types.ts` / `api/index.ts`（含 mock 镜像与 `last_link_root` 跟踪）/ 双语各 10 键 | ✅ 键集合各 637，零漂移 |
| 2026-09-24 | 阶段 2 | `InstanceEdit.vue`：预设 `a-select` + 「浏览…」`a-input-group` + HOME 包含预检 + `\\?\` 剥离 + 预设缓存 | ✅ 前端 +201 行 |
| 2026-09-24 | 阶段 5 | `cargo fmt --check` ✅ / `clippy -D warnings` ✅ / `cargo test` 187 passed ✅ / `pnpm build` ✅ | ✅ 全绿 |
| 2026-09-24 | 阶段 5 | 浏览器 mock 预览手工冒烟（zh-CN）：预设填充、浏览降级提示、HOME 内拦截无写入、合法路径生效、文件条目拼接、上次使用预设出现 | ✅；Tauri 桌面态与 WSL 列**未覆盖**（见 §5.2） |
| 2026-09-24 | 复核 v3 | 处理 PR #68 复核 F1~F8：文件条目去预设（F1）、`exists` 文案（F2）、verbatim UNC 与盘符根（F3/F4）、mock `exists` 自洽（F5）、过期响应守卫（F6）、混合分隔符（F7）、文档改名（F8） | ✅ 8/8，见 §7.2 |
| 2026-09-24 | 复核 v3 | 复验：fmt ✅ / clippy ✅ / 187 passed ✅ / `vue-tsc` ✅ / `pnpm build` 46.21s ✅ / 双语各 638 零差集 / 抽取函数 20 条断言 / zh+en mock 实测 / F7 端到端拦截且无写入 | ✅ 全绿 |

### 7.1 实现要点与偏差说明

1. **`label_key` 而非文案**：后端只发 `storagePresetLauncherData` 之类裸键，前端补 `instanceEdit.` 前缀并用 `te()` 兜底，避免后端硬编码语言；已脚本核验 3 个键在 zh/en 均可解析。
2. **候选集为「根目录」而非「完整目标路径」**：一套候选服务全部**目录条目**，前端 `joinLinkPath` 按根目录自身的分隔符拼接（Windows `\`、POSIX `/`），无需条目→建议表。**v3（F1）**：正因为候选是根目录，文件条目无法由它拼出「已存在」的目标文件，故文件条目不使用预设，候选根改作「浏览…」的 `defaultPath`。
3. **`same_volume_root` 的盘符处理**：Windows 走 `Component::Prefix`（`Disk` / `VerbatimDisk`），无盘符（unix、UNC）回退到 `<home 父目录>/dsh-data`；根目录无父目录时返回 `None` 而不是提议 HOME 内部路径（会被后端直接拒绝）。
4. **`exists` 只读探测**：仅 `Path::exists()`，不创建目录、不写配置、不做临时文件可写性探测（与 §3.2 约定一致，单测断言探测后目录仍不存在）。
5. **mock 忠实度**：mock 的 `suggest_home_link_targets` 同样做去重并读取 `last_link_root`，`set_home_link` 也镜像记录父目录，使浏览器预览行为与真实后端一致。**v3（F5）**：唯一刻意不忠实的是 `exists`——mock 无文件系统，按候选 id 给固定预览值并在注释里写明（原先的 `c.id !== 'same-drive'` 与注释自相矛盾，已改）。
6. **偏差**：§3.1 交互稿画的是「( ) 使用预设 / ( ) 自定义」单选，实际实现简化为「一个可清空的 `a-select` + 始终可编辑的输入框」。二者都满足 D3（预设填充后可继续编辑），但实现更轻、少一层状态机；如需严格单选 UI 可后续调整。
7. **未执行项**：Tauri 桌面运行态的 `plugin-dialog` 真实弹窗未实测（本机未启动桌面窗口）；WSL 列无可用发行版。均已在 §5.2 明确标注为「未覆盖」。

### 7.2 复核修复记录（v3，PR #68 review）

独立复核（[PR #68 评论](https://github.com/dsh-plugins/dsh-launcher/pull/68)）确认方向与能力边界站得住（`links.rs` +215/−0 纯新增、白名单与 `set_home_link` 校验零改动），提出 4 条建议合并前处理（F1~F4）与 4 条小项（F5~F8）。**8 条全部处理**：

| 编号 | 问题 | 处置 |
| --- | --- | --- |
| F1 | 文件条目点预设几乎必被后端拒（候选是**根目录**，`joinLinkPath` 拼出的文件路径基本不存在，而后端要求文件目标已存在） | 文件条目不渲染预设下拉（`v-if="linkIsDir"`），改为 `storageFileEntryHint` 提示；`applyLinkPreset` 同时加 `!linkIsDir` 守卫。候选根改用于「浏览…」的 `defaultPath` |
| F2 | `exists=false` 标成「该位置当前不可用」，但目录不存在正是首次安装的正常可用态 | 文案改为 `storagePresetWillCreate`（「尚无此目录，提交时自动创建」）；删除 `storagePresetUnavailable` |
| F3 | `normalizeLinkPath` 剥 `\\?\` 会把 verbatim UNC `\\?\UNC\srv\share\x` 变成**相对路径** `UNC\srv\share\x` | 先识别 `\\?\UNC\` 并改写为 `\\`（大小写不敏感），再剥普通 `\\?\` |
| F4 | `normalizeLinkPath` 把盘符根 `D:\` 变成盘符相对 `D:` | 剥尾分隔符后若结果为 `^[A-Za-z]:$`，补回 `\` |
| F5 | mock 的 `exists: c.id !== 'same-drive'` 与自身注释、与真实探测均不一致（`last-used` 恒 true 而 `same-drive` 恒 false，自相矛盾） | 改为显式 `mockExists` 固定表（`launcher-data`/`same-drive` 为 false、`last-used` 为 true），并写明 mock 无文件系统、这是刻意预览近似 |
| F6 | `loadLinkPresets` 缺过期响应守卫，快速连开两个对话框时先发后到的响应会串台 | 加单调递增的 `linkPresetReqSeq`；返回时 `reqId !== linkPresetReqSeq` 则丢弃（含 `catch` 的报错与 `finally` 的 loading） |
| F7 | `isInsideHome` 只按 HOME 自身分隔符判定，`C:/Users/x/.dsh/...`（正斜杠）漏检 | 比较前把两侧 `\` 统一折叠为 `/`，并对 HOME 剥尾分隔符 |
| F8 | 文档名 `TASK_LIST` 与仓库既有 `TASK_TREE` 惯例不一致 | `git mv` 改为 `docs/ISSUE65_TASK_TREE.md`，H1 同步为「任务树」 |

**v3 复验**（全部实测，非照抄）：

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |
| `cargo test --workspace --locked` | ✅ 187 passed / 0 failed |
| `vue-tsc --noEmit` | ✅ 0（先于 vite 单独通过） |
| `pnpm build` | ✅ 46.21s（沙箱下 esbuild `spawn EPERM`，放宽后成功） |
| 双语键集合 | ✅ 各 **638** 键、双向零差集（v2 为 637，删 1 增 2） |
| F3/F4/F7 纯函数断言 | ✅ 从 `InstanceEdit.vue` **提取真实函数**跑 20 条断言：verbatim UNC → `\\srv\share\x`、`D:\` 保持盘符根、混用分隔符的 HOME 内路径判为内部 |
| 浏览器 mock 实测（zh-CN） | ✅ 目录条目「预设位置」下拉 3 项 + 「尚无此目录，提交时自动创建」；**文件条目无下拉**、显示新提示；`last-used`（真实存在）不带后缀 |
| 浏览器 mock 实测（en-US） | ✅ `Preset location` / `Created automatically on submit` / 文件条目英文提示 |
| F7 端到端 | ✅ 输入 `C:/Users/Administrator/.dsh/cordis.patch.yml` → 提交前被拦（「目标不能位于该 DSH_HOME 内部」）、对话框保持打开、**`home.links` 无写入**（修复前该形态漏检） |
| 合法路径回归 | ✅ `D:/dsh-data-external/cordis.patch.yml` 写入成功、`last_link_root` 记为 `D:/dsh-data-external`、浏览按钮仍走 `browserPickHint` 降级 |

**仍未覆盖（同 §5.2，未变）**：Tauri 桌面真实弹窗与 `\\?\` 的**真实返回值**、WSL 列。F3/F4 的修复正是针对桌面态返回值，其**代码路径已由抽取函数的断言覆盖**，但真实弹窗未跑。

---

## 附：核验用命令速查

```powershell
# 基线
cd D:\DSH\DSH-Launcher; git rev-parse HEAD; git branch --show-current

# 关键源文件定位
Select-String -Path src-tauri/src/links.rs -Pattern 'REDIRECTABLE|fn set_home_link|fn clear_home_link'
Select-String -Path src/views/InstanceEdit.vue -Pattern 'openLinkDialog|confirmSetLink|linkTarget'

# 构建/测试
cargo fmt    --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test   --manifest-path src-tauri/Cargo.toml --workspace --locked
pnpm build
```
