# 启动器 Vue 控件

启动器使用 `@dsh-plugin/dsh-ui-components-vue` 的 `0.1.0-dev.44009856440` 版本，版本与完整性信息固定在 pnpm 锁文件中。

- `src/UiRoot.vue` 提供统一的主题、语言和浮层容器，同时覆盖主窗口与独立的终端、更新、启动进度窗口。
- `src/ui.ts` 按需注册启动器使用的 44 种控件，模板统一使用 `dsh-` 前缀。
- 按钮使用 `variant` 和 `sm/md/lg` 尺寸；原来的次要、文字及危险操作分别映射为对应语义变体。
- 页面消息使用 `useFeedback()`；命令式确认弹窗使用 `useDialog()`，继承当前主题与语言。
- Arco 直接依赖仅用于简体中文、英文语言资源，组件和图标均从 DSH Vue 包导入。
- 样式从新组件包的 `style.css` 加载。已有页面布局和终端内容保留，主启动按钮单独保持双行高度。

执行 `pnpm install --frozen-lockfile`、`pnpm build`、`pnpm test:market`、`pnpm test:network` 和 `pnpm test:compatibility-ui` 验证依赖、类型、构建及现有状态与解析规则。浏览器预览使用 `pnpm dev`，检查深浅主题、语言切换、下拉、提示、确认弹窗、表格与内部滚动。

组件库当前采用开发版本；升级时需重新核对其按钮、输入组合和浮层接口。原生 Tauri 文件选择与终端连接需在桌面构建中验收。
