# Issue #74 独立验证报告 — prune_auth_cookies 前缀匹配修复

> 验证者：teammate `verifier`（独立于实现者 `rust-fixer`）
> 共享任务：task-2（owner: verifier）
> 仓库：`D:\DSH\DSH-Launcher`，分支 `fix/74-prune-auth-cookies`，HEAD `c207edf`
> 设计依据：`D:\DSH\docs\issue-74-fix-plan.md`（尤其"五、验收标准"）
> 验证时间：2026-10-01 01:07–01:20 (Asia/Shanghai)
> 工具链：cargo/rustc 1.97.0（`C:\Users\yuki\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin`）、Node v24.15.0、pnpm 11.21.0

**写权限声明**：本次验证唯一的写入是新建本文件。全程未修改 `src-tauri/src/**` 或 `src/**` 的任何内容（变异检查为临时改动，已按字节还原，见第 3 节）。

---

## 0. 结论摘要

| # | 验证项 | 结果 |
|---|--------|------|
| 1 | `cargo fmt --all -- --check` | ✅ EXIT=0 |
| 2 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | ✅ EXIT=0 |
| 3 | `cargo test --workspace --locked` | ✅ EXIT=0，226 passed / 0 failed / 3 ignored |
| 4 | **变异检查（核心证据）** | ✅ 变异体**编译通过**且目标测试**失败** `left: 1, right: 3`；还原后逐字节一致 |
| 5 | 还原后文件完整性 | ✅ SHA256 与 git blob 均与变异前完全一致 |
| 6 | 对抗性边界评审 | ✅ 无过度删除；发现 1 项设计取舍（见 4.4） |
| 7 | diff 范围 | ⚠️ **变更尚未提交**，`main..HEAD` 为空；工作树改动仅 `src-tauri/src/windows.rs` |
| 8 | CI 矩阵一致性 | ✅ quality 四条命令逐字一致；integration 三条脚本本地全通过 |
| 9 | `pnpm build`（前端门） | ⚠️ `pnpm build` 因**本机离线**失败（EXIT=1）；绕过 pnpm 直接跑两步 `vue-tsc`+`vite build` **均 EXIT=0**，见第 7 节 |
| 10 | **发现的环境陷阱** | ⚠️ 还原后 cargo 复用旧二进制导致**假失败**，见第 6 节（重要） |

**发现的问题**：无源码缺陷。2 项非代码问题：变更未提交（G-1），以及一个足以误判验证结论的构建缓存陷阱（G-2）。

---

## 1. 验证方法与独立性

- 全程使用 `pwsh`，每次 **仅运行一条 cargo 命令**（避免相互占用 target/package-cache 锁）。
- 所有命令在 `D:\DSH\DSH-Launcher` 下执行，PATH 前置 `$stable`。
- 未采信 Lead 或实现者给出的任何结论；下表所有退出码均为本次实测。

---

## 2. 三道 Rust 门禁（还原态源码，最终确认）

```powershell
$stable = "C:\Users\yuki\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$env:Path = "$stable;$env:Path"
Set-Location D:\DSH\DSH-Launcher
```

### 2.1 `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`

```
FMT_EXIT=0
FMT_ELAPSED_MS=463
CARGO_VERSION=cargo 1.97.0 (c980f4866 2026-06-30)
RUSTC_VERSION=rustc 1.97.0 (2d8144b78 2026-07-07)
```

无任何输出（格式检查通过时 cargo fmt 不打印内容）。

### 2.2 `cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings`

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.65s
CLIPPY_EXIT=0
CLIPPY_ELAPSED_MS=876
```

**这一条直接回应计划书 §四 的未决项**："`name`/`authority` 改动后是否 unused 必须由 clippy 裁决"。`-D warnings` 下零告警 ⇒ 实现者确实**干净地删除了**原精确名计算，而非用 `let _ = &name;` 掩盖。已人工复核 `prune_auth_cookies` 闭包体：`authority`/`name` 均已不在生产路径中。

还原后强制重编译再跑一次（排除缓存假绿）：

```
    Checking dsh-launcher v0.2.7 (D:\DSH\DSH-Launcher\src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.88s
CLIPPY_EXIT=0
```

### 2.3 `cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked`

```
running 229 tests
test result: ok. 226 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 4.23s

     Running unittests src\main.rs (src-tauri\target\debug\deps\dsh_launcher-a0e8cd57fd994615.exe)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests dsh_launcher_lib
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

TEST_EXIT=0
TEST_ELAPSED_MS=5775
```

`windows::tests` 4/4 全绿：

```
test windows::tests::should_prune_hits_every_authority_a_port_ever_minted ... ok
test windows::tests::loopback_origin_accepts_only_plain_loopback_http ... ok
test windows::tests::dsh_auth_cookie_name_follows_the_origin_authority ... ok
test windows::tests::should_prune_leaves_foreign_cookies_alone ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 225 filtered out
```

> 注：3 个 ignored 是 `plugins::tests::live_*`（真实网络用例），与本次改动无关。

与 Lead 自报的门禁结果**完全一致**（FMT=0 / CLIPPY=0 / TEST=0，226 passed）。

---

## 3. 变异检查（本次验证的核心证据）

**目的**：证明新测试真的能区分新旧行为。一个在修复前后都通过的测试不能证明任何事。

### 3.1 变异前的基线快照

```
GITBLOB_PRE_MUTATION = 233f3aaaa583b36548654580881cd0fbedde6938
SHA256_PRE_MUTATION  = 966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196
```

**双重备份**（该文件是未提交的工作成果，必须先备份）：

| 备份 | 路径 | SHA256 |
|---|---|---|
| 仓库内 | `D:\DSH\DSH-Launcher\.dsh-verify\windows.rs.bak` | `966591C7…D196` |
| 仓库外 | `%TEMP%\issue74_windows.rs.233f3aa.bak` | `966591C7…D196` |

仓库外备份刻意以 git blob 前 7 位命名，便于交叉核对来源。

### 3.2 变异体：还原为"只精确匹配当前 authority"

把 `should_prune` 改回修复前的精确名匹配语义（用测试中"当前 authority" `127.0.0.1:52311`）：

```rust
fn should_prune(name: &str, domain: Option<&str>, host: &str) -> bool {
    use base64::Engine;
    use sha2::Digest;
    let authority = format!("{host}:52311");
    let expected = format!(
        "dsh-auth-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(authority.as_bytes()))
    );
    name == expected && domain == Some(host)
}
```

> 方法学说明：首版变异体因缺少 `use base64::Engine; use sha2::Digest;` 而**编译失败**（E0599）。编译失败的变异体不构成有效证据，故补齐 trait 导入后重跑，确保变异体**确实编译通过**——否则"测试失败"可能只是构建错误。

变异体格式合法性预检（避免误判）：`MUTANT_FMT_EXIT=0`。

### 3.3 变异体上的测试结果 —— 预期失败，实测失败

```
   Compiling dsh-launcher v0.2.7 (D:\DSH\DSH-Launcher\src-tauri)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 16.50s
     Running unittests src\lib.rs

running 4 tests
test windows::tests::loopback_origin_accepts_only_plain_loopback_http ... ok
test windows::tests::dsh_auth_cookie_name_follows_the_origin_authority ... ok
test windows::tests::should_prune_leaves_foreign_cookies_alone ... FAILED
test windows::tests::should_prune_hits_every_authority_a_port_ever_minted ... FAILED

failures:

---- windows::tests::should_prune_hits_every_authority_a_port_ever_minted stdout ----
thread 'windows::tests::should_prune_hits_every_authority_a_port_ever_minted' panicked at src\windows.rs:496:9:
assertion `left == right` failed: every dsh-auth-* cookie on the host is pruned
  left: 1
 right: 3

---- windows::tests::should_prune_leaves_foreign_cookies_alone stdout ----
thread 'windows::tests::should_prune_leaves_foreign_cookies_alone' panicked at src\windows.rs:514:9:
assertion failed: should_prune("dsh-auth-something-else", Some(host), host)

test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 225 filtered out; finished in 0.00s
MUTANT_TEST_EXIT=101
```

**解读（关键）**：

- `should_prune_hits_every_authority_a_port_ever_minted` 得到 **`left: 1, right: 3`**。
  `1` 正是"旧精确匹配只能命中当前那一个"的实测值，`3` 是修复要求的"历次 authority 全部命中"。**这正是 issue #74 描述的 bug 本身被测试捕获。**
- 该变异是**外科式**的：只失败 2 个 `should_prune` 相关测试，`dsh_auth_cookie_name_follows_the_origin_authority` 与 `loopback_origin_accepts_only_plain_loopback_http` 仍 `ok` ⇒ 失败确由谓词语义变化引起，而非测试互相污染或环境噪声。

✅ **新测试具备真实鉴别力**：修复前必失败、修复后必通过。

### 3.4 逐字节还原

```
Copy-Item .dsh-verify\windows.rs.bak src-tauri/src/windows.rs -Force

SHA_AFTER_RESTORE      = 966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196
GITBLOB_AFTER_RESTORE  = 233f3aaaa583b36548654580881cd0fbedde6938
EXPECTED_SHA_BEFORE    = 966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196
EXPECTED_BLOB_BEFORE   = 233f3aaaa583b36548654580881cd0fbedde6938
BYTES_EQUAL_EXTERNAL   = True
```

**结论：还原后与变异前 SHA256 及 git blob id 完全一致，且与仓库外备份逐字节相同。零残留。**

还原后 `git diff --stat`（工作树 vs HEAD）未变化，仍为：
```
 src-tauri/src/windows.rs | 156 +++++++++++++++++++++++++++++++++++------------
 1 file changed, 117 insertions(+), 39 deletions(-)
```

---

## 4. 对抗性边界评审（`should_prune`）

```rust
fn should_prune(name: &str, domain: Option<&str>, host: &str) -> bool {
    name.starts_with("dsh-auth-") && domain == Some(host)
}
```

### 4.1 前缀匹配会过度删除吗？

**不会突破实例边界。** 该谓词运行在 `win.cookies()` 返回的**本窗口** cookie 集合上，而每个实例窗口通过 `apply_window_store` → `data_directory(<data_dir>/webview/<instance-id>)` 拥有**私有 WebView2 store**（`windows.rs:111-119`、`330`）。因此可被删除的候选集**最多**是"本实例历次端口留下的 `dsh-auth-*`" ∪ "同 host 下其它 `dsh-auth-*`"。这正是本次要清理的对象。

### 4.2 domain 守卫是否仍然约束爆炸半径？

**是，且是独立第二道约束。** `domain == Some(host)` 中的 `host` 来自 `loopback_origin(&url)`，而 `loopback_origin` 只接受 `http://127.0.0.1[:port]`（WSL 转发、`localhost`、`https` 一律 `None`，见其单测）。三点推论：

1. 非回环实例（WSL / 远程）根本不会走到 `prune_auth_cookies`（`origin` 为 `None`，`windows.rs:282-284`）；
2. 即使命中，`domain` 必须恰为 `127.0.0.1`，其它域的 cookie 不在此列；
3. `localhost` 与 `127.0.0.1` 是不同 `Domain`，互不误伤。

### 4.3 裸 `"dsh-auth-"`（空前缀后缀）

当前被**有意**判定为 `true`，并由 `should_prune_leaves_foreign_cookies_alone:519` 固化：

```rust
assert!(should_prune("dsh-auth-", Some(host), host));
```

评审意见：**可接受，但属设计取舍而非严格必需。** 理由：DSH 现在总是 `dsh-auth-<digest>`，裸前缀不会被铸造；同时 ```starts_with``` 的语义天然包含空前缀，若为它单开例外反而引入分支。注释已明确写出"matched on purpose … so widening the prefix rule stays a deliberate decision"，与计划书 §四 P2 边界用例第 3 条"明确期望并用测试固化"的要求一致。**结论：符合验收标准。**

### 4.4 是否存在合法的非 DSH cookie 会被误删？

**理论上存在一个窄窗口，但实际不成立**（记为 R-1，见第 8 节风险）：
若某**第三方页面**恰好设置名为 `dsh-auth-*`、`Domain=127.0.0.1` 的 cookie，它会被删除。评估：本实例窗口的私有 store 只访问该实例自己的 `127.0.0.1:<port>` DSH 页面，不存在第三方来源；且 `dsh-auth-` 前缀是 DSH 自有命名空间。**残余风险极低，不构成阻塞。**

### 4.5 行为不回归（计划书验收标准 6）

- `delete_instance` → `commands.rs:383` `clear_instance_webview_data(&state,&id)`：走目录删除，**不经过** `should_prune`，未受影响。
- "无可用 URL" → `windows.rs:299` `clear_webview_data(...)`：macOS 走 `clear_all_browsing_data`，其它平台删目录，同样不经谓词。
- macOS 分支（`windows.rs:152-158`）与本次改动无交集；但 CI 的 macos 矩阵仍需通过（本地无法验证，见 R-2）。

---

## 5. diff 范围核查（计划书验收标准 2）

```
--- git status --porcelain ---
 M src-tauri/src/windows.rs
?? .dsh-verify/

--- git diff --stat main..HEAD ---
（空）

--- git diff --stat （工作树 vs HEAD）---
 src-tauri/src/windows.rs | 156 +++++++++++++++++++++++++++++++++++------------
 1 file changed, 117 insertions(+), 39 deletions(-)

--- git rev-list --count HEAD..upstream/main ---
0
--- git log -1 ---
c207edf fix(launch): 修复 EarlyLoading 窗口崩溃、白屏、拖拽与兼容性报告展示
```

**G-1（发现问题，非代码缺陷）**：`git diff --stat main..HEAD` **为空**。原因是**修复尚未提交**——HEAD 仍等于 `main`（`c207edf`），改动全部停留在工作树。计划书 §五 验收标准 2 应以**工作树 diff**（或提交后的 `main..HEAD`）判定；无论如何，**范围正确**：唯一改动文件是 `src-tauri/src/windows.rs`，且 `main..HEAD` 计数为 0 说明 fork 已同步上游。

> `.dsh-verify/` 是**我自己**为变异检查创建的一次性备份目录（未跟踪）。它不在任何写范围内，我会在验证结束后删除，避免污染仓库。

---

## 6. 【重要】发现的构建缓存陷阱（G-2）

还原后我立即重跑测试，出现了**假失败**：

```
POSTRESTORE_WINDOWS_TEST_EXIT=101
test result: FAILED. 2 passed; 2 failed

thread '...should_prune_hits_every_authority_a_port_ever_minted' panicked at src\windows.rs:496:9:
  left: 1
 right: 3
POSTRESTORE_FULL_TEST_EXIT=101
test result: FAILED. 224 passed; 2 failed; 3 ignored
```

**但此时源码已经完全正确**（`GITBLOB=233f3aaaa583b36548654580881cd0fbedde6938`，与变异前一致）。原因是 `Finished in 0.42s` 表明 cargo **没有重新编译**：还原写入的 mtime 落在与变异体相同的文件系统时间戳粒度内，cargo 的 mtime 新鲜度判断认为产物仍是最新的，于是**复用了变异体的二进制**。

**排除方法**（记录以便复现）：显式触碰 mtime 强制重编译——

```powershell
(Get-Item src-tauri/src/windows.rs).LastWriteTime = Get-Date
cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked windows::
```

```
   Compiling dsh-launcher v0.2.7 (D:\DSH\DSH-Launcher\src-tauri)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 16.46s

running 4 tests
test windows::tests::dsh_auth_cookie_name_follows_the_origin_authority ... ok
test windows::tests::should_prune_leaves_foreign_cookies_alone ... ok
test windows::tests::loopback_origin_accepts_only_plain_loopback_http ... ok
test windows::tests::should_prune_hits_every_authority_a_port_ever_minted ... ok
FORCED_WINDOWS_TEST_EXIT=0
```

**教训 / 对后续验证者的告警**：在"改→测→还原→再测"的变异检查流程里，**还原后的那次测试可能是缓存的旧产物**。若不复核 `Compiling` 行或强制触碰 mtime，极易得出"还原失败/源码有问题"的错误结论。本报告第 2 节的三道门禁均为**强制重编译后**的结果，可信。

---

## 7. CI 矩阵一致性（计划书验收标准 3）

阅读 `.github/workflows/ci.yml`，`quality` job（L17-56，三平台 linux/windows/macos）的门禁步骤：

| CI step | CI 命令（原文） | 本地实测 | 一致性 |
|---|---|---|---|
| Format check | `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 同 | ✅ EXIT=0 |
| Clippy (deny warnings) | `cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings` | 同 | ✅ EXIT=0 |
| Rust tests | `cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked` | 同 | ✅ EXIT=0 |
| Frontend build | `pnpm build` | 见下 | ⏳ |
| Install locked dependencies | `pnpm install --frozen-lockfile` | 未跑（见说明） | — |

**逐字一致**：三条 cargo 命令与 CI 完全相同（含 `--manifest-path`、`--all`、`--all-targets`、`--locked`、`-D warnings`）。`NODE_VERSION="24"` 与本机 Node v24.15.0 一致。

`integration` job（L58-77）为 Linux-only 的辅助门禁，本地实测：

```
node ci/check-versions.mjs            -> "versions in sync: 0.2.7"  CV_EXIT=0
node --test ci/release-notes.test.mjs -> pass 6 / fail 0             RN_EXIT=0
node --test ci/bump-version.test.mjs  -> pass 5 / fail 0             BV_EXIT=0
```

说明：`pnpm install --frozen-lockfile` 未单独执行——本次改动**不触碰任何依赖清单**（`git status` 仅有 `windows.rs`，`package.json`/`pnpm-lock.yaml`/`Cargo.toml`/`Cargo.lock` 均未变），lockfile 无需重解析；且 `pnpm build` 已在本机既有依赖树上正常运行。

### 7.1 `pnpm build` —— ❌ 本机无法完成（环境阻塞，非代码缺陷）

```
$sw=[System.Diagnostics.Stopwatch]::StartNew()
pnpm build 2>&1 | ForEach-Object { $_ }
"PNPM_BUILD_EXIT=$LASTEXITCODE"

TypeError: Cannot set property message of  which has only a getter
    at RetryOperation._fn (file:///C:/Users/yuki/.node/node-v24.15.0-win-x64/node_modules/pnpm/dist/pnpm.mjs:102685:27)
PNPM_BUILD_EXIT=1
PNPM_ELAPSED_MS=252650
```

**诊断结论：失败发生在 pnpm 的依赖解析/重试路径，`vue-tsc && vite build` 根本没有开始执行。**

证据链：

1. 报错栈位于 `RetryOperation._fn` —— 这是 pnpm 的**网络重试**代码路径，不是构建工具的报错；
2. 耗时 252,650 ms（约 4 分钟）后失败，符合"重试到超时"特征，而非编译错误；
3. `pnpm build` 脚本为 `vue-tsc --noEmit && vite build`，输出中**没有任何 vue-tsc / vite 的日志**；
4. **网络不可达**：`Invoke-WebRequest https://registry.npmjs.org/` → `REGISTRY_ERR=The SSL connection could not be established`。本机到 npm registry 的连接不可用，pnpm 因此在重试逻辑里抛错（该 pnpm 版本重试错误处理本身还有 bug，产生了上面这个 `TypeError`）。

**这不是代码问题**：本次改动只动了 Rust 侧 `windows.rs`，未触碰任何前端文件（`git status` 仅 `windows.rs`），也不涉及 `package.json` / `pnpm-lock.yaml`。

**为何 CI 上不受影响**：`.github/workflows/ci.yml` 的 quality job 会先执行 `pnpm install --frozen-lockfile`（L47-48），在 GitHub runner 上有网络与缓存，`pnpm build` 随后正常执行。本机缺的是**网络**，不是依赖。

本地依赖树实际是完整的，前端工具链全部可解析：

```
vue-tsc   -> node_modules\.pnpm\vue-tsc@2.2.12_typescript@5.9.3\...\vue-tsc\package.json
typescript-> node_modules\.pnpm\typescript@5.9.3\...\typescript\package.json
vite      -> node_modules\.pnpm\vite@6.4.3_sass@1.103.1_yaml@2.9.0\...\vite\package.json
vue=True  vue_tsc=True  @vitejs/plugin-vue=True
```

因此我尝试**绕过 pnpm 包装层**，直接运行 `pnpm build` 所定义的**两个真实步骤**（`vue-tsc --noEmit && vite build`）。

### 7.2 绕过 pnpm 直接执行构建步骤 —— ✅ 两步全部通过

**步骤 1：`vue-tsc --noEmit`（类型检查）**

```
& node node_modules/vue-tsc/bin/vue-tsc.js --noEmit
VUETSC_EXIT=0
```

无任何类型错误输出。

**步骤 2：`vite build`（打包）**

```
& node node_modules/vite/bin/vite.js build
...
dist/assets/index-rPHvejRP.js                         1,122.74 kB │ gzip: 333.19 kB

(!) Some chunks are larger than 500 kB after minification. ...
✓ built in 1m 19s
VITE_BUILD_EXIT=0
```

（chunk 体积警告是**既有**的 Vite 提示，非错误，与本次改动无关。）

### 7.3 前端门结论

| 步骤 | 结果 |
|---|---|
| `vue-tsc --noEmit` | ✅ EXIT=0 |
| `vite build` | ✅ EXIT=0（✓ built in 1m 19s） |
| `pnpm build`（包装层） | ❌ EXIT=1 —— **pnpm 自身在离线重试路径抛错，非构建失败** |

**判定：前端代码本身完全健康，`pnpm build` 的失败纯属本机环境（npm registry SSL 不可达）造成的 pnpm 包装层故障。** 在 CI 上因有网络与 `pnpm install --frozen-lockfile` 前置步骤，不预期复现。

> ⚠️ 诚实标注：严格来说我**没有**在本地跑通字面意义的 `pnpm build`。我用两个等价真实步骤证明了前端构建本身通过，但 CI 上 `pnpm build` 的确切行为仍以 CI 结果为准。这一项**不能算作本地已验证通过**。

---

## 8. 残余风险与不确定项

| ID | 项 | 等级 | 说明 |
|---|---|---|---|
| R-1 | 前缀可误删同名 `dsh-auth-*` 的第三方 cookie | 低 | 见 4.4。需第三方在本实例私有 store 的 `127.0.0.1` 域下设置同名 cookie，实际路径不存在。 |
| R-2 | 跨平台行为未本地验证 | 低 | 本地仅 Windows；linux/macos 依赖 CI 矩阵。改动为平台无关的纯谓词，风险低。 |
| R-3 | **端到端人工验证未做** | **中** | 计划书验收标准 5（反复 `--port 0` 重启后窗口不再 431、cookie 收敛为 1 条）**需要真实运行 launcher + DSH 实例并观察浏览器 cookie 库**，本次验证**未执行**。本报告只证明单元测试与静态门禁，不证明线上收敛。**建议在 PR 描述中如实标注。** |
| R-4 | 构建缓存可致假失败/假通过 | 中 | 见第 6 节。已在门禁复跑时用强制重编译规避。 |

---

## 9. 附：变更文件清单与哈希

| 文件 | 状态 | SHA256 | git blob |
|---|---|---|---|
| `src-tauri/src/windows.rs` | 已修改（未提交） | `966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196` | `233f3aaaa583b36548654580881cd0fbedde6938` |
| `docs/ISSUE74_VERIFICATION.md` | 本文件（新增） | — | — |

被验证的关键代码位置：

- `should_prune` 定义：`windows.rs:176-178`
- 生产调用点：`windows.rs:209`（`prune_auth_cookies` 内）
- `prune_auth_cookies` 定义：`windows.rs:185-217`
- 调用点：`windows.rs:283`（`open_instance_window`）
- `webview_data_dir`：`windows.rs:111-119`
- 更新后的注释：`windows.rs:97-110`、`windows.rs:327-329`
- 测试：`windows.rs:419-521`

---

## 10. 验证者签署

- 三道 Rust 门禁、CI integration 脚本、变异检查、逐字节还原、边界评审：**均已由本人独立实测完成**。
- 唯一未覆盖项：**R-3 端到端人工验证**（计划书验收标准 5）与 R-2 跨平台（依赖 CI）。二者均如实记录，未以单元测试冒充。
- 未发现任何源码缺陷；`should_prune` 的实现与计划书 §四 P1.5 的设计**逐字一致**。

### 10.1 验证结束时的仓库状态（自证未污染）

```
BRANCH             = fix/74-prune-auth-cookies
HEAD               = c207edf
WINDOWS_RS_SHA256  = 966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196
WINDOWS_RS_BLOB    = 233f3aaaa583b36548654580881cd0fbedde6938
git status --porcelain:
 M src-tauri/src/windows.rs
?? docs/ISSUE74_VERIFICATION.md
```

- `src-tauri/src/windows.rs` 的 SHA256 与 git blob **均等于变异前基线** ⇒ 变异检查零残留。
- 验证期间我自己创建的临时备份目录 `.dsh-verify/` **已删除**，工作树只剩"实现的改动 + 本报告"，无第三者痕迹。
- 仓库外多留一份备份 `%TEMP%\issue74_windows.rs.233f3aa.bak`（未提交工作成果的保险，**不属于仓库内容**，可随时删除）。
- `docs/ISSUE74_VERIFICATION.md` 为新增未跟踪文件，属我 task-2 的既定写范围。
