# Issue #46 任务树 — 扩展插件市场发现来源（多目录源 + GitHub topic 实时发现 + 可信度分层）

> 来源:<https://github.com/dsh-plugins/dsh-launcher/issues/46>
> 核验基线:`origin/main` @ `3a802ae`(`feat(modpack): 下载页新增整合包市场`)
> 工作仓库:`D:\DSH\dsh-launcher-pr`(fork;`origin` 指向上游 `dsh-plugins/dsh-launcher`)
> 分支:`feat/relocatable-data-dir`(基于 `origin/main` @ `3a802ae`,含 issue #46 全部改动)
> 文档状态:**final v2**(阶段 0–3、5 完成并入分支;阶段 4 有意延期)。本文件既是规划文档,也是执行时回填进度的活文档(勾选框 + §6 回填日志)。

---

## 0. 结论先行

| 项 | 结论 |
| --- | --- |
| 问题是否真实存在 | ✅ 真实。市场只消费两个硬编码 URL(`plugins.rs:11,14`),主源 `dsh-plug.in` 近乎为空(7 条),实际只靠 `awesome` 单源 |
| 是否可做 | ✅ 可行。`parse_awesome_install` / `github_api_url` / `plugin_sources` 配置路径均可复用;无阻塞性依赖 |
| 主要风险 | GitHub search 限流(~30/min)、代理可达性(全部 HTTP 走 `proxy::apply`)、`PluginSource` 为封闭 enum 需改造、`alpha_commit` 回查静态市场的既有缺陷 |
| 粒度红线 | **绝不误装 `@deepseek-ai/*` 核心包**;实时来源必须标记 `Unverified` + 安装前二次确认 |
| 建议总迭代 | 4 个阶段,约 **10~14 轮 agent 迭代**(阶段 2 的源注册表 + DshGet 适配是核心,阶段 3 的 topic 通道是风险最高项) |

---

## 1. 源码核验明细(证据)

### 1.1 当前硬编码的源

- `src-tauri/src/plugins.rs:11` — `const MARKET_URL: &str = "https://dsh-plug.in/api/plugins.json";`
- `src-tauri/src/plugins.rs:14` — `const AWESOME_URL: &str = "https://awesome-dsh-plugin.com/plugins.json";`
- `fetch_plugin_market`(`plugins.rs:394` 起)用 `tokio::join!` 并发拉这两个常量 URL,再按 id 去重(保留更早/更高优先源),单源失败仅 `log_warn!` 跳过,不阻塞其余源。
- 无任何「源」抽象:URL、解析、`kind` 全部写死在函数体内。

### 1.2 现有数据结构(改造面)

- `PluginSource`(`plugins.rs:91-101`)是**封闭 enum**:`DshPlugins(默认)| AwesomeDshPlugin`,`#[serde(rename_all = "kebab-case")]` 序列化为 `dsh-plugins` / `awesome-dsh-plugin`。新增 dshget / github-topic 必须扩展此枚举(或改为字符串化 id)。
- `MarketPlugin`(`plugins.rs:68` 起)已带 `source: PluginSource`、`category`、`stars`、`downloads`;缺「可信度 confidence」「来源归属 sources[]」「repo 提示」字段。
- `parse_awesome_install(install: &str)`(`plugins.rs:154`)已能解析 npm / `github:owner/repo[#path:<sub>]` / `tgz:` 三种目标 → 可直接复用于 `DshGet` 与 `GithubTopic` 适配器。
- `awesome_to_market`(`plugins.rs:236`)是「原始条目 → MarketPlugin」的既有范式,新适配器照此实现。
- `github_api_url`(`plugins.rs:25`)+ `GITHUB_CLIENT_ID`(`:22`)— 已带匿名 client_id 提额,可直接用于 topic 搜索。

### 1.3 既有缺陷(必须同步修复)

- `alpha_commit`(`plugins.rs:709`)内部调用 `fetch_plugin_market(None)` 回查市场来定位 repo:
  - 实时通道新增的插件**不在静态目录里** → `alpha_commit` 报「插件 … 不在市场中」。
  - 修复方向:版本解析需接受条目自带 `repo` 提示(前端把 `MarketPlugin.urls`/`repo` 传入 `fetch_plugin_versions`),或让 `alpha_commit` 走统一源解析而非静态回查。

### 1.4 前端现状(改造面)

- 源过滤写死:`src/views/plugins/Market.vue:109-113` 三个 `<a-option>`(`''` / `dsh-plugins` / `awesome-dsh-plugin`);`sourceOf()`(`:39`)、`filtered`(`:43`)按 `PluginSource` 字符串匹配。
- 类型:`src/api/types.ts:392` `PluginSource = 'dsh-plugins' | 'awesome-dsh-plugin'`;`:394` `MarketPlugin`。
- API 层:`src/api/index.ts:1364` `fetchPluginMarket`;`:925` 浏览器预览 mock 必须同步;settings:`:1325-1326` `getSettings` / `updateSettings`。
- 设置页可复用范式:`Settings.vue:457-482` 的 `skillRepos` 列表(输入 + 新增 + 删除 + `patchSettings`)——「插件源」管理 UI 照此实现即可。
- i18n:`zh-CN.json:134` `plugins` 块,`:137` `sourceAll`;`en-US.json` 同结构须同步。
- 命令注册点:`src-tauri/src/lib.rs:266` `plugins::fetch_plugin_market`;`commands.rs:1018/1030` `get_settings`/`update_settings`。

### 1.5 周边机制

| 机制 | 位置 | 对本次改动的影响 |
| --- | --- | --- |
| 代理 | 全部 HTTP 经 `proxy::apply(reqwest::Client::builder())`(`plugins.rs` `http_client`) | topic 搜索同样走代理;代理不可达时须 last-good 兜底 |
| 缓存目录 | `state.data_dir`(见 `commands.rs` / `plugins.rs:2051`) | 新增 `data_dir/plugin-cache/` 落盘缓存 |
| 安装流程 | `src/views/plugins/InstallWizard.vue`、`VersionPick.vue` | `Unverified` 二次确认插在此处 |
| 设置持久化 | `Config.settings: LauncherSettings`(`config.rs:56`)+ `SettingsPatch`(`config.rs:198`)+ `update_settings`(`commands.rs:1030`) | `plugin_sources` 字段 + patch 分支 |

### 1.6 dshget `catalog.json` 实测样本(阶段 0 核验)

- **仓库**:`bobby-sheng/dshget-data`(默认分支 `main`);快照 URL:
  `https://cdn.jsdelivr.net/gh/bobby-sheng/dshget-data@main/catalog.json`
- **体积**:2,603,554 字节(2.6 MB)→ 现有 `fetch_json` cap 需按 8 MB 给足。
- **⚠️ 可达性实测(阶段 2 发现,已改变定稿)**:本机(及国内多数网络)
  `raw.githubusercontent.com` 与 `api.github.com` **不可达**(curl 000 / reqwest 请求失败),
  而 jsDelivr CDN 返回**字节数完全相同**的 2,603,554 B 快照
  (`cdn` / `fastly` / `gcore` 三个节点均 200,1.6–4.7s)。
  故 dshget 默认 URL **改用 jsDelivr**,topic 探测也改走
  `https://cdn.jsdelivr.net/gh/<owner>/<repo>@HEAD/{package.json,cordis.patch.yml}`。
  用户在设置页可改回 `raw.githubusercontent.com` 或自建镜像。
  推论:GitHub 搜索/API 与 alpha/release 通道本就依赖代理,故 `github-topic` 默认关闭是合理默认。
  端到端验证:新增 `live_dshget_catalog_parses_and_filters_core`(`--ignored`)实测通过
  —— >1000 条、全部带 `source`/`confidence`、github 条目全部带 `repo` 提示、无核心包。
- **顶层结构**:`{ name, url, sources[], updated, syncedAt, count, categories{}, plugins[] }`,`count = 2460`。
- **`sources[]`** 是上游归属表(awesome-dsh-plugin / hrhgit-catalog / omdsh-hub / github-topic),与本次的 launcher 源 id 不同层,但可映射进 `MarketPlugin.sources` 归属。
- **条目字段**(实测 union):`name, owner, url, page, category, description{en,zh}, npm, stars, install, added, sources[], verification, installable, license, version, featured, tags[]`。
- **`install` 行格式**:`dsh plugin --profile web add github:owner/repo`(无 `tgz:`)→ 与 `parse_awesome_install` 100% 兼容,**无需新解析器**。
- **关键数据事实(影响实现)**:
  - `installable === false` 共 **605** 条 → 适配器应跳过(不可安装)。
  - `install` 行含 `@deepseek-ai/` 共 **21** 条 → 属于粒度红线,**适配器硬性排除**(见 §2.9)。
  - `npm` 字段非空 **1114** 条,但 `install` 仍多为 `github:`(如 `open-design`)→ **以 `install` 行为准**,`npm` 仅作 `category` 之外的兜底显示信息,不用于生成 id。
  - `verification` 有值 **1000** 条,取值含 `"rejected"` → 保留原始值到条目 `verification`,前端标注;`rejected` 不静默丢弃(用户仍可见,但标 Unverified)。
- **示例条目**:
  ```json
  { "name": "7d7d", "owner": "omdsh-dev", "url": "https://github.com/omdsh-dev/7d7d",
    "category": "ui", "description": {"en": "…", "zh": "…"}, "npm": null, "stars": 0,
    "install": "dsh plugin --profile web add github:omdsh-dev/7d7d", "sources": ["omdsh-hub"],
    "verification": null, "installable": true, "license": "MIT", "version": "0.4.0-rc.2",
    "featured": true, "tags": ["games", "html5", "ruffle"] }
  ```

### 1.7 `PluginSource` 改造面(全仓实测引用)

| 位置 | 现状 | 改法 |
| --- | --- | --- |
| `plugins.rs:82` | `pub source: PluginSource` | 改 `pub source: String`,serde default `"dsh-plugins"` |
| `plugins.rs:94-102` | `enum PluginSource { DshPlugins, AwesomeDshPlugin }` | **删除**,id 由源描述符提供 |
| `plugins.rs:270` | `awesome_to_market` 内赋值 | 由 adapter 统一回填 |
| `plugins.rs:416` | primary 循环内赋值 | 同上 |
| `plugins.rs:2749` | 单测断言 enum | 改断言 `String` |
| `types.ts:392` | `type PluginSource = 'dsh-plugins' \| 'awesome-dsh-plugin'` | 改 `type PluginSource = string` |
| `types.ts:402` / `launcher.ts:64` / `Market.vue:21,39` | 直接使用 | 保持,语义变宽 |

> 结论:改造面极小,`PluginSource` 由封闭 enum 改为**字符串源 id** 即可同时支持内置源与 `custom:<id>` 自定义源,无需引入 `custom:` 解析分支。

---

## 2. 方案设计(采纳路径)

### 2.1 源描述符 + adapter(A)

用源描述符表替换两个常量,`fetch_catalog(src)` 按 `kind` 分派:

```rust
struct PluginSourceConfig {
    id: String,          // "dsh-plugins" | "awesome" | "dshget" | "github-topic" | 自定义
    url: String,
    kind: SourceKind,    // Primary | Awesome | DshGet | GithubTopic
    enabled: bool,
    confidence: Confidence, // Official | Curated | Aggregated | Unverified
    order: u32,
}
```

首批适配器:
- `PrimaryDshPlugins`(现有逻辑抽取)
- `Awesome`(现有逻辑抽取)
- `DshGet`(**新增**):解析 `catalog.json`,install 行复用 `parse_awesome_install`
- `GithubTopic`(**新增**,见 2.3)

### 2.2 源可配置化(D)

- `LauncherSettings.plugin_sources: Vec<PluginSourceConfig>`(`config.rs:56` 加字段,带默认值并持久化);`SettingsPatch` 加 `plugin_sources`(`config.rs:198` → `commands.rs:1030` 加分支)。
- 新增命令 `list_plugin_sources`(`lib.rs` 注册)。
- 前端源过滤从写死改为**动态渲染**(`Market.vue:109-113`);`Settings.vue` 支持增删 / 排序 / 自定义 URL(含镜像)。
- 环境变量 `DSHLAUNCHER_PLUGIN_SOURCES` 可覆盖(对齐 dsh-market 的 `DSHM_REGISTRY_URL` 思路)。

### 2.3 GitHub topic 实时发现(C)

- `GET /search/repositories?q=topic:dsh-plugin&sort=updated&per_page=100&page=N`,复用 `github_api_url` 提额;注意 search 子限流(~30/min)→ 页级预算 + 退避。
- 降噪(14.8k → 可用):repo 带 topic **且**(`package.json` 声明 `dsh.bundle` / 存在 `cordis.patch.yml` / 名匹配 `dsh-*`);排除 DSH 核心仓与已知无关大仓(可维护 denylist)。
- 落 `data_dir/plugin-cache/github-topic.json`,**TTL 24h + 断点分页 + 磁盘 last-good 兜底**。
- 条目标记 `Unverified`,安装前二次确认。

### 2.4 可信度分层

- `Confidence = Official | Curated | Aggregated | Unverified`;同 id 去重取最高层,union `stars/sources/category`。

### 2.5 定稿项(阶段 1 落地)

**命令签名**(Rust,返回 `Result<_, String>`):
```rust
#[tauri::command] pub fn list_plugin_sources(state: State<AppState>) -> Result<Vec<PluginSourceConfig>, String>
// fetch_plugin_market 保持不变(内部改为遍历已启用源 + 缓存)
```
**i18n 键位**(zh-CN / en-US 同步):`plugins.source.*`、`plugins.confidence.{official,curated,aggregated,unverified}`、`settings.pluginSources.*`、`plugins.unverifiedConfirm`。

### 2.6 定稿:类型与去重优先级(阶段 1)

类型落点:为打破 `config.rs` → `plugins.rs` 的循环,把三个新类型定义在 **`config.rs`**(`LauncherSettings` 需要持有它们),`plugins.rs` 通过 `crate::config::` 引用。

```rust
// config.rs —— 源种类:决定分派到哪个 adapter
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind { Primary, Awesome, DshGet, GithubTopic }

// config.rs —— 可信度:Ord 升序 = 不可信 → 可信,便于同 id 去重取 max
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Confidence { #[default] Unverified, Aggregated, Curated, Official }

// config.rs —— 源描述符
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginSourceConfig {
    pub id: String,           // "dsh-plugins" | "awesome-dsh-plugin" | "dshget" | "github-topic" | "custom-<n>"
    pub url: String,          // 目录 JSON 的 http(s) URL
    pub kind: SourceKind,
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default)] pub confidence: Confidence,
    #[serde(default)] pub order: u32,
}
```

`MarketPlugin` 增字段(id 为字符串源 id):
```rust
#[serde(default = "default_source_id")] pub source: String,   // 默认 "dsh-plugins"
#[serde(default)] pub confidence: Confidence,                  // 默认 Unverified
#[serde(default)] pub sources: Vec<String>,                    // 归属 union
#[serde(default)] pub repo: Option<String>,                    // github:owner/repo 提示(修 alpha_commit)
#[serde(default)] pub verification: Option<String>,            // dshget verification 原样透传
```

> 兼容性:`source` 的 serde 默认值仍是 `"dsh-plugins"`,旧缓存/旧前端载荷不破。

**去重优先级**(同 id 多条):
1. `confidence` 高者胜(Ord max);同级按源 `order` 小者胜。
2. 胜者保留自身主体字段;`stars`/`downloads` 取各条最大值。
3. `sources` 取 union;`category` 取胜者非空值,否则取首个非空。
4. 排除规则(§2.9)在任何源产出后立即生效,**先排除再去重**。

### 2.7 定稿:默认源与 `DSHLAUNCHER_PLUGIN_SOURCES`(阶段 1)

默认描述符表(4 条,**3 条启用**;`github-topic` 随阶段 3 落地后默认关闭、由用户在设置页开启):

| order | id | kind | confidence | enabled | url |
| --- | --- | --- | --- | --- | --- |
| 0 | `dsh-plugins` | `primary` | `official` | ✅ | `https://dsh-plug.in/api/plugins.json` |
| 1 | `awesome-dsh-plugin` | `awesome` | `curated` | ✅ | `https://awesome-dsh-plugin.com/plugins.json` |
| 2 | `dshget` | `dsh-get` | `aggregated` | ✅ | `https://cdn.jsdelivr.net/gh/bobby-sheng/dshget-data@main/catalog.json` |
| 3 | `github-topic` | `github-topic` | `unverified` | ⬜ | (无静态 URL,走 search API) |

`DSHLAUNCHER_PLUGIN_SOURCES` 解析规则(设置存在时**整体覆盖**启用列表):
- 逗号分隔多条;每条 `id|kind|url`(管道分隔,3 段)或仅 `url`(2 段以下时 id 由 URL 推导、kind 由 URL 启发式推断)。
- `kind` 取值:`primary` / `awesome` / `dsh-get` / `github-topic`。
- 解析失败/URL 非 http(s) 的条目跳过并 `log_warn!`,不影响其余条目;全部失败则回退到持久化设置。
- 覆盖仅作用于**本次进程的读取结果**,不回写 `config.json`。

### 2.8 定稿:topic 降噪与 denylist(阶段 1,阶段 3 实现)

- 搜索:每页 100 条,上限 `MAX_PAGES = 2`(即最多 200 条候选,页级预算 + 每页失败指数退避 1s/2s/4s,最多 3 次)。
- 候选必须**同时**满足:
  1. repo 带 `topic:dsh-plugin`(API 已保证);
  2. **且**下列任一:仓库名匹配 `(?i)^dsh[-_]`、`package.json` 声明 `dsh.bundle`、仓库根存在 `cordis.patch.yml`;
  3. **且**不在 denylist(名称或 `full_name` 精确/前缀匹配)。
- denylist(内置常量,可后续外置):`deepseek-ai/DeepSeek-*`、`deepseek-ai/dsh` 等核心仓,以及已确认与插件无关的大仓(如 awesome 类聚合仓、`dsh-plugin-hub` 等)。维护方式:先内置为 `const TOPIC_DENYLIST: &[&str]`,附单测保证不误纳核心仓。
- 名字启发式(规则 2 里的"名匹配 `dsh-*`")仅作**准入**,不构成"插件"判定;因此额外要求(2)中至少一项为真,避免纯名字巧合。

### 2.9 定稿:粒度红线与 `@deepseek-ai/*` 排除(阶段 1)

- 任何 adapter 产出的条目,若 `id`(install target)以 `@deepseek-ai/` 开头,或 `github:` owner 为 `deepseek-ai`,则**直接丢弃**并 `log_warn!`。
- 该规则在第一入口统一执行(`fetch_catalog` 返回后过滤),单测覆盖,确保任何源(含用户自定义源)都无法把核心包喂进市场。
- 前端:安装向导对 `confidence === 'unverified'` 的条目二次确认(含"未验证来源"文案),后端 `start_install_plugin_task` 不做额外拦截(避免误伤合法 unverified 插件)。

---

## 3. 阶段任务树

### 阶段 0:现状确认(1 轮 scout / 主 agent)  ✅
- [x] 冻结基线 `3a802ae`,复核 §1 行号 —— `3a802ae` 是 HEAD(`feat/relocatable-data-dir` @ `ed574d5`)祖先;`plugins.rs` 相对基线**零改动**,§1 行号全部有效
- [x] 拉取 `dshget-data/catalog.json` 样本(见 §1.6),确认可解析 JSON 且 install 行 `dsh plugin --profile web add <target>` 完全兼容 `parse_awesome_install`
- [x] 确认 `PluginSource` 封闭 enum 的改造范围 —— 全仓仅 6 处引用(见 §1.7),改 `String` 成本低

### 阶段 1:设计与定稿(1 轮 oracle + review)  ✅
- [x] 定稿源描述符结构 / `SourceKind` / `Confidence` 与去重优先级(见 §2.6)
- [x] 定稿 `plugin_sources` 默认值(4 条描述符,3 条默认启用)与 `DSHLAUNCHER_PLUGIN_SOURCES` 解析规则(见 §2.7)
- [x] 定稿 topic 降噪规则与 denylist 维护方式(见 §2.8)
- [x] 定稿 i18n 键位与命令签名(见 §2.5)

### 阶段 2:核心 — 源注册表 + DshGet 适配 + 动态 UI(A+D,立即 +2.4k)  ✅
- [x] 抽象 `fetch_catalog(src)`,抽取 Primary / Awesome 到 adapter
- [x] `PluginSource` enum → 字符串源 id + `MarketPlugin` 增加 `confidence` / `sources` / `repo` / `verification`
- [x] `DshGet` 适配器(复用 `parse_awesome_install`,跳过 `installable:false`,兼容 `description{en,zh}`)
- [x] `LauncherSettings.plugin_sources` + `SettingsPatch` + `list_plugin_sources` 命令 + `lib.rs` 注册 + `DSHLAUNCHER_PLUGIN_SOURCES` 覆盖
- [x] 每源 last-good 磁盘缓存(`data_dir/plugin-cache/<id>.json`,失败降级不阻塞其余源)
- [x] `Market.vue` 动态源渲染;`Settings.vue` 源增删/排序/自定义 URL
- [x] i18n 双语;`api/types.ts` + `api/index.ts` mock 同步
- [x] `cargo check` + `vue-tsc --noEmit` + `vite build` 零错(129 单测全过)

### 阶段 3:GitHub topic 实时通道(C,风险最高)+ 可信度 UI  ✅
- [x] topic 搜索 client + 页级预算/退避(复用 `github_api_url`,2 页 × 100 条,3 次指数退避)
- [x] 降噪过滤 + denylist(名匹配 `dsh-*` 或探测 `package.json` 的 `dsh.bundle` / `cordis.patch.yml`,探测预算 30)
- [x] `data_dir/plugin-cache/github-topic.json` TTL 24h + 断点分页(后页失败保留已得页)+ last-good 兜底
- [x] `Unverified` 标识 + 安装前二次确认(InstallWizard)—— 市场列表红色标签 + 向导警告横幅 + 勾选后才可提交
- [x] 修复 `alpha_commit` 实时条目 repo 回查失效(`fetch_plugin_versions` / `InstallPluginInput` 新增 `repo` 提示,`resolve_repo` 统一解析)
- [x] 单测:降噪过滤 / 去重分层 / TTL 缓存 / 核心包排除 / dshget 解析

### 阶段 4:可选 — npm 反链通道 / 代码搜索
- [ ] `registry.npmjs.org/-/v1/search` + 仓库双向校验(可选)
- [ ] 代码搜索 `dsh.bundle`(需用户自备 token,列为可选项)

> **本轮不实现(有意延期)**:issue 与本文档均将阶段 4 标为「可选」,§4 验收标准亦未包含;
> 且代码搜索需用户自备 token、npm 反链与 dshget(已聚合 npm 元数据)高度重叠。
> 源注册表已可容纳该通道(新增 `SourceKind` 变体 + 一个 adapter 即可),后续可增量接入。

### 阶段 5:回归与验收  ✅
- [x] 逐条对照 §4 验收标准(结论见 §4 表)
- [x] 源离线降级 / last-good 恢复 / 不阻塞其余源的端到端验证(单测 + 三源实测)

### 阶段 6:复核收尾 — 前端缺陷修复 + CI 门禁 + 验收证据补强  ✅
- [x] 前端四处复核缺陷修复(见 §6 第 5 行):`github-topic` 源无法保存、保存期无互斥、
      保存失败丢表单、`confidence` 缺失被当可信 + 过滤 id 悬空
- [x] 对齐后端契约复核:`sanitize_plugin_sources` 对 `github-topic` 放行空 URL、
      `patchSettings` 吞错语义、`default_plugin_sources` 的 id/kind/confidence 取值
- [x] CI 门禁修复:`cargo fmt --all -- --check` 与 `clippy -D warnings` 在本分支**本就失败**
      (前几轮后端改动引入 15 处未格式化位置 + 1 处 clippy 告警,详见 §7),修复后才可能过 CI
- [x] 拆分 live 冒烟测试:`live_fetch_market`(不碰 `api.github.com`,可离线跑)与
      `live_fetch_plugin_versions`;市场半边新增「每源必须贡献」硬断言
      —— 原先源失败只 `log_warn!` 静默跳过,无任何断言能发现
- [x] 全套 CI 门禁本地复跑通过(见 §6 第 6 行)
- [x] 推送 `feat/46-plugin-sources` + 开 PR #47;修掉 CI 首轮暴露的**环境依赖测试**
      (`stale_topic_cache_*` 把「离线」写进断言),复推后 **CI 4/4 通过**(见 §6 第 7 行)

---

## 4. 验收标准(issue 原文)

| # | 验收标准 | 状态 |
| --- | --- | --- |
| 1 | 市场可切换 ≥3 个目录源(dsh-plug.in / awesome / dshget),用户可增删自定义源 | ✅ **端到端实测**:`live_fetch_market`(`--ignored`)通过,三个默认源各自贡献条目(硬断言「每个已启用源都必须在合并结果里出现自己的 source id」),全程无「获取失败」告警;设置页支持增删/排序/自定义 URL |
| 2 | 开启实时通道后能发现不在任何静态目录中的 `topic:dsh-plugin` 插件,并正确过滤噪声 | ⚠️ 实现完成 + 降噪单测覆盖(4 项);**端到端受限**:本机 `api.github.com` 不可达(需代理),未能实抓搜索结果。注:受此限制的是「版本查询」通道,`live_fetch_plugin_versions` 同样不可跑;市场三源不受影响(见第 1 条) |
| 3 | 某源不可达/离线时降级为 last-good 缓存,不阻塞其余源 | ✅ 单测 `unreachable_source_falls_back_to_last_good_cache`(无缓存时如实报错)、`stale_topic_cache_is_refetched_instead_of_served`;JoinSet 并发 + 单源失败仅告警,不阻塞其余源 |
| 4 | 未验证来源有明确标识与安装确认;绝不误装 `@deepseek-ai/*` 核心包 | ✅ 市场红色可信度标签 + 向导警告横幅 + 勾选后方可提交;`confidence` 缺失(旧缓存/第三方源)按最保守的 unverified 处理;后端 `drop_core_packages` 前置过滤,单测 `core_packages_are_never_market_entries` / `core_packages_are_dropped_from_a_source_listing` + `live_fetch_market` 实测零核心包 |
| 5 | 单测覆盖:适配器解析、降噪过滤、去重分层、TTL 缓存 | ✅ `cargo test --workspace --locked` → **134 passed / 3 ignored**(lib 137 项:134 通过 + 3 项 `--ignored` 联网冒烟);新增 15 项(适配器解析 ×2、降噪 ×2、去重分层 ×2、核心包 ×2、TTL/last-good ×2、env 解析、repo 归一化 ×2、缓存降级) |

---

## 5. 迭代轮数预算

| 阶段 | Agent 调用 | 预算轮数 | 累计 | 实际轮数 |
| --- | --- | --- | --- | --- |
| 0 现状确认 | scout×1 | 1 | 1 | 1 |
| 1 设计定稿 | oracle×1, reviewer×1 | 1 | 2 | 1 |
| 2 源注册表 + DshGet + UI | worker×2~3, reviewer×1 | 3~4 | 5~6 | 2(后端 1 + 前端 1) |
| 3 topic 通道 + 可信度 | worker×2~3, reviewer×1 | 3~4 | 8~10 | 2(+可达性修正 0.5) |
| 4 可选通道 | worker×0~1 | 0~2 | 8~12 | 0(有意延期) |
| 5 回归与验收 | worker×1, reviewer×1 | 1~2 | 9~14 | 1.5 |
| 6 复核收尾(新增) | 主 agent×1 | — | — | 1 |

**预算合计:约 10~14 轮;实际约 9 轮**(阶段 4 未做,阶段 6 为新增的收尾轮,与预算对冲)。
风险缓冲:topic 限流/代理问题可致阶段 3 +1~2 轮 —— 实际以**可达性修正**(§1.6:改走 jsDelivr,
topic 默认关闭)对冲,未超预算。

---

## 6. 回填日志(执行时填写)

| 轮次 | 阶段 | agent | 摘要 | 结果 |
| --- | --- | --- | --- | --- |
| — | 建档 | 主 agent | 依据 issue #46 + 基线 `3a802ae` 源码核验,产出本文件 | ✅ 完成 |
| 1 | 阶段 0 现状确认 | 主 agent | 确认 `3a802ae` 为 HEAD 祖先且 `plugins.rs` 零漂移;拉取 dshget `catalog.json`(2460 条 / 2.6 MB)实测结构与 install 行兼容性;盘点 `PluginSource` 全部 6 处引用 | ✅ 完成(§1.6/§1.7) |
| 2 | 阶段 1 设计定稿 | 主 agent | 定稿 `SourceKind`/`Confidence`/`PluginSourceConfig`、去重优先级、默认 4 描述符表、env 解析规则、topic 降噪 + denylist、`@deepseek-ai/*` 红线 | ✅ 完成(§2.6–§2.9) |
| 3 | 阶段 2/3 后端 | 主 agent | `config.rs` 新增源描述符与 sanitize;`plugins.rs` 重构为 adapter 注册表(primary/awesome/dshget/github-topic)、去重分层、核心包红线、last-good 缓存;`list_plugin_sources` 命令;`alpha_commit`/`do_install_plugin` 改用 repo 提示;单测扩至 45 项 | ✅ 完成(`cargo check` + `cargo test` 零错) |
| 3b | 阶段 2/3 可达性修正 | 主 agent | 实测 raw.githubusercontent/api.github.com 不可达而 jsDelivr 三节点均 200 且字节一致 → dshget 默认源与 topic 探测改走 jsDelivr;新增 `live_dshget_catalog_parses_and_filters_core` 实测通过(>1000 条、全部带 source/confidence/repo、无核心包) | ✅ 完成(§1.6 已回填) |
| 4 | 阶段 2/3 前端 | worker(并行子代理) | types/api mock/store/Market/Settings/InstallWizard/VersionPick + i18n 双语;主 agent 复核并补:源编辑后失效 `pluginSourcesLoadedAt` 缓存、mock topic id 对齐真实 `github:<owner>/<repo>` 形态、探测加 8s 超时 | ✅ 完成(`vue-tsc --noEmit` 0 错,`vite build` ✓ 13.87s) |
| 5 | 阶段 6 前端复核缺陷 | 主 agent | 复核后端契约后修 4 处:`github-topic` 源空 URL 被前端校验挡住(与 `sanitize_plugin_sources` 不一致)、源增删/排序/开关无互斥(连续点击互相覆盖)、`patchSettings` 吞错导致添加失败仍清空表单、`confidence` 缺失被当可信(向导免确认 + 无标签)。另:过滤 id 悬空清零、下拉只列启用源、mock 改读真实 settings | ✅ 完成 → 提交 `60595b1`(`vue-tsc` 0 错、`vite build` ✓ 10.5s) |
| 6 | 阶段 6 CI 门禁 + 验收补强 | 主 agent | **发现 CI 本就失败**:`cargo fmt --check` 与 `clippy -D warnings` 在前几轮后端改动中已破(未格式化宏/断言 + `migrate.rs` 一处 `unnecessary_to_owned`),PR 无法过 CI → 修复 → 提交 `d245626`。拆分 live 冒烟测试为 `live_fetch_market`(不碰 `api.github.com`,可离线跑,新增「每源必须贡献」硬断言)+ `live_fetch_plugin_versions` → 提交 `83c14ef` | ✅ 完成:fmt/clippy/`cargo test --workspace`/`vue-tsc`/`vite build` **五项全绿**;`live_fetch_market` 实测三源均贡献、零核心包 |
| 7 | 阶段 6 PR + CI 回归 | 主 agent | 推送 `feat/46-plugin-sources` 并开 PR [#47](https://github.com/dsh-plugins/dsh-launcher/pull/47)。**首次 CI 三平台全挂**:`stale_topic_cache_is_refetched_instead_of_served` 断言的是「过期缓存走 last-good 返回 stale 条目」——该分支只在网络失败时成立,本机 `api.github.com` 不可达故本地一直通过,CI runner 网络可达时 `fetch_catalog` 成功返回线上目录,断言即失败(**环境依赖测试,非 flake**)。改为对两个方向都断言真实契约(刷新成功→缓存被重写;刷新失败→文件未改写且旧载荷作 last-good),均由读回磁盘验证 → 提交 `b0f7bfa` | ✅ 完成:复推后 **CI 4/4 通过**(Quality linux/macos/windows + Integration checks)|

> 提交序列(分支 `feat/46-plugin-sources`,基线 `3a802ae`):
> `fe68db6` 阶段0/1 文档 → `8abdb30` 后端多源注册表 → `278ae71` 前端多源市场 →
> `85e78bc` 阶段5 验收回填 → `fe67ecb` 后端复核 5 处修复 → `60595b1` 前端复核修复 →
> `d245626` CI 门禁修复 → `83c14ef` live 测试拆分 → `fafbcb8` 阶段6 文档回填 →
> `b0f7bfa` topic 缓存测试去环境依赖。

> **已提 PR**:[dsh-plugins/dsh-launcher#47](https://github.com/dsh-plugins/dsh-launcher/pull/47)
> (`feat/46-plugin-sources` → `main`)。首轮 CI 三平台全挂的原因见第 7 行;修复后
> **CI 4/4 通过**(Quality linux / macos / windows + Integration checks)。
> 分支基于 PR [#44](https://github.com/dsh-plugins/dsh-launcher/pull/44)(issue #43)的
> 8 个提交构建,故 PR #47 当前显示 16 个提交;#43 合入 main 后会自动只显示 #46 增量。

---

## 7. 风险 / 开放问题

- GitHub search 限流(~30/min)与代理可达性(Launcher 全部 HTTP 走 `proxy::apply`)—— 已做页级预算(2 页 × 100)+ 三次指数退避 + last-good;**实测本机 `api.github.com` 不可达**,故 topic 通道默认关闭、需用户开启并保证代理可用。
- **jsDelivr 单点依赖(新增风险)**:dshget 默认源与 topic 探测都改走 `cdn.jsdelivr.net`。若该 CDN 在用户网络不可达,dshget 会降级为 last-good 缓存(首次运行则无缓存 → 仅剩 2 源)。缓解:用户可在设置页改回 `raw.githubusercontent.com` 或自建镜像;last-good 缓存保证已成功拉取过的用户不受影响。
- 代码搜索 API 需鉴权 token → 阶段 4 可选,本轮未实现。
- 第三方目录的许可与归属 → 保留 `sources` 归属字段(已实现,含 dshget 上游 `sources[]` 透传)。
- `plugin.dshx.dev` 无公开 JSON API(Nuxt SSR),暂不接入。
- `PluginSource` 已由封闭 enum 改为**字符串源 id**,自定义源(含镜像)天然支持,无需 `custom:` 前缀解析。
- topic 探测(`package.json` / `cordis.patch.yml`)对未按名命名的仓库存在**漏收**(探测预算 30 用尽后的候选被跳过并记日志),这是有意的限流取舍。
- **CI 门禁此前是破的(阶段 6 才发现并修复)**:`.github/workflows/ci.yml` 要求
  `cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  而本分支前几轮后端改动已引入 **15 处未格式化位置**(plugins.rs 11 + migrate.rs 4,实测
  `git show fe67ecb:...` 文件经 rustfmt 后的 hunk 数)+ 1 处 `clippy::unnecessary_to_owned`
  (`migrate.rs:201`),即**本轮之前该 PR 的 CI 必然失败**。已由 `d245626` 修复。
  教训:改后端后应本地跑完整 CI 五步,而不是只跑 `cargo test`。
- **拆分提交时漏改动(过程坑,已修正)**:用 `git diff -U1` + `git apply --cached --recount`
  暂存「格式修复子集」时,`-U1` 上下文太短,git 把格式 hunk 与相邻的自有改动合并成一个 hunk,
  `--recount` 无法还原 → 一处格式修复被静默漏进提交,最终工作树却是干净的(fmt 通过)从而掩盖问题。
  仅靠在提交态 **隔离复跑** `cargo fmt --check` 才发现(plugins.rs:4237)。已 amend 修正。
- **环境依赖测试只在 CI 暴露(PR #47 首轮 CI 三平台全挂)**:`stale_topic_cache_is_refetched_instead_of_served`
  断言「过期缓存走 last-good 返回 `github:stale/x`」——那条路径**只在网络失败时成立**。
  本机 `api.github.com` 不可达,所以本地一直绿;CI runner 网络可达,`fetch_catalog`
  成功返回线上目录,断言立刻失败。**这是环境依赖测试,不是 flake**。
  教训:凡是断言「网络失败后如何」的测试,必须要么标记 `#[ignore]`(联网冒烟),
  要么对**两个方向**都断言(成功→缓存被重写;失败→文件未改写),或改用**必然失败**
  的端点(如 `http://127.0.0.1:1/...`,连接立即被拒,与主机网络无关)。
  已由 `b0f7bfa` 按「两个方向都断言」修正,复推后 CI 4/4 通过。
