# #107：实例内插件操作的 pnpm 环境

## 调查结果

原报告没有提供日志或可用备份，无法确认当时每次失败的具体原因。本次在隔离环境中复现了与恢复、安装流程直接相关的包管理环境差异：

- 启动器的插件安装通过 `dsh plugin` 传入 `--store-dir <data>/.pnpm-store`，必要时使用 `<data>/tools` 中的 pnpm 11。
- Web/TUI 实例原先只继承系统 PATH，未携带该 store 配置；实例中的 dshmarket 可以选到不同的 pnpm 或 store。
- 对已使用启动器 store 的 profile，换用另一个 store 执行 `add` 会出现 `ERR_PNPM_UNEXPECTED_STORE`。恢复时执行的 `install` 也可能要求重建现有 node_modules。
- pnpm 11 接受 `PNPM_CONFIG_STORE_DIR`；旧的 `npm_config_store_dir` 拼写无法传递该设置，已通过真实 pnpm 验证。

上游依据：

- [dshmarket 的 restoreBackup](https://github.com/dsh-market/dsh-market/blob/81d03b611d1a61cae5865c442e3e7d9cf7f6ae05/src/routes.ts)：恢复 manifest 后执行 `runPlugin(profile, ['install'])`；失败后会进入逐依赖恢复分支。
- [dshmarket 的子进程环境及 CLI 调用](https://github.com/dsh-market/dsh-market/blob/81d03b611d1a61cae5865c442e3e7d9cf7f6ae05/src/dsh-cli.ts)：内部操作继承宿主环境并重新调用 CLI。
- [pnpm 11 配置说明](https://pnpm.io/11.x/configuring)：环境配置使用 `pnpm_config_` / `PNPM_CONFIG_` 前缀。

## 修复

本地 Web、TUI 和嵌入式终端统一携带启动器 store 的默认配置；实例显式设置的同名配置仍然有效。存在合适的托管 pnpm 11 时，将其目录加入实例 PATH；终端添加 dsh shim 时保留该工具路径和实例原有 PATH。WSL 继续使用发行版内的环境。

## 验证

- 单元测试覆盖托管 pnpm 与终端 shim 的 PATH 顺序、实例显式 store 设置和旧版托管 pnpm 的处理。
- 真实 pnpm 11 离线回归使用三个本地 tgz 插件：先通过另一个 store 复现安装失败，再使用实例环境完成新增插件及恢复 manifest 后的安装，检查三个插件文件和 `.modules.yaml` 中的 store。

运行真实 pnpm 回归（需指定 pnpm 11 的 JS 入口）：

```powershell
$env:DSH_TEST_PNPM_CLI = 'C:\path\to\pnpm\bin\pnpm.mjs'
cargo test --manifest-path src-tauri/Cargo.toml --locked instance_pnpm_add_and_backup_restore_keep_the_launcher_store -- --ignored --nocapture
```

该回归验证的是恢复下载后的包安装步骤。Gist 鉴权下载、用户原始备份以及 dshmarket 的自行重启行为未在本次隔离测试中覆盖。
