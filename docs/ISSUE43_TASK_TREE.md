# Issue #43 任务树 — 支持修改启动器数据目录位置

> 来源:<https://github.com/dsh-plugins/dsh-launcher/issues/43>
> 核验基线:`origin/main` @ `3a802ae`(2026-09 之后无相关改动)
> 文档状态:draft v1。本文件既是规划文档,也是执行时回填进度的活文档。

---

## 0. 结论先行

| 项 | 结论 |
| --- | --- |
| 问题是否真实存在 | ✅ 真实。`lib.rs:121` 硬编码 `app.path().app_data_dir()`,数据目录固定为 `%APPDATA%\in.dsh-plug.dsh-launcher`,无可配置覆盖机制;设置页仅有查看按钮 |
| 是否可做 | ✅ 可行,但**不能运行中热迁移**,必须走「写入新位置 → 重启 → 启动迁移」流程 |
| 主要风险 | 鸡生蛋(新位置存哪)、日志句柄占用、versions/homes 被运行实例占用、deep-link/单实例/托盘的自举顺序 |
| 建议总迭代 | 6 个阶段,约 **8~11 轮 agent 迭代**(其中阶段 2 迁移引擎是核心) |

---

## 1. 源码核验明细(证据)

### 1.1 数据目录的固定方式

- `src-tauri/src/lib.rs:121` — `let data_dir = app.path().app_data_dir()?;`
  - Tauri 2 在 Windows 上解析为 `%APPDATA%\{identifier}`,identifier 见 `tauri.conf.json`:`in.dsh-plug.dsh-launcher`
  - 写入 `AppState { data_dir, config_path, ... }` 后全程不可变
- 全仓库 `data_dir` 引用 **48 处**,核心消费方:
  - `config.json`:`lib.rs:126` + `config::load_config`
  - `logs/latest.log`:`applog::init`(`applog.rs:56`,`lib.rs:134`)持有**文件句柄**
  - `versions/<version>`:`tasks.rs:929,1078`(安装/删除版本)
  - `homes/<name>`(专属 HOME 默认路径):`commands.rs:72 default_dedicated_home_path`
  - `.pnpm-store`:`tasks.rs:931,1081`、`plugins.rs:1633,1859`、`modpack.rs:1412`
  - `tools/node`(managed node,issue #23):`runtime.rs:56,401,415`
  - `icons/`:`commands.rs:1280`;运行日志:`process.rs:384`、`tui.rs:245`、`terminal.rs:154`

> **阶段 0 复核修正(2026-09 基线 `3a802ae`)**:以下行号为实际值,原文编号仅作参考。
> - `lib.rs:122` `let data_dir = app.path().app_data_dir()?;`,`lib.rs:123` create_dir_all、`:126` ensure_local_node_on_path、`:127` config_path、`:135` applog::init、`:146` 写入 AppState
> - `tasks.rs:960,1109`(`versions/<version>` 安装/删除)、`tasks.rs:962,1112`(`.pnpm-store`)、`tasks.rs:1298`(tools)
> - `plugins.rs:2051,2277`(`.pnpm-store`)、`modpack.rs:1483`(`.pnpm-store`)
> - `runtime.rs:57` `local_node_dir`、`runtime.rs:402` tools、`runtime.rs:416` local_node_dir
> - `commands.rs:77` homes、`commands.rs:426` data_dir、`commands.rs:1025` get_launcher_directory、`commands.rs:1179` open_launcher_directory、`commands.rs:1192` open_launcher_log、`commands.rs:1196,1215` logs、`commands.rs:1382` icons
> - `process.rs:384`、`tui.rs:434`、`terminal.rs:154`(`data_dir.join("bin")`/logs)
> - `AppState.data_dir` 写后不可变(仅在 setup 赋值),全部消费方约 27 处 `state.data_dir` + 各处形参传递
> - 依赖:`tauri-plugin-dialog = "2"`(Cargo.toml:16)已在;`walkdir`/`fs_extra` **未引入**,迁移复制用 std::fs 递归即可(阶段 2 确认)
> - UI:`Settings.vue:479-484` 数据目录卡片;`api/index.ts:754-763,1326-1330` 命令映射;`zh-CN.json:279`/`en-US.json:279` `dataDir`
> - `invoke_handler` 注册点 `lib.rs:182`;single_instance `lib.rs:64`、deep-link `lib.rs:88`、托盘 `lib.rs:158`

### 1.2 现有 UI / 命令状态

- `Settings.vue:435-443`:「数据目录」卡片 = 路径展示 + `打开目录` + `查看日志` 两个按钮
- 命令:`get_launcher_directory`(commands.rs:920)、`open_launcher_directory`(commands.rs:1075)、`open_launcher_log`(commands.rs:1090)
- i18n `zh-CN.json:219-225` 的 `settings.dataDir.*` 无「更改位置」键
- 已具备的现成依赖:`tauri-plugin-dialog`(Cargo.toml:16)可复用做目录选择器

### 1.3 与周边机制的关系(架构红线)

| 机制 | 位置 | 对迁移的影响 |
| --- | --- | --- |
| 单实例插件 `single_instance` | lib.rs:64 | 第二个进程直接退出,迁移必须由**已运行的主进程**发起重启 |
| deep-link `dsh-launcher://` | lib.rs:88 | 自举时路径解析须在 deep-link 注册之后、配置加载之前完成,顺序敏感 |
| 托盘 + 关闭到托盘 | lib.rs:158 | 迁移写 pending 后用户可能只是关窗不退出 → UI 须给明确"重启生效"提示 |
| managed node | runtime.rs:56 | node 目录在 data_dir/tools 下,迁移时可能被本进程使用,须一并复制 |

---

## 2. 方案设计(采纳的路径)

### 2.1 自举顺序(鸡生蛋解法)

新位置**不能**存在 `config.json` 里(config 本身在新目录里)。采用优先级链:

```text
1. 环境变量  DSH_LAUNCHER_DATA_HOME      (便携/多机场景,最高优先)
2. 指针文件  <默认app_data_dir>\data-home.txt   (UI 迁移写出的目标)
3. 默认位置  %APPDATA%\in.dsh-plug.dsh-launcher (兜底)
```

- 启动时解析顺序:`lib.rs` setup 中,**先**读 env → 读指针文件 → fallback 默认。
- `AppState.data_dir` 仍是不变字段,所有 48 处消费方**零改动**。
- UI「更改位置」只做:**校验新目录 → 写入指针文件(pending)→ 提示重启**。不做任何文件移动。

### 2.2 迁移流程(重启后,power-failure 安全)

迁移在**重启后的启动阶段**执行,此时本进程尚未持有日志句柄、无实例运行:

```text
启动 → 解析 data_dir(env/指针/默认)
  → 发现 指针文件指向 ≠ 当前 data_dir
  → 进入迁移态:
     1. 创建新目录 + 写入 MIGRATION_IN_PROGRESS 标记
     2. 逐项复制: config.json / versions/ / homes/ / logs/ / .pnpm-store/ / tools/ / icons/
        使用 Windows 语义复制(fs_extra 或 walkdir+tokio),遇失败记录路径
     3. 校验: 比对文件数+总大小;新 config.json 可解析;新 tools/node 可执行
     4. 切换: 把指针文件内容更新为「已确认」;AppState.data_dir 立即指向新目录
     5. 清理: 删除旧目录(保留 30 天窗口:旧目录重命名为 <dir>.old-<ts> 而非直接删)
     6. 任一前置步骤失败 → 删除新目录 + 回退指针文件 → 用旧目录正常启动
```

### 2.3 失败兜底(issue 需求 3)

- 启动时若解析出的 data_dir 不可读/不可写(掉盘、权限变化)→ 回退默认目录并弹窗提示,不崩溃
- 迁移校验失败 → 自动回滚旧目录,指针文件还原,日志记录原因
- 旧目录保留 `.old-<ts>` 快照,用户可在 UI「恢复上一数据目录」手动回退

### 2.4 环境变量(issue 需求 4,进阶项)

`DSH_LAUNCHER_DATA_HOME` 支持便携化:优先于指针文件;设置页显示当前生效来源
(「环境变量 / 已迁移 / 默认」)。此需求独立于迁移 UI,可拆为 P4 单独交付。

---

### 2.5 设计定稿(阶段 1,主 agent 直接定稿)

**命令签名**(Rust 侧,全部返回 `Result<String | DataDirInfo, String>`):

```rust
/// 打开目录选择器;用户取消返回空字符串。
#[tauri::command]
pub fn pick_data_dir(app: AppHandle) -> Result<String, String>

/// 校验新目录并写入指针文件(不移动任何文件)。
/// 拒绝:路径为空/不存在/不可写/等于当前 data_dir。
/// 成功返回新路径;指针文件写入后 UI 提示「重启生效」。
#[tauri::command]
pub fn commit_data_dir(state: State<'_, AppState>, path: String) -> Result<String, String>

/// 返回当前 data_dir 与来源;source ∈ "env" | "pointer" | "default"。
#[tauri::command]
pub fn get_data_dir_source(state: State<'_, AppState>) -> Result<DataDirInfo, String>
```

补充命令(阶段 4 复用):`relocate_fallback` 不需要;env 分支由 `resolve_data_dir` 内置,
`get_data_dir_source` 直接暴露来源即可覆盖「设置页显示来源」需求。

**迁移状态机终稿**(在 setup 前段、`applog::init` 之前**同步**执行):

```text
resolve_data_dir(app):
  target = env DSH_LAUNCHER_DATA_HOME           // 最高优先,不迁移、不改指针
        or read_pointer(<default>)\data-home.txt  // UI 迁移写出
        or <default>                              // 兜底
  pointer_pending = (pointer_file 存在且 target==指针值 且 target != 当前生效目录)

启动迁移(仅当 pointer_pending):
  1. 若 <target> 存在 MIGRATION_IN_PROGRESS → 上次迁移被杀:删 <target> 重来
  2. 创建 <target> + 写 MIGRATION_IN_PROGRESS(内容=目标路径)
  3. 逐项复制:config.json、versions/、homes/、logs/、.pnpm-store/、tools/、icons/(std::fs 递归,无 walkdir 依赖)
  4. 校验:文件数+总大小一致;新 config.json 可解析;tools/node 存在
  5. 切换:删除 MIGRATION_IN_PROGRESS;主进程 data_dir 指 <target>;指针文件更新为「已确认」(发 events,前端 toast)
  6. 清理:旧目录重命名 <old>.old-<ts>(不删除,30 天窗口由用户手动清理)
  7. 任一步失败 → 删除 <target> + 还原指针 → 继续用旧目录启动

指针文件格式:`data-home.txt` 单行 UTF-8 路径。
UI 改动(阶段 3):commit 后 Modal「重启生效」;启动后若 source=="pointer" 且迁移曾发生 →
一次性 toast「数据目录已迁移到 <path>」;「恢复上一数据目录」按钮列出 *.old-* 目录可回滚(阶段 3 范围外,键位保留)。

**i18n 键位**(zh-CN / en-US 同步):
settings.dataDir.moveTo           更改位置
settings.dataDir.restartHint      更改将在重启后生效,重启时自动迁移现有数据
settings.dataDir.migrating        正在迁移数据目录…
settings.dataDir.migratedToast    数据目录已迁移到 {0}
settings.dataDir.sourceEnv        环境变量
settings.dataDir.sourcePointer    已迁移
settings.dataDir.sourceDefault    默认
settings.dataDir.rollback         恢复上一数据目录


---

## 3. 子 agent 调用与使用规范

本仓库目录:`D:\DSH\dsh-launcher-pr`(fork,origin 指向上游)。子 agent 一律
在该仓库内工作,遵守仓库 AGENTS 规则与 Windows 环境(WSL 优先,但 Rust 构建
依赖 Windows 原生工具链,pwsh 辅助)。

### 3.1 可用 agent 角色与选用准则

| 角色 | 内置名(别名) | 用途 | 本项目选用准则 |
| --- | --- | --- | --- |
| Worker | `worker`(implementer/coder/developer) | 实现代码 | **阶段 2~5 主实现**,长上下文继承(fork) |
| Oracle | `oracle`(advisor) | 决策一致性审查,防止漂移 | 阶段 1 设计定稿 + 阶段 2 中途架构校验 |
| Reviewer | `reviewer` | diff/方案/PR 审查 | 每个阶段完成后的验收审查 |
| Scout | `scout` | 快速代码侦察 | 阶段 0 收尾/对陌生子模块摸底 |
| Researcher | `researcher` | 网络调研 | 可选项:Tauri 目录 API/跨盘移动兼容性调研 |
| Delegate | `delegate` | 轻量杂务 | 本任务不用,避免无意义调用 |

### 3.2 调用规范(硬性要求)

1. **一次调用一个阶段目标**:不给 worker 塞多个阶段的活;阶段间必须过 review 再进下一轮。
2. **worker 必须带齐全上下文**:任务描述里写入
   `data_dir 48 处引用清单`(见 1.1)、`自举优先级链`(2.1)、`迁移状态机`(2.2),
   禁止让子 agent 重新考古。
3. **提交约束**:worker 只允许改 `src-tauri/src/*.rs`、`src/views/Settings.vue`、
   `src/locales/*.json`;`lib/`、`node_modules/`、`dist/` 禁止入库;
   **绝不 `git add .`**,只暂存显式路径;不主动 commit/push(用户明确要求除外)。
4. **验证纪律**:每个 worker 任务交付前必须跑 `cargo check` + `pnpm build`(或
   `npx tsc --noEmit`),并把输出贴回;reviewer 核查构建通过后才算接受。
5. **reviewer 验收标准**:功能性满足 issue 的 4 点要求、无回退风险、i18n 双语文案、
   内存/路径处理正确(Windows 大小写不敏感、UNC/长路径)。
6. **oracle 调用时机**:仅在设计阶段(阶段 1)与迁移状态机编码前(阶段 2 前段)
   调用;日常迭代不需要,避免 token 浪费。
7. **wsl-first 说明**:本机 WSL 优先;但 Rust 工具链若在 WSL 内可用则统一 WSL,
   否则用 `pwsh -NoProfile -Command "cargo ..."` 并在交付说明中注明。

---

## 4. 阶段任务树

### 阶段 0:现状确认(1 轮 scout)

- 目标:冻结基线,产出本文件 §1 的证据核对(defer 到 scout 复核最新 main)
- 任务:
  - [x] 确认 `origin/main` 最新提交与 3a802ae 无数据目录相关改动
  - [x] 复核 §1 引用的行号仍准确(改动即更新)
  - [x] 明确 `tauri-plugin-dialog` + `process.rs` running map 的可复用接口
- 交付:本文件 v1 修订;验收人:主 agent(免 review)

### 阶段 1:设计与定稿(1 轮 oracle + review)

- 目标:§2 方案正式定稿,拆出可执行任务清单
- 任务:
  - [x] 确认自举优先级链与迁移状态机的最终形态
  - [x] 定义新命令签名:`pick_data_dir`、`commit_data_dir`、`get_data_dir_source`
  - [x] 定义 i18n 键位:settings.dataDir.moveTo / restartHint / migrating / source.*
- 交付:设计定稿 + 任务拆解;验收人:oracle + reviewer

### 阶段 2:核心 — 路径解析与迁移引擎(2~3 轮 worker + 1 轮 review)

- 目标:实现启动自举 + 迁移状态机,Rust 侧全部完成
- 轮次拆解:
  - [x] 轮 2a:完成 `migrate.rs` 状态机 + `bootstrap(app)` 自举(env→指针→默认),
    `AppState` 增加 `data_dir_source` + `data_dir_notice`
  - [x] 轮 2a 后:状态机边界审定(失败回滚、`.old-<ts>` 快照、标记文件、指针存活)
  - [x] 轮 2b:完整迁移实现(复制/校验/切换/快照/回滚)+ `pick_data_dir` / `commit_data_dir` / `get_data_dir_source`
  - [x] 轮 2c:边缘处理(目标已有文件、权限拒绝、env 分支不迁移、启动时断点清理)
  - [x] 轮 2d:全量 review,`cargo check` + 4 单元测试通过
- 交付:`resolve_data_dir` + `migrate.rs` + 3 命令;验收人:reviewer

### 阶段 3:UI 与 i18n(1 轮 worker + review)

- 目标:设置页完成「更改位置」交互
- 任务:
  - [x] `Settings.vue` 数据目录卡片增加「更改位置」按钮 + 重启提示 Modal
  - [x] 迁移进行中的进度展示(来源标识 + 成功/失败提示;设计裁定为启动时一次性迁移,无长任务轮询)
  - [x] zh-CN / en-US 双语文案
- 交付:UI 改动;验收人:reviewer(检查 i18n 完整性)

### 阶段 4:环境变量支持(1 轮 worker,可选拆分)

- 目标:`DSH_LAUNCHER_DATA_HOME` 生效 + 设置页来源标识
- 说明:若阶段 2 的 `resolve_data_dir` 已内置 env 分支,本阶段仅剩 UI 标识,
  可并入阶段 3(视阶段 1 定稿结果)
- **裁定结果:并入阶段 2/3 完成**(bootstrap 内置 env 分支且不迁移;设置页来源标识)
- 任务:
  - [x] `bootstrap` env 分支:env 优先、不可用时回退默认并提示(migrate.rs:107)
  - [x] 设置页显示当前生效来源(env / pointer / default)

### 阶段 5:回归与打包(1 轮 worker + 1 轮 review)

- 目标:全量回归 + 可执行产物验证
- 任务:
  - [ ] 手动场景:迁移成功 / 迁移中杀进程(重试恢复)/ 新目录掉盘(回退默认)
  - [ ] 老数据兼容:旧版本 config.json 无指针文件时正常启动
  - [ ] `pnpm build` → `tauri build` 产物在本机构建通过
- 交付:测试记录 + 构建产物;验收人:reviewer + 主 agent 汇总

---

## 5. 预期目标与验收标准(issue 4 点需求)

| issue 需求 | 验收标准 |
| --- | --- |
| 1. UI 入口 | 设置页「数据目录」卡片可打开目录选择器并更改位置 |
| 2. 自动迁移 | 重启后自动完成复制→校验→切换→清理;任一步失败回滚不损坏原配置 |
| 3. 异常兜底 | 新目录不可用时回退默认目录并明确提示,不崩溃 |
| 4. 环境变量(进阶) | `DSH_LAUNCHER_DATA_HOME` 可覆盖默认位置,设置页显示来源 |

附加验收:48 处 `data_dir` 消费者行为不变;`cargo check` + `tsc` 零告警;
迁移前后启动器版本、实例列表、插件状态完全一致。

---

## 6. 迭代轮数预算

| 阶段 | Agent 调用 | 轮数 | 累计 |
| --- | --- | --- | --- |
| 0 现状确认 | scout×1 | 1 | 1 |
| 1 设计定稿 | oracle×1, reviewer×1 | 1 | 2 |
| 2 迁移引擎(核心) | worker×2~3, oracle×1, reviewer×1 | 3~4 | 5~6 |
| 3 UI 与 i18n | worker×1, reviewer×1 | 1~2 | 6~8 |
| 4 环境变量 | 视阶段 1 定稿,worker×0~1 | 0~1 | 7~9 |
| 5 回归与打包 | worker×1, reviewer×1 | 1~2 | 8~11 |

**合计:8~11 轮**(worker 类 5~7 轮,oracle 2 轮,reviewer 4~5 轮)。
风险缓冲:若迁移引擎出现平台特有坑(长路径/权限),阶段 2 最多 +1 轮。

## 7. 回填日志(执行时填写)

| 轮次 | 阶段 | agent | 摘要 | 结果 |
| --- | --- | --- | --- | --- |

| 0 | 现状确认 | scout(主 agent) | 基线 3a802ae 无数据目录改动;行号修正;dialog 插件已在 | ✅ 完成 |
| 1 | 设计定稿 | oracle/reviewer(主 agent 替代) | 命令签名、迁移状态机终稿、i18n 键位、UI 裁定 | ✅ 完成 |
| 2 | 迁移引擎 | worker(主 agent) | migrate.rs + bootstrap + 3 命令;cargo check 零告警、4 测试通过 | ✅ 完成 |
| 3 | UI 与 i18n | worker(主 agent) | Settings.vue 更改位置交互 + 来源标识 + Modal;api 3 命令;tsc/pnpm build 零错 | ✅ 完成 |
| 4 | 环境变量 | (并入阶段 2/3) | bootstrap env 分支 + UI 来源标识,已交付 | ✅ 完成(并入) |