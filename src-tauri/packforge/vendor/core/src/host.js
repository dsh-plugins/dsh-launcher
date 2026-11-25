/**
 * Host —— core 的唯一 I/O 依赖（依赖注入边界）。
 *
 * core 自身不 import node:fs / node:crypto / child_process 等 Node 内建，
 * 全部通过 Host 注入，使同一份逻辑可同时跑在：
 *   - Node 宿主（CLI / Electron / server）     → @dsh-packforge/host-node
 *   - DSH 客户端插件（浏览器 + DSH 文件系统桥）→ @dsh-packforge/host-dsh-plugin
 *
 * 约定：
 *   - 所有方法返回 Promise（joinPath/resolvePath/cwd 是纯函数，同步返回）；
 *   - 读方法在「不存在 / 无权」时返回 null（不抛），写方法抛错；
 *   - 绝对路径：整体交给方法的参数，连接/解析用 joinPath/resolvePath，**绝不在 core 里手拼系统路径分隔符**；
 *   - 相对路径（rel）一律用 '/' 分隔（对齐格式规范 pack-structure）。
 */
export class Host {
  /** 连接路径片段为绝对路径（Node: path.join）。 */
  joinPath(..._parts) {
    throw new Error('Host.joinPath 未实现');
  }

  /** 解析路径为绝对路径（Node: path.resolve；相对片段基于 cwd）。 */
  resolvePath(..._parts) {
    throw new Error('Host.resolvePath 未实现');
  }

  /** 进程当前工作目录。 */
  cwd() {
    throw new Error('Host.cwd 未实现');
  }

  /** 用户主目录（如 '~'）。 */
  homedir() {
    throw new Error('Host.homedir 未实现');
  }

  /** 读环境变量；未设置 → null。 */
  env(_name) {
    throw new Error('Host.env 未实现');
  }

  /** 路径最后一段（Node: path.basename）。 */
  basename(_abs) {
    throw new Error('Host.basename 未实现');
  }

  /** 读文本（UTF-8）；不存在/无权 → null。 */
  async readTextFile(_abs) {
    throw new Error('Host.readTextFile 未实现');
  }

  /** 写文本（UTF-8，无 BOM）；自动建父目录。 */
  async writeTextFile(_abs, _text) {
    throw new Error('Host.writeTextFile 未实现');
  }

  /** 读二进制；不存在/无权 → null。 */
  async readFile(_abs) {
    throw new Error('Host.readFile 未实现');
  }

  /** 写二进制；自动建父目录。 */
  async writeFile(_abs, _data) {
    throw new Error('Host.writeFile 未实现');
  }

  /** 单路径元信息；不存在/无权 → null。返回 { size, isFile, isDirectory, isSymbolicLink }。 */
  async stat(_abs) {
    throw new Error('Host.stat 未实现');
  }

  /** 读取普通文件的 POSIX 访问位，宿主不支持时返回 null。 */
  async packFileMode(_abs) {
    throw new Error('Host.packFileMode 未实现');
  }

  /** 使用 Cordis 组合从低到高排列的补丁副本，返回配置行数组。 */
  async composePackEntries(_patches) {
    throw new Error('Host.composePackEntries 未实现');
  }

  /** 为普通文件设置 POSIX 访问位，宿主不支持时保留默认权限。 */
  async chmodPackFile(_abs, _mode) {
    throw new Error('Host.chmodPackFile 未实现');
  }

  /** 读目录条目；不存在/无权 → null。返回 [{ name, abs, type }]，type ∈ file|dir|symlink|other。 */
  async readdir(_abs) {
    throw new Error('Host.readdir 未实现');
  }

  /** 递归创建目录。 */
  async mkdir(_abs) {
    throw new Error('Host.mkdir 未实现');
  }

  /** 删除（opts: { recursive?, force? }）。 */
  async rm(_abs, _opts) {
    throw new Error('Host.rm 未实现');
  }

  /** 建临时目录，返回其绝对路径。 */
  async mkdtemp(_prefix) {
    throw new Error('Host.mkdtemp 未实现');
  }

  /** 对一段字节求 sha256，返回 64 位十六进制。 */
  async sha256(_data) {
    throw new Error('Host.sha256 未实现');
  }

  /** 对文件求 sha256（建议流式，避免大文件整读进内存）。 */
  async sha256File(_abs) {
    throw new Error('Host.sha256File 未实现');
  }

  /** 执行外部命令，返回 { status, error? }（Node: spawnSync 继承 stdio；插件/浏览器宿主另议）。 */
  async exec(_cmd, _args, _opts) {
    throw new Error('Host.exec 未实现');
  }

  /** 下载 http(s) URL 到本地文件。 */
  async download(_url, _destAbs) {
    throw new Error('Host.download 未实现');
  }

  /** 移动/重命名文件（跨目录），自动建目标父目录。 */
  async move(_from, _to) {
    throw new Error('Host.move 未实现');
  }
}
