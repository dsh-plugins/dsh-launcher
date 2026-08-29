// Shared types mirrored from the Rust backend (src-tauri/src/config.rs).

export interface DshHome {
  id: string
  name: string
  path: string
}

export interface DshVersion {
  id: string
  version: string
  dir: string
}

export interface DshInstance {
  id: string
  name: string
  version_id: string
  home_id: string
  env_overrides: Record<string, string>
  default_profile: string | null
  last_profile: string | null
  /** http(s) URL, "local" (cropped PNG in the HOME), or null/undefined = launcher default. */
  icon?: string | null
}

export interface LauncherSettings {
  locale: string
  minimize_to_tray: boolean
  autostart: boolean
  last_instance_id: string | null
  news_source: string
  theme: ThemeMode
  log_level: LogLevel
  /** SKILL source repos: https://[user:password@]github.com/user/repo[.git][#/path/to/skill] */
  skill_repos: string[]
}

/** UI theme: explicit light/dark, or follow the OS color scheme. */
export type ThemeMode = 'light' | 'dark' | 'system'

/** Runtime log level written to <data_dir>/logs/latest.log. */
export type LogLevel = 'debug' | 'info' | 'warn' | 'error'

/** Severity of a dependency-tree preflight finding. */
export type FindingLevel = 'warn' | 'error'

export interface DoctorFinding {
  level: FindingLevel
  /** core-version-mismatch | core-missing | profile-core-copy | profile-core-mixed */
  code: string
  message: string
}

/** Dependency-tree preflight result for an instance + profile (advisory). */
export interface DoctorReport {
  instance_id: string
  profile: string
  findings: DoctorFinding[]
}

/** Result of checking GitHub for a newer launcher release. */
export interface LauncherUpdateInfo {
  current: string
  /** "dev" for -dev.N builds, otherwise "stable". */
  channel: 'dev' | 'stable'
  up_to_date: boolean
  latest: string | null
  url: string | null
  published_at: string | null
}

export type InstanceState = 'stopped' | 'starting' | 'running' | 'exited'

export interface InstanceStatus {
  id: string
  state: InstanceState
  url: string | null
  profile: string | null
  exit_code: number | null
}

export interface ToolStatus {
  installed: boolean
  version: string | null
  path: string | null
}

export interface RuntimeStatus {
  node: ToolStatus
  pnpm: ToolStatus
}

/** `queued`: waiting for another operation on the same profile to finish. */
export type TaskState = 'queued' | 'running' | 'done' | 'error' | 'cancelled'

export interface TaskInfo {
  id: string
  kind: string
  label: string
  version: string
  state: TaskState
  percent: number
  created_at: number
  message: string | null
  instance_id: string | null
  instance_name: string | null
  logs: string[]
}

export interface TaskProgress {
  id: string
  state: TaskState
  percent: number
  message: string | null
  instance_id: string | null
}

export interface TaskLog {
  id: string
  line: string
}

export interface RemoteVersion {
  version: string
  released_at: string | null
  /** 'github' = GitHub-only tag, installed by building from source. */
  source?: 'npm' | 'github' | null
}

/** Modpack export overrides; unset fields fall back to profile-derived defaults. */
export interface ExportModpackInput {
  home_id: string
  profile: string
  out_dir: string
  name?: string
  version?: string
  displayName?: string
  description?: string
  author?: string
}

/** Modpack manifest (v2/v3); displayName/description may be a string or a locale map. */
export interface ModpackManifest {
  manifestVersion: number
  name: string
  displayName?: string | Record<string, string> | null
  version: string
  description?: string | Record<string, string> | null
  author?: string | null
  icon?: string | null
  dshVersion?: string | null
  profileName?: string | null
  bundles: string[]
  dependencies: Record<string, string>
  patch?: string | null
}

export interface ImportModpackInput {
  source: string
  force?: boolean
  instance_name?: string
  profile_name?: string
  /** Import into this existing instance instead of creating a new one. */
  existing_instance_id?: string
}

/** Repo origin recorded for an installed skill. */
export interface SkillOrigin {
  repo: string
  commit: string
  tag?: string | null
}

export interface SkillInfo {
  name: string
  description: string
  /** "dir" bundle or flat "file". */
  kind: string
  origin?: SkillOrigin | null
}

/** A skill discovered in a source repository (install picker). */
export interface RepoSkillInfo {
  name: string
  description: string
  /** Top-level path inside the repo; null when the repo root is the skill. */
  subpath?: string | null
}

/** A repo-sourced skill whose remote HEAD moved past the recorded commit. */
export interface SkillUpdateInfo {
  name: string
  current: string
  latest: string
}

// ---------------------------------------------------------------------------
// MCP servers (`@deepseek-ai/dsh-mcp-client` rows in cordis.patch.yml)
// ---------------------------------------------------------------------------

/** Transport selector, serialised exactly as dsh-mcp-client's `transport`. */
export type McpTransport = 'stdio' | 'streamable-http'

/** One ordered key/value row (request headers / env), like the env editor. */
export interface McpKv {
  key: string
  value: string
}

/** One editable MCP server: the loader row id plus its mcp-client config. */
export interface McpServer {
  /** Loader entry id (`mcp-<serverName>`), assigned by the backend; '' = new. */
  id: string
  /** Tool namespace `mcp__<serverName>__<rawName>`; [A-Za-z0-9_-]{1,32}. */
  serverName: string
  transport: McpTransport
  /** Streamable HTTP endpoint. */
  url: string
  /** Streamable HTTP request headers. */
  headers: McpKv[]
  /** stdio executable. */
  command: string
  /** stdio arguments, passed without shell interpolation. */
  args: string[]
  /** stdio extra environment variables. */
  env: McpKv[]
  /** stdio working directory ('' = inherit). */
  cwd: string
  /** false writes `disabled: true` on the loader row. */
  enabled: boolean
  /** Config keys the form does not surface (timeouts, reconnect), preserved. */
  extra: Record<string, unknown>
}

export interface NewInstanceInput {
  name: string
  version_id: string
  home_id: string
  env_overrides: Record<string, string>
  default_profile: string | null
}

/** Input for duplicating an instance (new name + reuse/new DSH_HOME choice). */
export interface CopyInstanceInput {
  source_id: string
  name: string
  new_home: boolean
}

// ---------------------------------------------------------------------------
// Plugin marketplace
// ---------------------------------------------------------------------------

export interface MarketPluginDescription {
  language: string
  content: string
}

export interface MarketPluginUrls {
  homepage?: string
  repository?: string
  issues?: string
}

export interface MarketPluginRelationship {
  kind: string // "dependency" | "incompatibility"
  id: string
  versions: string
}

/** Which catalog a market entry came from (serialised kebab-case). */
export type PluginSource = 'dsh-plugins' | 'awesome-dsh-plugin'

export interface MarketPlugin {
  id: string
  name: string
  description?: string | MarketPluginDescription[]
  support_versions?: unknown
  urls?: MarketPluginUrls
  relationship?: MarketPluginRelationship[]
  /** Absent on old payloads: treated as the primary dsh-plugins catalog. */
  source?: PluginSource
  /** Community-catalog extras. */
  category?: string
  stars?: number
  downloads?: number
}

export type PluginChannel = 'stable' | 'beta' | 'alpha'

export interface PluginVersionInfo {
  version: string
  channel: PluginChannel
  label?: string
  is_default: boolean
}

/** A page of versions; `has_more` enables infinite scrolling (alpha channel). */
export interface PluginVersionPage {
  versions: PluginVersionInfo[]
  has_more: boolean
}

export interface InstalledPlugin {
  id: string
  version?: string
  enabled: boolean
  cordis_id?: string
}

export interface InstallPluginInput {
  pluginId: string
  version: string
  channel: PluginChannel
  instanceId: string
  profile: string
}

export interface SetPluginsEnabledInput {
  instanceId: string
  profile: string
  pluginIds: string[]
  enabled: boolean
}

export interface UninstallPluginInput {
  instanceId: string
  profile: string
  pluginId: string
}

// ---------------------------------------------------------------------------
// Embedded terminal
// ---------------------------------------------------------------------------

/** Input for starting / restarting an instance's embedded terminal session. */
export interface StartTerminalInput {
  instanceId: string
  cols: number
  rows: number
}

/** Input for writing / resizing / closing a session. */
export interface TerminalIpcInput {
  instanceId: string
  /** For write: base64 of raw bytes to feed the PTY. */
  data?: string
  cols?: number
  rows?: number
}

/** Session state pushed to the frontend. */
export interface TerminalStatus {
  instanceId: string
  running: boolean
  exitCode: number | null
}

/** Raw PTY output pushed as `terminal://data`. */
export interface TerminalData {
  instanceId: string
  data: string
}
