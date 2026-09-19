# Issue #49 任务树 — 完善 WSL2 实例的 DSH_HOME 可操作性

> 来源：<https://github.com/dsh-plugins/dsh-launcher/issues/49>
> 核验基线：**上游 `main` @ `9105434`**（2026-09-18 `fix(homes): unix 删除目录符号链接用 unlink`）
> 工作仓库：`D:\DSH\dsh-launcher-pr`（fork；`origin` 指向上游 `dsh-plugins/dsh-launcher`）
> 关联：issue #19（WSL2 支持，已合并）· **PR #48**（`feat/wsl-home-ops`，OPEN / **CONFLICTING**）
> 文档状态：**draft v1**。本文件既是规划文档，也是执行时回填进度的活文档（勾选框 + §6 回填日志）。

---

## 0. 结论先行

| 项 | 结论 |
| --- | --- |
| 问题是否真实存在 | ✅ **真实**。WSL 实例创建后，`InstanceEdit.vue` 对 `profiles / plugins / skills / mcp / storage / terminal` 六个标签页整页禁用；`copy_instance` 明确拒绝；`InstallWizard` / `MigratePluginsDialog` 直接过滤掉 WSL |
| 是否已有实现 | ⚠️ **是**。**PR #48** 已覆盖 issue 全部 7 条期望能力（18 文件，+597/-109），但**未合并**（OPEN）且**与 main 冲突**（`mergeStateStatus=DIRTY`） |
| 本计划的真实目标 | 不是从零实现，而是 **(a) 让 PR #48 追上 main 的 15 个提交并合入；(b) 补齐 PR #48 遗留的 6 个缺口；(c) 建立可验收的 WSL 冒烟矩阵** |
| 主要风险 | main 漂移巨大（`plugins.rs` +1359/-140、新增 `links.rs` / `migrate.rs`、`DshHome` 新增 `links` 字段）；UNC 路径上的**同步 I/O 阻塞 async runtime**；**CI 无法覆盖 WSL**（GitHub runner 无 WSL2） |
| 建议总迭代 | **6 个阶段，约 11~16 轮** agent 迭代（阶段 1 冲突合并 + 阶段 3 插件链路是核心） |

### 0.1 一句话路线图

```text
阶段 0 现状确认 → 阶段 1 追平 main（解冲突） → 阶段 2 补齐纯文件类缺口
   → 阶段 3 补齐进程类缺口（插件/终端/TUI） → 阶段 4 前端解禁 + 能力降级 → 阶段 5 验收
```

---

## 1. 源码核验明细（证据）

> 行号均为**上游 `main` @ `9105434`** 实测值（经 `gh api .../contents?ref=main` 逐文件核对）。
> 本地 `D:\DSH\dsh-launcher-pr` 的 `origin/main` 引用停在 `3a802ae`，**已过期 15 个提交**，勿作为基线。

### 1.1 前端禁用点（issue 原文所指）

| 位置（main） | 现状 | 需改为 |
| --- | --- | --- |
| `src/views/InstanceEdit.vue:1421-1427` | `v-else-if="isWsl && ['profiles','plugins','skills','mcp','storage','terminal'].includes(activeTab)"` 整页 `a-alert` 占位 | 删除占位，按各模块**真实能力**渲染 |
| `src/views/InstanceEdit.vue:1333-1340` | HOME 选择器 `:disabled="isWsl"`、专属 HOME 选项 `v-if="!isWsl"` | 保持（WSL HOME 确实不可切换），但文案保留 |
| `src/views/InstanceEdit.vue:1278` / `1327` | WSL 标签 + `wslHomeFixed` 提示 | 保留 |
| `src/views/plugins/InstallWizard.vue:40-41` | `store.instances.filter((i) => !store.homeById(i.home_id)?.wsl)` | 放开为全部实例 |
| `src/components/MigratePluginsDialog.vue:38-40` | 同上（排除 WSL 源实例） | 放开为全部实例 |
| `src/locales/zh-CN.json:438` / `en-US.json:438` | `instanceEdit.wslTabUnsupported` | 删除死键（两语言同步） |
| `src/locales/zh-CN.json:385` / `en-US.json:385` | `instanceEdit.storageDesc` 结尾「WSL HOME 暂不支持」 | 视阶段 2 的 `links.rs` 决策改写 |

### 1.2 后端"整页禁用"的成因：路径语义分裂

启动器内部对"一个实例"存在**两套路径语义**，WSL 场景下二者不同，这正是所有缺口的总根源：

| 语义 | 含义 | 当前唯一入口 | 适用场景 |
| --- | --- | --- | --- |
| **Linux 路径** | 发行版内真实路径，如 `/home/u/.dsh-launcher/homes/x` | `config::DshHome.path` / `DshVersion.dir` | 在发行版内执行命令（`wsl_bash` / `launch_script`） |
| **Windows 可视路径** | `\\wsl$\<distro>\home\u\...` UNC | **不存在统一入口**，仅 `wsl::unc_path` 被零散调用 | Windows 侧 `std::fs` 读写、`Command::current_dir`、`open::that` |

- `src-tauri/src/wsl.rs:73-76` `pub fn unc_path(distro, linux_path)` —— 唯一的映射函数，但**调用方需自行判断是否 WSL**。
- `src-tauri/src/plugins.rs:1734-1755` `resolve_instance` 返回 `(home.path.clone(), version.dir.clone())` —— **原样返回 Linux 路径**，在 Windows 侧消费即失败。
- `src-tauri/src/commands.rs:948` `resolve_instance_paths` 同样原样返回。

> **结论**：issue 的 7 条期望能力，本质是同一件事——**把 `unc_path` 收敛成一个"路径桥"，并让每个消费点显式声明自己要哪种语义**。

### 1.3 `resolve_instance` 的全部消费点（改造面实测）

| 文件:行（main） | 所属函数 | 消费方语义 | WSL 下现状 |
| --- | --- | --- | --- |
| `plugins.rs:1580` | `list_installed_plugins` | 读 profile 目录 / package.json | ❌ 失败（Linux 路径） |
| `plugins.rs:1827` | `check_plugin_updates` | 读 node_modules | ❌ 失败 |
| `plugins.rs:1897` | `set_plugins_enabled` | 读写 `cordis.patch.yml` | ❌ 失败 |
| `plugins.rs:1937` | `uninstall_plugin` | 文件准备 + 发行版内执行 | ❌ 失败 |
| `plugins.rs:2343` | `start_install_plugin_task` | 参数解析 | ⚠️ 部分 |
| `plugins.rs:2488` | `do_install_plugin` | 文件准备 + 发行版内执行 | ❌ 失败 |
| `terminal.rs:232` | `spawn_session` | PTY cwd | ❌ 失败 |
| `tui.rs:113` | `start_tui_session` | PTY cwd | ❌ 失败 |

另有 `commands.rs:983/1002/1021`（`resolve_instance_paths`）服务于 `doctor` 与健康检查。

### 1.4 "发行版内执行"路径的既有能力（PR #48 复用面）

`wsl.rs` 已具备完整的发行版内执行原语，PR #48 正是复用它们：

| 原语 | 位置（main） | 作用 |
| --- | --- | --- |
| `wsl_cmd` / `wsl_bash` | `wsl.rs:23` / `:32` | 构造 `wsl.exe -d <distro> -- ...` |
| `sh_quote` | `wsl.rs:40` | 单引号转义（脚本注入安全） |
| `wsl_output` / `wsl_test` | `wsl.rs:45` / `:62` | 一次性执行 / `test -e/-f/-d/-s` 探针 |
| `WslRoot` | `wsl.rs:124` | 解析 `$HOME/.dsh-launcher`，提供 `node_exe/pnpm_exe/pnpm_store/version_dir/home_dir` |
| `ensure_node` / `ensure_pnpm` | `wsl.rs:172` / `:239` | 发行版内 Node/pnpm 就绪（Windows 侧下载后经 stdin 灌入） |
| `launch_script` | `wsl.rs:281` | 启动包装：打印 `DSH_PID:$$` + `exec env ...` |

### 1.5 与周边机制的关系（架构红线）

| 机制 | 位置（main） | 对本次改动的影响 |
| --- | --- | --- |
| **单例/托盘/deep-link** | `lib.rs:64/88/158` | 与 WSL 无关，无影响 |
| **`AppState`** | `lib.rs:28-52` | PR #48 新增 `distro_ready` 字段做 TTL 缓存 —— 需与 main 的 `tui_sessions` 等字段**合并**（`lib.rs` 是冲突文件） |
| **存储重定向（issue #51）** | `links.rs:134,163` | **新增**，对 WSL 硬拒绝（`WSL 实例的 DSH_HOME 暂不支持存储重定向`）；且 `DshHome` 新增 `links` 字段 → **PR #48 所有 `DshHome` 字面量构造需补字段** |
| **数据目录迁移（issue #43）** | `migrate.rs`（新增 820 行） | 与 WSL 无直接交互，但 `lib.rs` 注册点冲突 |
| **进程/日志管线** | `process.rs:269-340,542-563` | WSL 启动与停止**已实现**（`launch_script` + inner PID kill），本次不动 |
| **UNC 访问前提** | `wsl.rs` | `\\wsl$\` 仅在**发行版运行中**可达 → 所有 UNC 操作前必须 `ensure_distro_running` |

### 1.6 PR #48 实测状态（关键事实）

| 项 | 值 |
| --- | --- |
| 标题 | `feat(wsl): 完善 WSL 实例创建后的 HOME 操作能力（issue #19 后续）` |
| 分支 / 提交 | `feat/wsl-home-ops` @ `ead6a23` |
| base / 状态 | base `3a802ae`（= PR #47 合并前）· `state=OPEN` · `mergeable=CONFLICTING` · `mergeStateStatus=DIRTY` |
| 规模 | 18 文件，**+597 / -109** |
| 覆盖面 | issue 7 条期望能力 **7/7 均有对应改动**（见 §2.1 对照表） |
| 自述验证 | fmt / clippy / `cargo test` 115 项 / `pnpm build` / 真机 Ubuntu 冒烟 / `tauri build --no-bundle` 均通过 |

**PR #48 与 main 的冲突面（实测）**：PR #48 触及 18 个文件，其中 **9 个**在 `3a802ae...main` 区间内被改动：

| 文件 | main 侧改动量 | 冲突风险 |
| --- | --- | --- |
| `src-tauri/src/plugins.rs` | **+1359 / -140** | 🔴 极高（PR #48 改了 192 行） |
| `src-tauri/src/commands.rs` | +166 / -39 | 🔴 高 |
| `src-tauri/src/lib.rs` | +30 / -1 | 🟠 中（`AppState` 字段 + 命令注册表） |
| `src-tauri/src/tasks.rs` | +59 / -12 | 🟠 中 |
| `src/components/MigratePluginsDialog.vue` | +48 / -13 | 🟠 中 |
| `src/views/InstanceEdit.vue` | +197 / -3 | 🟠 中（新增 storage tab） |
| `src/views/plugins/InstallWizard.vue` | +40 / -1 | 🟡 低 |
| `src/locales/zh-CN.json` / `en-US.json` | +78 / +79 | 🟡 低（但新增了 wslTabUnsupported 的邻居键） |
| `README.md` / `README.zh_CN.md` | 0 | 🟢 无 |

> **额外陷阱**：`DshHome` 在 main 上新增了 `links: BTreeMap<String,String>` 字段（`config.rs:19-23`）。PR #48 的 `commands.rs` 与 `tasks.rs` 中有 `DshHome { ... }` 字面量构造，**合并后必须补 `links: Default::default()`**，否则编译失败。同理 `tests` 中的 fixture。

---

## 2. 方案设计

### 2.1 issue 期望能力 × PR #48 覆盖 × 本计划补齐

| # | issue 期望能力 | PR #48 覆盖 | 本计划动作 |
| --- | --- | --- | --- |
| 1 | 统一路径桥 + 按需拉起发行版（TTL 缓存） | ✅ `wsl::fs_path/home_fs_path/version_fs_path` + `ensure_distro_running`（TTL 5s） | 直接采纳；**补齐未接入的消费点**（§2.2 缺口 G1/G2/G5/G6） |
| 2 | Profile 增删改查 + Web/TUI 类型识别 | ✅ 6 个 profile 命令全走 `home_fs_path` | 直接采纳 |
| 3a | 插件查看/启停/卸载 | ✅ `resolve_instance_linux` + UNC 读写 + 发行版内 CLI | 直接采纳 |
| 3b | 插件安装（npm/GitHub/本地 tgz）在发行版内执行 | ✅ `dsh_plugin_command` 走 `wsl_bash` + 发行版 pnpm/store；tgz 先复制进发行版 | 直接采纳；**修 G3 阻塞** |
| 4 | Skills 与 MCP 增删查改 | 🟡 MCP ✅（`home_path_of` 走 UNC）；**Skills 仅路径桥，git 子进程仍在 Windows 侧** | **补 G6** |
| 5 | 实例复制（WSL → 本地新 HOME） | ✅ 解除 `copy_instance` / `start_copy_instance_task` 限制，源走 UNC | 直接采纳；**补 G5 的 dshhome 导入分支** |
| 6 | 内置终端 + TUI | ✅ 终端 `wsl.exe -d <d> -- bash -i`；TUI 经 `wsl_bash` + `exec` | 直接采纳；**补 G1/G2 的探针缺口** |
| 7 | 前端解禁 | ✅ 删除整页占位 + 放开两个对话框 | **改为能力降级**（G4：storage tab 仍硬拒绝） |

### 2.2 六个待补缺口（本计划的核心增量）

> 均为**对照 PR #48 diff 与 main 源码实测**得出，非推测。

#### G1 — `terminal.rs` 探针未走发行版（🔴 高）

`terminal.rs:158` `prepare_shim` 内 `crate::process::version_bin_ready(version_dir)` 是**纯 Windows `Path::exists()` 检查**；WSL 实例的 `version_dir` 是 Linux 路径，在 Windows 上必然不存在 → **WSL 终端直接报"版本安装不完整"**。
PR #48 只改了 `spawn_session` 的 shell 选择（`shell="wsl.exe"`、`shim_dir=None`），**未改 `prepare_shim` 的调用路径**——实际 PR #48 已让 WSL 分支 `shim_dir=None` 从而**跳过** `prepare_shim`，需在阶段 2 复核确认该跳过是完整正确的（而非掩盖探针缺失）。

**动作**：WSL 分支不调用 `prepare_shim`（PR #48 已如此），但需补 `wsl_test(d, "-s", bin)` 的显式校验 + 单测。

#### G2 — `tui.rs` 的 `resolve_instance` 与 `version_bin_ready`（🟠 中）

`tui.rs:113` `resolve_instance` 返回 **UNC 路径**（PR #48 语义），`tui.rs:127` `version_bin_ready(&version_dir)` 在 WSL 下即对 UNC 路径做 `exists()` —— **UNC 在发行版未运行时返回 false**。PR #48 已插入 `ensure_distro_running`（`tui.rs` 新增），但**顺序是 `resolve_instance` → `ensure_distro_running`**，需确认 `ensure` 在 `version_bin_ready` 之前执行。

**动作**：调整顺序并补测试；同时 main 的 `tui.rs` 已重构（`inst` → `any()`、`build_env` 去重），**合并时需手工解冲突**。

#### G3 — UNC 同步 I/O 阻塞 async runtime（🔴 高）

PR #48 的 `dsh_plugin_command`（原同步 `fn`）改为 `async fn`，内部调用 `crate::wsl::wsl_test(...).await` 探针 —— 方向正确。但其余大量 `std::fs` 调用（`std::fs::create_dir_all`、`read_to_string`、`write`、`copy`、`remove_dir_all`）**仍在 `async fn` 内同步执行**，且现在目标是 **UNC 网络路径**（延迟比本地磁盘高 1~2 个数量级）。

**动作**：把 UNC 上的重 I/O（尤其 `copy_instance` 的递归拷贝、`uninstall` 的 `remove_dir_all`）包进 `tauri::async_runtime::spawn_blocking`。`tasks.rs` 的 `do_copy_instance` **已有** `spawn_blocking` 范式可照抄。

#### G4 — `links.rs` 存储重定向 tab 对 WSL 硬拒绝（🟠 中）

main 新增 `storage` tab（issue #51），`links.rs:134/163` 对 WSL 直接 `Err("WSL 实例的 DSH_HOME 暂不支持存储重定向")`。PR #48 的 `InstanceEdit.vue` 改动是**整块删除** WSL 占位（`v-else-if` 分支），删除后**没有任何 tab 级 WSL 判断残留** → 合并后 storage tab 会照常渲染给 WSL 实例，点进去被后端硬拒绝、弹错误。

**动作**：在 `InstanceEdit.vue` 的 storage 分支加**局部能力提示**（`a-alert` 而非整页禁用），保留 `links.rs` 的后端硬拒绝作为安全网；或实现 WSL 存储重定向（**建议延期**，见 §4 开放问题）。

#### G5 — `modpack.rs` 的 dshhome 导入分支未覆盖（🟠 中）

PR #48 改了 `do_import_modpack`（`modpack.rs:1679`）的 WSL 分支，但 main 上 `import_dshhome_body`（`modpack.rs:2221`，**第 2275 行**同样调用 `pnpm_install_profile`）是 **issue #51/#43 引入的新路径**，PR #48 未覆盖。同理 `export_dshhome_modpack`（`:1278`）与 `export_modpack`（`:927`）的 `home_path_of` 已由 PR #48 改走 `home_fs_path`（✅ 导出方向 OK）。

**动作**：给 `import_dshhome_body` 补 WSL 分支（复用 PR #48 的 `pnpm_install_profile_wsl`）。

#### G6 — `skills.rs` 的 git 子进程仍在 Windows 侧（🟠 中）

PR #48 只改了 `skills.rs:37` `home_path_of` → `home_fs_path`（UNC）。但 `install_skill_repo`（`:303`）会**在 Windows 侧执行 `git clone`**，目标目录是 UNC 路径：

- Windows `git.exe` 对 `\\wsl$\` 的写入**可行但极慢**，且 `git` 对 UNC 的 `chdir`/权限语义与 Linux 不同（`core.filemode`、符号链接、可执行位）；
- 若发行版未运行，`\\wsl$\` 不可达 → clone 失败。

**动作**：WSL 实例改为**在发行版内执行 `git`**（`wsl_bash` + `git clone`，目标为 Linux 路径），与 `dsh_plugin_command` 的范式一致。`list_repo_skills` / `check_skill_updates` 的 `git ls-remote` 不写盘，可保留 Windows 侧。

### 2.3 路径桥的收敛设计（采纳 PR #48，补充文档约束）

采纳 PR #48 的 `wsl.rs` 新增 API：

```rust
pub fn fs_path(distro: Option<&str>, linux: &Path) -> PathBuf;   // 统一映射
pub fn home_fs_path(home: &DshHome) -> PathBuf;                  // None → 原样
pub fn version_fs_path(ver: &DshVersion) -> PathBuf;             // Some → UNC
pub async fn ensure_distro_running(state, distro) -> Result<(), String>;  // TTL 5s
```

**命名约定（补充，防回归）**：

- 变量名以 `_fs` 结尾（`fs_home`、`src_fs`）→ **Windows 可视路径**，可传给 `std::fs`；
- 变量名以 `_linux` 结尾（`home_linux`、`linux_version`）→ **Linux 路径**，只能传给 `wsl_bash` / `launch_script`；
- `resolve_instance` = Windows 可视；`resolve_instance_linux` = Linux 路径 + distro。

**红线**：任何 `std::fs` / `tokio::fs` / `Command::current_dir` 的目标路径，必须来自 `*_fs` 或本地路径；任何进 `sh_quote` 的路径必须是 Linux 路径。二者混用是本次唯一的系统性风险。

---

## 3. 阶段任务树

### 阶段 0：现状确认（1 轮 scout）✅

- [x] 冻结基线：确认 `9105434` 为当前上游 `main` HEAD，记录 `git rev-parse origin/main`（注意本地 `origin/main` 已过期，需用 `gh api` 或重新 `fetch`）
- [x] 复核 §1.1/§1.3/§1.6 全部行号与 PR #48 状态（`gh pr view 48 --json mergeable,mergeStateStatus`）
- [x] 确认本机 WSL 环境：`wsl.exe -l -q` 是否存在可用发行版；记录发行版名与 `$HOME`
- [x] 确认本机能否构建 `src-tauri`（`cargo --version`、MSVC 工具链、`pnpm install --frozen-lockfile`）

> **本机实测（2026-09-19）**
>
> | 项 | 值 |
> | --- | --- |
> | 上游 `main` HEAD | `910543472d20f0e84b80608cc4b61514dde613af`（`gh api repos/dsh-plugins/dsh-launcher/commits/main` 与 `git ls-remote` 双向确认） |
> | PR #48 | `state=OPEN` · `mergeable=CONFLICTING` · `mergeStateStatus=DIRTY` · head `ead6a23`（2 个提交：`5bc1d72` + `ead6a23`）· base `main` |
> | 网络 | 本机 `git` 的 schannel 后端报 `SEC_E_NO_CREDENTIALS`，**必须** `-c http.sslBackend=openssl`（或走 `gh`）才能访问 GitHub |
> | WSL | ❌ **本机未安装 WSL2**：`wsl.exe -l -q` 返回 `WSL_E_DISTRO_NOT_FOUND`（系统提示去装 WSL）。→ §5.2 真机冒烟矩阵**本机无法执行**，只能以单测 + 纯函数覆盖替代（见 §6 阶段 5 记录） |
> | 工具链 | `cargo 1.97.0` · `rustc 1.97.0` · `node v24.15.0` · `pnpm 11.17.0`；`cargo check/clippy/test` 与 `pnpm build` 均可跑通 |

**出口条件达成**：基线 SHA `9105434`、PR #48 状态 `OPEN/CONFLICTING`、可用发行版名 **无**（本机无 WSL2）。

**出口条件**：基线 SHA、PR #48 状态、可用发行版名三项写入 §6 回填日志。

### 阶段 1：追平 main（解冲突）🔴 核心（2~4 轮）✅

- [x] 基于 `9105434` 新建分支（建议 `feat/49-wsl-home-ops`），把 PR #48 的 `ead6a23` **rebase 或 cherry-pick** 到新基线
- [x] 解 `plugins.rs` 冲突（+1359/-140 的 main 改动 vs PR #48 的 192 行）—— **逐函数比对**，不得整块取一边
- [x] 解 `commands.rs` 冲突（main 的 `resolve_instance_paths`/`doctor` 改动 vs PR #48 的 profile 命令改动）
- [x] 解 `lib.rs` 冲突：合并 `AppState` 字段（main 的 `links` 命令注册 + PR #48 的 `distro_ready`）
- [x] 解 `tui.rs` 冲突：main 已重构 `inst` → `any()`、`build_env` 去重；PR #48 新增 WSL 分支
- [x] 解 `tasks.rs` / `InstanceEdit.vue` / `MigratePluginsDialog.vue` / `InstallWizard.vue` / i18n 冲突
- [x] **补 `DshHome { ... }` 字面量的 `links` 字段**（`commands.rs`、`tasks.rs`、测试 fixture）—— 编译驱动，`cargo check` 全绿为止
- [x] 阶段 1 门禁：`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --workspace` + `pnpm build` 四项全绿

> **实际结果（3 个提交）**
>
> | 提交 | 内容 |
> | --- | --- |
> | `6638349` | cherry-pick PR #48 `5bc1d72`（WSL HOME 操作能力主体）；冲突 4 文件手工解 |
> | `b146e10` | cherry-pick PR #48 `ead6a23`（clippy `Some(_)` 修复）；**无冲突自动应用** |
> | `2486c92` | 补 `DshHome.links`（main `config.rs:24` 新增字段 → `wsl.rs` 测试 fixture 编译失败） |
>
> **冲突实况 vs 计划预估（重要修正）**
>
> | 文件 | 计划预估 | 实测 |
> | --- | --- | --- |
> | `plugins.rs`（+1359/-140） | 🔴 极高 | ✅ **git 自动合并成功，零冲突**（PR #48 的改动落在函数体内，与 main 的新增函数不重叠） |
> | `commands.rs` | 🔴 高 | ✅ **自动合并，零冲突** |
> | `lib.rs` | 🟠 中 | ⚠️ 冲突 ×2（`AppState` 字段 + 初始化字面量）→ 两侧都留 |
> | `tasks.rs` | 🟠 中 | ⚠️ 冲突 ×1：main 新增「统计进度心跳」vs PR #48 的 `count_tree_files`。**取 main 的心跳实现，只把计数源从 `src_home.path` 换成 `src_fs`**（保住 main 的进度反馈 + PR #48 的 UNC 语义）；`count_tree_files` 在 main 上**不存在**，PR #48 的新调用被丢弃 |
> | `tui.rs` | 🟠 中 | ⚠️ 冲突 ×1：main 的 `any()` 守卫 vs PR #48 的 `inst`/`wsl_distro`。取 PR #48 版（它是 `any()` 的超集，同时提供后续 `inst.home_id`/`inst.version_id`） |
> | `InstanceEdit.vue` | 🟠 中 | ⚠️ 冲突 ×1：删除 WSL 整页占位（PR #48 意图），保留 main 的 storage 分支 |
> | `MigratePluginsDialog.vue` / `InstallWizard.vue` / i18n | 🟡 低 | ✅ 自动合并 |
>
> **门禁实测（全绿）**：`cargo fmt --check` → exit 0 · `cargo clippy --workspace --all-targets -- -D warnings` → exit 0 · `cargo test --workspace` → **166 passed / 0 failed / 3 ignored** · `pnpm build`（`vue-tsc --noEmit && vite build`，3306 modules）→ exit 0
>
> **R1 风险结论**：实际冲突面**远小于**文档预估（计划按"9 文件高危"预算 2~4 轮，实测 1 轮内解决）。`git diff 9105434...HEAD --stat` 显示 18 文件、+604/-111，与 PR #48 原始 diff 一致，**无 main 侧回退**。

**出口条件**：分支可编译、测试全绿，且 `git diff 9105434...HEAD` 只含 WSL 相关改动（无 main 侧回退）。

> ⚠️ **过程坑预警**（源自 issue #46 的教训）：解冲突时不要用 `git checkout --ours/--theirs` 整块取边；main 侧 `plugins.rs` 有 1359 行改动，整块取边会**静默回退** main 的功能（市场多源、doctor、迁移）。逐函数比对 + `git diff 9105434...HEAD --stat` 复核。

### 阶段 2：补齐纯文件类缺口（G1/G2/G4/G5/G6）🟠（3~4 轮）✅

- [x] **G1**：`terminal.rs` WSL 分支确认跳过 `prepare_shim`；补 `wsl_test(d,"-s",bin)` 显式校验
- [x] **G2**：`tui.rs` 确认 `ensure_distro_running` 在 `version_bin_ready`/UNC 探测**之前**执行；顺序错误则调整
- [x] **G4**：`InstanceEdit.vue` storage tab 对 WSL 显示**局部能力提示**（非整页禁用）；`links.rs` 后端硬拒绝保留为安全网
- [x] **G5**：`modpack.rs` `import_dshhome_body`（`:2275`）补 WSL 分支，复用 `pnpm_install_profile_wsl`
- [x] **G6**：`skills.rs` `install_skill_repo` 在 WSL 下改为**发行版内 `git clone`**（`wsl_bash` + Linux 目标路径）
- [x] **G3（部分）**：UNC 重 I/O 包 `spawn_blocking`（`copy_instance` 递归拷贝、`uninstall` 的 `remove_dir_all`）
- [x] 单测：`fs_path` / `home_fs_path` / `version_fs_path` 双向映射（PR #48 已带 3 项，补齐边界：根路径 `/`、尾斜杠、含空格、含单引号）
- [x] 门禁四项全绿

> **实际结果**
>
> | 缺口 | 落地方式 | 关键设计 |
> | --- | --- | --- |
> | G1 | `terminal.rs` 新增 `wsl_version_bin_ready(distro, bin)` + `spawn_session` 改 `async` | WSL 分支**确实**跳过 `prepare_shim`（该函数写 Windows `dsh.cmd` 并用 `version_bin_ready` 检查 Linux 目录，必然失败）；探针改为发行版内 `test -s`，错误信息打印 **Linux** 路径（不泄露用户看不见的 UNC） |
> | G2 | `tui.rs` 顺序确认正确，加注释锁定契约 + 源码序回归测试 | `wsl_boot_precedes_unc_probe` 断言 `ensure_distro_running` 出现在 `profile_kind` / `version_bin_ready` / `wsl_test` / `spawn_command` **之前**（真机路径 CI 无法覆盖，见 R4） |
> | G4 | `InstanceEdit.vue` storage 分支加 `v-if="isWsl"` 提示 + 操作按钮 `:disabled="isWsl"`；i18n 新增 `storageWslUnsupported` | 后端 `links.rs` 硬拒绝保留为安全网；**未**实现 WSL 存储重定向（采纳 §7.2 Q1 延期建议） |
> | G5 | `import_dshhome_body` 新增 `home_linux` / `distro` 参数；`ImportModpackInput` 新增 `wsl_distro`；前端 `ModpackImportDialog` 加发行版选择器 | **复核修正**（见下）：初版只把文件路径改成 UNC，漏了"版本必须来自该发行版" |
> | G6 | `install_skill_repo` WSL 分支：`mktemp -d` → 发行版内 `git clone` → UNC 读 bundle → 复制进发行版 skills 目录 | 抽出 4 个**纯函数**脚本构造器（`wsl_clone_script` / `wsl_rev_parse_script` / `wsl_describe_script` / `wsl_rm_script`）以便单测；URL 与目录全部 `sh_quote` |
> | G3 | 新增共享 `wsl::run_blocking`；包装 `modpack` 的 4 处 tree 拷贝、`skills` 的 UNC 遍历、`plugins` 的清单读取 / 版本探测 / patch 读改写 | 说明见下 |
>
> **G5 复核修正（自审发现的真实缺陷）**：初版实现把 `home.path` 直接当 Windows 路径用（未过 `home_fs_path`），且在 WSL 目标下可能解析到**本地 Windows 的版本记录**——启动时会在发行版内引用一个不存在的 Linux 路径。已改为：先定 `distro` → 再 `resolve_import_version(.., distro)` 按 `v.wsl` 过滤 → 缺失时调 `install_version_streamed_wsl`。新增回归测试 `import_version_selection_is_distro_scoped`。
>
> **G3 范围说明（与计划原文的差异）**：计划点名 `copy_instance` 递归拷贝，但 `tasks.rs::do_copy_instance` **在 main 上已经**是 `spawn_blocking`（阶段 1 合并时保留，只把源换成 `src_fs`），无需改动。`uninstall_plugin` 的 `remove_dir_all` 经复核**不在**卸载路径上（它只读/写 `cordis.patch.yml`，实际删除由发行版内的 CLI 完成），因此改为包装真正存在的 UNC I/O：profile 清单读取、node_modules 版本探测、`cordis.patch.yml` 读改写、以及 modpack/skills 的树遍历。

### 阶段 3：补齐进程类缺口 + 插件链路复核（G3 收尾）🔴 核心（3~4 轮）✅

- [x] 逐条复核 §1.3 的 **8 个 `resolve_instance` 消费点**在 WSL 下走对了语义（Windows 可视 vs Linux）
- [x] `dsh_plugin_command` 的 `wsl_test` 探针：确认不阻塞（`async`）+ 失败信息可诊断
- [x] `start_install_plugin_file_task` 的 tgz 复制：确认 UNC 复制目标目录已 `mkdir -p`，且失败时清理发行版临时文件
- [x] `run_dsh_plugin` 的 store 一致性检测（`linked_store_dir` / `store_paths_match`）：WSL 下比对的是 **Linux store 路径**（PR #48 已传入 `linux_store`），补单测
- [x] `relink_profile_store` 的 `node_exe` 参数：确认 WSL 下为发行版 node（非 Windows node）
- [x] 补测：`forwarded_pnpm_flags` 纯函数化后（PR #48 已去掉 `State` 参数）的单测

> **逐条复核结论（§1.3 的 8 个消费点）**
>
> | 消费点 | 语义 | 结论 |
> | --- | --- | --- |
> | `plugins.rs::list_installed_plugins` | 读 profile 清单 + patch | ✅ 走 `resolve_instance`（UNC）；已移入 `run_blocking` |
> | `plugins.rs::check_plugin_updates` | 读 node_modules | ✅ 走 `resolve_instance`（UNC）；版本探测已移入 `run_blocking` |
> | `plugins.rs::set_plugins_enabled` | 读写 `cordis.patch.yml` | ✅ 走 `resolve_instance`（UNC）；读写已移入 `run_blocking` |
> | `plugins.rs::uninstall_plugin` | 文件准备 + 发行版内执行 | ✅ `resolve_instance_linux` 取 Linux 路径给 CLI；UNC 给 patch 读写；两者**未混用** |
> | `plugins.rs::start_install_plugin_task` | 参数解析 | ✅ `resolve_instance`（UNC）仅用于 `dir.exists()` 预检，正确 |
> | `plugins.rs::do_install_plugin` | 文件准备 + 发行版内执行 | ✅ `resolve_instance_linux` → `fs_home` 走 UNC、`home_path`/`version_dir` 走 CLI；命名与用法一致 |
> | `terminal.rs::spawn_session` | PTY cwd | ✅ 阶段 2 已重构为 `home_fs`（本地 cwd）+ 发行版内 bash |
> | `tui.rs::start_tui_session` | PTY cwd | ✅ 同上；`ensure_distro_running` 前置 |
>
> **其余复核项**
>
> - `dsh_plugin_command` 的 `wsl_test` 是 `async fn`，不阻塞 runtime；bin 缺失时报错含**发行版内 Linux 路径**，可诊断。
> - tgz 复制：`wsl_output(["mkdir","-p",tmp_dir])` 先建目录，再用 `uuid` 唯一化文件名避免并发同名冲突。**§7.2 Q3 已落地**：`InstallPluginInput` 新增 `wsl_scratch` 字段记录发行版内临时路径，`run_install_plugin_task` 在安装结算后（成功或失败）调用 `cleanup_wsl_scratch` 删除该文件；清理是 best-effort（失败只记 warn，不会把成功安装变成错误），且**不会**为删一个临时文件去唤醒已停止的发行版。
> - store 一致性：`linux_store` 取自 `WslRoot::pnpm_store()`（Linux 路径），与 `linked_store_dir` 读到的 `.modules.yaml` 值同属 Linux 语义 → 比对正确。新增 `store_paths_match_wsl_linux_store` 锁定（并断言 UNC 形式**不**匹配）。
> - `relink_profile_store` 的 `node_exe` 在 WSL 分支来自 `WslRoot::node_exe()`（发行版 node），非 `process::node()`。
> - `forwarded_pnpm_flags` 单测：新增 `forwarded_flags_use_the_supplied_store` 与 `forwarded_flags_registry_only_when_set`（后者含环境变量清理，避免污染其他测试）。

### 阶段 4：前端解禁与能力降级（1~2 轮）✅

- [x] 删除 `InstanceEdit.vue` 的 WSL 整页占位分支（PR #48 已删，合并后复核 storage 是否被漏掉）
- [x] 删除 i18n 死键 `instanceEdit.wslTabUnsupported`（zh-CN + en-US 同步）
- [x] `InstallWizard.vue` / `MigratePluginsDialog.vue` 放开 WSL 过滤（PR #48 已做）
- [x] **新增**：对 WSL 实例在插件安装向导中提示"依赖将在发行版内安装（拉取 Linux 二进制）"，避免用户误解
- [x] `vue-tsc --noEmit` + `vite build` 零错

> **G4 复核修正（自审发现的真实缺陷）**：初版只加了提示，但 storage tab 仍会调用 `api.listHomeLinks`，而 `links.rs::list_home_links` 对 WSL **硬拒绝**——用户每次进入该 tab 都会看到一条后端错误 toast，与"给出能力提示而非报错"的目标相反。已改为：`loadHomeLinks()` 对 WSL 直接短路返回空、表格整体不渲染（`v-if="... && !isWsl"`）、`profilesNeedHome` 兜底分支加 `v-else-if="!isWsl"`。

> **落地细节**：`InstallWizard.vue` 新增 `selectedDistro` 计算属性（经 `store.homeById(inst.home_id)?.wsl`），选中 WSL 实例时渲染 `plugins.wslInstallHint` 提示；`ModpackImportDialog.vue` 为 dshhome 形态新增发行版选择器（`onMounted` 拉 `api.listWslDistros()`，失败则静默留空，本地路径不受影响）。i18n 键 `instanceEdit.wslTabUnsupported` 全仓零引用（grep 确认）。**i18n 键对齐已校验**：zh-CN 与 en-US 各 627 个叶子键，双向差集为空。

### 阶段 5：验收（2~3 轮）

- [ ] 逐条对照 §5 验收标准（含 issue 原文 7 条）
- [ ] **真机 WSL 冒烟矩阵**（§5.2）逐项执行并留证据（日志/截图）
- [ ] 离线/发行版未运行降级验证：停掉发行版（`wsl --terminate <distro>`）后调用各命令，确认**自动拉起**或给出**可诊断错误**（而非静默失败）
- [ ] 回归：本地 Windows 实例的全部功能不受影响（`cargo test` + 手工冒烟）
- [ ] 文档回填：README 中英文 WSL 段、本文档 §6 日志

---

## 4. 迭代轮数预算

| 阶段 | Agent 调用 | 预算轮数 | 累计 |
| --- | --- | --- | --- |
| 0 现状确认 | scout×1 | 1 | 1 |
| 1 追平 main（解冲突） | worker×2~3, reviewer×1 | 2~4 | 3~5 |
| 2 纯文件类缺口 | worker×2, reviewer×1 | 3~4 | 6~9 |
| 3 进程类缺口 + 插件链路 | worker×2, reviewer×1 | 3~4 | 9~13 |
| 4 前端解禁 | worker×1 | 1~2 | 10~15 |
| 5 验收 | worker×1, reviewer×1 | 2~3 | 12~18 |

**预算合计：约 12~18 轮。** 风险缓冲：`plugins.rs` 冲突若比预期严重（+1359 行），阶段 1 可 +2 轮。

**并行化建议**：阶段 2 的 G4（前端）与 G5/G6（后端）可并行派发；阶段 3 依赖阶段 2 的路径桥稳定，**不宜并行**。

---

## 5. 验收标准

### 5.1 issue 原文 7 条对照

| # | 验收标准 | 判定方式 | 状态 |
| --- | --- | --- | --- |
| 1 | 统一路径桥：WSL 路径自动映射 `\\wsl$\`，访问前自动检测/拉起发行版（TTL 探针缓存） | 单测 `fs_path`/`home_fs_path`/`version_fs_path` 双向 + 真机 `wsl --terminate` 后自动拉起 | 🟡 **单测 ✅ / 真机不可执行**（本机无 WSL2）。新增边界单测 `fs_path_handles_boundary_linux_paths`（根 `/`、尾斜杠、空格、单引号）与 `fs_path_is_identity_for_local_homes` |
| 2 | Profile 增删改查 + Web/TUI 类型识别 | 真机：新建/复制/重命名/删除 profile，TUI profile 显示为 TUI 类型 | 🟡 代码路径已全部改走 `home_fs_path`；**真机未验** |
| 3 | 插件查看版本状态 + 启停 + 卸载 | 真机：列出已装插件、切换启用、卸载 | 🟡 语义复核 ✅（§阶段 3 表）+ 新增 store/参数单测；**真机未验** |
| 3b | 插件安装（npm/GitHub/本地 tgz）在发行版内执行，拉取 Linux 产物 | 真机：三种来源各装一次，`node_modules` 内含 `.node` Linux 二进制 | 🟡 `dsh_plugin_command` 走 `wsl_bash` + 发行版 node/pnpm/store 已确认；**真机未验** |
| 4 | Skills 与 MCP 在 WSL HOME 中增删查改（`cordis.patch.yml`） | 真机：装一个 skill 仓库、增删一个 MCP server | 🟡 **本轮补齐**：`install_skill_repo` 改发行版内 `git clone`（原为 Windows git 写 UNC）；4 个脚本构造器有单测；**真机未验** |
| 5 | 从 WSL 实例复制到本地 Windows 新 HOME，迁移 Profile 与图标 | 真机：复制后本地实例可启动，图标保留 | 🟡 `tasks.rs` 源走 `src_fs`（UNC）+ `spawn_blocking`；**真机未验** |
| 6 | 终端经 `wsl.exe -d <distro> -- bash -i`；TUI Profile 在 WSL 内 exec | 真机：终端可交互、`echo $HOME` 为 Linux 路径；TUI 可启动 | 🟡 **本轮补齐** G1/G2：终端版本探针改发行版内 `test -s`；TUI 顺序契约有回归测试；**真机未验** |
| 7 | 前端解禁：移除整页禁用占位，按能力正常呈现 | 视觉核对：6 个 tab 均可用（storage 为能力提示） | ✅ 整页占位已删、死键已清、`pnpm build` 零错；**视觉核对未做**（需运行 GUI + WSL） |

> **验收口径说明（重要）**：本机 **未安装 WSL2**（`wsl.exe -l -q` 返回 `WSL_E_DISTRO_NOT_FOUND`），§5.2 真机冒烟矩阵**在本机不可执行**。因此上表所有涉及发行版内实际行为的条目均为「代码路径复核 + 单测覆盖」而非「真机通过」。这正对应计划 §7.1 **R4**（CI/开发机均无 WSL2）。**PR 评审时应明确要求维护者在具备 WSL2 的机器上跑一遍 §5.2 矩阵**。

### 5.2 真机冒烟矩阵（阶段 5 执行）

> 前置：`wsl --terminate <distro>` 后再跑一遍"冷启动"列，验证 `ensure_distro_running`。

| 场景 | 冷启动（发行版未运行） | 热态（已运行） |
| --- | --- | --- |
| 打开实例设置（6 个 tab） | 应自动拉起发行版，无报错 | 正常 |
| 列出 Profile | 正常 | 正常 |
| 新建/复制/重命名/删除 Profile | 正常 | 正常 |
| 列出已装插件 | 正常 | 正常 |
| 启用/禁用插件 | 正常 | 正常 |
| 卸载插件 | 正常 | 正常 |
| 安装 npm 插件 | 正常，产物为 Linux 二进制 | 正常 |
| 安装 GitHub 插件 | 正常 | 正常 |
| 安装本地 tgz 插件 | 正常（tgz 先复制进发行版） | 正常 |
| 安装 Skill 仓库 | 正常（发行版内 git clone） | 正常 |
| 增删 MCP server | 正常 | 正常 |
| 复制实例到本地 | 正常 | 正常 |
| 内置终端 | 正常（bash -i，`$HOME` 为 Linux） | 正常 |
| TUI 启动 | 正常 | 正常 |
| 整合包导入到 WSL 实例 | 正常 | 正常 |
| 整合包导出（WSL HOME） | 正常 | 正常 |
| 存储重定向 tab | 显示能力提示（非报错） | 同左 |

### 5.3 回归门禁（每阶段必过）

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked
pnpm build            # vue-tsc --noEmit && vite build
```

> ⚠️ 与 issue #46 的教训一致：**改后端后必须跑完整五项，不能只跑 `cargo test`**。

---

## 6. 回填日志（执行时填写）

| 轮次 | 阶段 | agent | 摘要 | 结果 |
| --- | --- | --- | --- | --- |
| — | 建档 | 主 agent | 依据 issue #49 + 上游 `main` @ `9105434` 源码核验，产出本文件；实测 PR #48 状态为 OPEN/CONFLICTING | ✅ 完成 |
| 0 | 阶段 0 现状确认 | 主 agent | 基线 `9105434` 冻结（gh api + ls-remote 双确认）；PR #48 仍 OPEN/CONFLICTING/DIRTY；**本机无 WSL2**（`WSL_E_DISTRO_NOT_FOUND`）→ 真机矩阵不可执行；工具链 cargo 1.97/node 24/pnpm 11 可构建 | ✅ 完成 |
| 1 | 阶段 1 追平 main | 主 agent | 分支 `feat/49-wsl-home-ops` 起于 `9105434`；cherry-pick PR #48 两提交（`6638349`/`b146e10`）+ 补 `DshHome.links`（`2486c92`）。冲突仅 4 文件（`plugins.rs`/`commands.rs` **自动合并**，与预估的 🔴 极高不符）；四项门禁全绿（fmt 0 / clippy 0 / test 166 passed / pnpm build 0） | ✅ 完成 |
| 1 | 阶段 1 复核 | 主 agent | `git diff 9105434...HEAD --stat` = 18 文件 +604/-111，与 PR #48 原始 diff 吻合，无 main 侧回退；`count_tree_files` 不存在于 main，已按"取 main 心跳实现 + 换 UNC 源"处理 | ✅ 完成 || 2 | 阶段 2 纯文件类缺口 | 主 agent | G1（终端探针）/G2（TUI 顺序契约）/G4（storage 能力提示）/G5（dshhome 导入 WSL）/G6（发行版内 git clone）/G3（UNC I/O → `run_blocking`）全部落地；边界单测补齐；门禁四项全绿（fmt 0 / clippy 0 / test 179 passed / pnpm build 0） | ✅ 完成 |
| 2 | 阶段 2 复核 | 主 agent | **自审发现 G5 真实缺陷**：初版未把 `home.path` 过 `home_fs_path`，且 WSL 目标可能解析到本地 Windows 版本记录 → 已抽 `resolve_import_version(.., distro)` 按 `v.wsl` 过滤 + 走 `install_version_streamed_wsl`，补回归测试 | ✅ 已修 |
| 3 | 阶段 3 进程类/插件链路 | 主 agent | §1.3 的 8 个消费点逐条复核（结论见阶段 3 节）；新增 `store_paths_match_wsl_linux_store` / `forwarded_flags_*` 单测；门禁全绿（test 179 passed） | ✅ 完成 |
| 4 | 阶段 4 前端解禁 | 主 agent | 整页占位已删、`wslTabUnsupported` 死键零引用、向导新增 WSL 提示、整合包导入新增发行版选择器；i18n 键双向对齐校验通过（627/627）；`pnpm build` 0 | ✅ 完成 |
| 5 | 阶段 5 验收 | 主 agent | §5.1 逐条对照（见 5.1 表）；README 中英文 WSL 段更新；**真机矩阵不可执行**（本机无 WSL2，R4）→ 已明确标注为评审方待办 | ⚠️ 部分（真机待外部执行） |

---

## 7. 风险 / 开放问题

### 7.1 已识别风险

| # | 风险 | 等级 | 缓解 |
| --- | --- | --- | --- |
| R1 | PR #48 与 main 冲突（`plugins.rs` +1359 行），解冲突可能静默回退 main 功能 | 🔴 高 | 逐函数比对 + `git diff 9105434...HEAD --stat` 复核；不用整块取边 |
| R2 | `DshHome` 新增 `links` 字段 → PR #48 字面量构造编译失败 | 🟠 中 | 编译驱动补齐；`cargo check` 全绿为准 |
| R3 | UNC 同步 I/O 阻塞 async runtime（列表类命令卡 UI） | 🔴 高 | 重 I/O 包 `spawn_blocking`；列表类优先走 `wsl_output` 一次性执行 |
| R4 | **CI 无法覆盖 WSL**：GitHub runner（linux/macos/windows）**均无 WSL2**，所有 WSL 路径只能靠单测 + 手工冒烟 | 🔴 高 | 把可测逻辑（路径映射、参数构造、脚本生成）抽成**纯函数**加单测；进程类走 §5.2 手工矩阵 |
| R5 | `\\wsl$\` 在发行版未运行时不可达 → 静默失败 | 🟠 中 | 所有 UNC 操作前 `ensure_distro_running`；TTL 5s 缓存防抖动 |
| R6 | WSL 实例的 `doctor` 依赖自检（`commands.rs:983/1002`）走 `resolve_instance_paths` | 🟡 低 | 阶段 3 复核；UNC 读取可行，但需确保发行版运行 |
| R7 | main 持续前进，PR #48 越拖越难合并 | 🟠 中 | 阶段 1 尽快执行；合并后立即 rebase |

### 7.2 开放问题（需产品决策）

- **Q1**：存储重定向（`links.rs`）是否要支持 WSL HOME？实现需在发行版内建符号链接（`ln -s`）+ 校验，工作量约 1~2 轮。**建议：本轮延期**，UI 给能力提示即可。
- **Q2**：WSL 实例能否切换 DSH_HOME？当前 `InstanceEdit.vue:1333` 禁用。技术上需要跨发行版迁移 HOME，**建议：保持禁用**。
- **Q3**：`start_install_plugin_file_task` 复制进发行版的 tgz 是否需要清理策略？当前留在 `~/.dsh-launcher/tmp`。**建议**：加 TTL 清理或安装成功后删除。 → **✅ 已实现「安装成功后删除」**（见阶段 3 节）。
- **Q4**：WSL 实例的 alpha（源码构建）版本支持？`tasks.rs:542` 明确不支持。**建议**：保持，issue 未要求。

### 7.3 关键路径

```text
阶段1(解冲突) ──→ 阶段2(纯文件类) ──→ 阶段3(进程类) ──→ 阶段4(前端) ──→ 阶段5(验收)
      │                  │                 │
      └─ R1/R2 阻塞       └─ G4/G5/G6       └─ G3 收尾
```

**最可能拖慢进度的三项**：R1（冲突规模）、R3（UNC 阻塞）、R4（CI 不可覆盖 → 只能手工验收）。
