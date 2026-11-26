# PackForge 参考引擎

此目录使用 `dsh-packforge-app` 提交 `d54e4aa079bbcc018ab99312bbfc08af90ca7910` 的 Core 和 Node Host 源码。两份上游 `package.json` 保留原有 MIT 许可声明。适配启动器的进程入口位于 `../worker.mjs`。本地改动将安装基线绑定到来源与归档摘要，协调启动器配置写入与引擎文件事务，并在导出预扫描和读取实例图标前应用插件的本机文件排除规则。

[上游源码](https://github.com/dsh-plugins/dsh-packforge-app/tree/d54e4aa079bbcc018ab99312bbfc08af90ca7910/packages)
