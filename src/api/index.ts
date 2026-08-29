// API layer: talks to the Tauri backend via invoke when running inside the
// desktop shell, and falls back to a localStorage-backed mock in a plain
// browser so the UI can be previewed without the Rust side.

import type {
  CopyInstanceInput,
  DoctorReport,
  DshHome,
  DshInstance,
  DshVersion,
  ExportModpackInput,
  ImportModpackInput,
  InstallPluginInput,
  InstalledPlugin,
  InstanceStatus,
  LauncherSettings,
  LauncherUpdateInfo,
  MarketPlugin,
  McpServer,
  ModpackManifest,
  NewInstanceInput,
  RepoSkillInfo,
  SkillInfo,
  SkillUpdateInfo,
  PluginChannel,
  PluginVersionPage,
  RemoteVersion,
  RuntimeStatus,
  SetPluginsEnabledInput,
  StartTerminalInput,
  TaskInfo,
  TaskLog,
  TaskProgress,
  TerminalData,
  TerminalIpcInput,
  TerminalStatus,
  UninstallPluginInput,
} from './types'

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

// ---------------------------------------------------------------------------
// Mock backend (browser preview)
// ---------------------------------------------------------------------------

const MOCK_KEY = 'dsh-launcher.mock.v1'

interface MockDb {
  homes: DshHome[]
  versions: DshVersion[]
  instances: DshInstance[]
  settings: LauncherSettings
  running: Record<string, InstanceStatus>
  /** MCP servers per scope key `<homeId>::<profile|__global__>`. */
  mcp: Record<string, McpServer[]>
}

function seedDb(): MockDb {
  return {
    homes: [
      { id: 'h-default', name: '默认 DSH_HOME', path: 'C:\\Users\\Administrator\\.dsh' },
      { id: 'h-lab', name: '实验室环境', path: 'D:\\dsh-homes\\lab' },
    ],
    versions: [
      { id: 'v-rc6', version: '0.1.0-rc.6', dir: 'C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\versions\\0.1.0-rc.6' },
      { id: 'v-rc5', version: '0.1.0-rc.5', dir: 'C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\versions\\0.1.0-rc.5' },
    ],
    instances: [
      {
        id: 'i-main',
        name: '主实例',
        version_id: 'v-rc6',
        home_id: 'h-default',
        env_overrides: { DSH_TELEMETRY_DISABLED: '1' },
        default_profile: 'web',
        last_profile: 'web',
      },
      {
        id: 'i-exp',
        name: '实验实例',
        version_id: 'v-rc5',
        home_id: 'h-lab',
        env_overrides: {},
        default_profile: null,
        last_profile: null,
      },
    ],
    settings: {
      locale: 'zh-CN',
      minimize_to_tray: true,
      autostart: false,
      last_instance_id: 'i-main',
      news_source: 'https://gist.githubusercontent.com/Gu-ZT/f08daa33afb82f4b375e604039b92742/raw/DSH_NEWS.md',
      theme: 'system',
      log_level: 'info',
      skill_repos: ['https://github.com/Gu-ZT/skills'],
    },
    running: {},
    mcp: {},
  }
}

function loadDb(): MockDb {
  try {
    const raw = localStorage.getItem(MOCK_KEY)
    if (raw) {
      const db = JSON.parse(raw) as MockDb
      // Backfill fields added after the mock db was persisted.
      db.settings.log_level = db.settings.log_level ?? 'info'
      db.mcp = db.mcp ?? {}
      return db
    }
  } catch {
    // fall through to seed
  }
  const db = seedDb()
  localStorage.setItem(MOCK_KEY, JSON.stringify(db))
  return db
}

function saveDb(db: MockDb) {
  localStorage.setItem(MOCK_KEY, JSON.stringify(db))
}

function uuid(): string {
  return 'xxxxxxxx-xxxx-4xxx'.replace(/x/g, () => ((Math.random() * 16) | 0).toString(16))
}

/** Mock scope key for MCP servers: the HOME plus the profile (or global). */
function mcpScopeKey(args?: Record<string, unknown>): string {
  const profile = (args?.profile as string | null | undefined) ?? '__global__'
  return `${String(args?.homeId ?? '')}::${profile}`
}

// Simple event emitter used by the mock to mimic Tauri events.
type Listener<T> = (payload: T) => void
const statusListeners = new Set<Listener<InstanceStatus>>()
const taskProgressListeners = new Set<Listener<TaskProgress>>()
const taskLogListeners = new Set<Listener<TaskLog>>()
const terminalDataListeners = new Set<Listener<TerminalData>>()
const terminalStatusListeners = new Set<Listener<TerminalStatus>>()

function emitStatus(s: InstanceStatus) {
  statusListeners.forEach((fn) => fn(s))
}
function emitTaskProgress(p: TaskProgress) {
  taskProgressListeners.forEach((fn) => fn(p))
}
function emitTaskLog(l: TaskLog) {
  taskLogListeners.forEach((fn) => fn(l))
}

// Mock task storage (runtime only, like the real backend).
const mockTasks = new Map<string, TaskInfo>()
// Mock profiles created at runtime per home id.
const mockProfiles: Record<string, string[]> = {}

function mockNewId(prefix: string): string {
  return `${prefix}-${uuid()}`
}

// ---------------------------------------------------------------------------
// Tauri invoke wrapper
// ---------------------------------------------------------------------------

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    const { invoke } = await import('@tauri-apps/api/core')
    return invoke<T>(cmd, args)
  }
  return mockCall<T>(cmd, args)
}

async function mockCall<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const db = loadDb()
  function fail(msg: string): never {
    throw new Error(msg)
  }

  switch (cmd) {
    case 'list_homes':
      return db.homes as T
    case 'default_dedicated_home_path': {
      const name = String(args?.name ?? 'instance')
      const safe = name.replace(/[^\w一-龥.-]+/g, '_')
      return `C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\homes\\${safe}` as T
    }
    case 'create_home': {
      const name = String(args?.name ?? '').trim()
      const path = String(args?.path ?? '').trim()
      if (!name || !path) fail('名称与路径不能为空')
      const home: DshHome = { id: `h-${uuid()}`, name, path }
      db.homes.push(home)
      saveDb(db)
      return home as T
    }
    case 'remove_home': {
      const id = String(args?.id)
      if (db.instances.some((i) => i.home_id === id)) fail('该 DSH_HOME 仍被实例引用，无法删除')
      db.homes = db.homes.filter((h) => h.id !== id)
      saveDb(db)
      return undefined as T
    }
    case 'list_versions':
      return db.versions as T
    case 'fetch_available_versions':
      return [
        { version: '0.1.2-alpha.1', released_at: '2026-10-01T08:00:00Z', source: 'github' },
        { version: '0.1.0-rc.6', released_at: '2026-08-01T12:00:00Z' },
        { version: '0.1.0-rc.5', released_at: '2026-07-15T09:30:00Z' },
        { version: '0.1.0-rc.4', released_at: '2026-07-01T10:00:00Z' },
        { version: '0.1.0-rc.3', released_at: '2026-06-15T08:00:00Z' },
      ] as T
    case 'start_create_instance_task': {
      const name = String(args?.name ?? '').trim()
      const version = String(args?.version ?? '').trim()
      const dedicated = Boolean(args?.dedicated)
      const homeIdArg = args?.home_id as string | null
      if (!name) fail('实例名称不能为空')
      if (!version) fail('版本号不能为空')
      if (db.instances.some((i) => i.name === name)) fail('同名实例已存在')
      if ([...mockTasks.values()].some((t) => t.state === 'running' && t.instance_name === name)) {
        fail('同名实例的下载任务已在进行中')
      }

      // Dedicated HOME is only materialized when the task finishes, mirroring
      // the real backend's placeholder semantics.
      let dedicatedPath: string | null = null
      if (dedicated) {
        const safe = name.replace(/[^\w一-龥.-]+/g, '_')
        dedicatedPath = `C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\homes\\${safe}`
      }
      if (!dedicated && (!homeIdArg || !db.homes.some((h) => h.id === homeIdArg))) {
        fail('请选择 DSH_HOME')
      }

      const task: TaskInfo = {
        id: mockNewId('t'),
        kind: 'create-instance',
        label: `下载 DSH ${version} 并创建实例「${name}」`,
        version,
        state: 'running',
        percent: 0,
        created_at: Date.now(),
        message: null,
        instance_id: null,
        instance_name: name,
        logs: [],
      }
      mockTasks.set(task.id, task)
      emitTaskProgress({ id: task.id, state: 'running', percent: 0, message: null, instance_id: null })

      // Simulate npm --loglevel=http download + install + instance creation.
      const fakeLogs = [
        `npm http fetch GET 200 https://registry.npmjs.org/@deepseek-ai%2fdsh 120ms`,
        `npm http fetch GET 200 https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-${version}.tgz 480ms`,
        `npm info ok`,
        `added 213 packages in 2s`,
      ]
      let step = 0
      const timer = setInterval(() => {
        const t = mockTasks.get(task.id)
        if (!t || t.state !== 'running') {
          clearInterval(timer)
          return
        }
        if (step < fakeLogs.length) {
          const line = fakeLogs[step]
          t.logs.push(line)
          t.percent = Math.min(95, t.percent + 20)
          emitTaskLog({ id: task.id, line })
          emitTaskProgress({ id: task.id, state: 'running', percent: t.percent, message: null, instance_id: null })
          step += 1
          return
        }
        clearInterval(timer)
        const cur = loadDb()
        // Resolve the HOME now (dedicated HOME is created at completion time).
        let resolvedHomeId = homeIdArg
        if (dedicated && dedicatedPath) {
          const existing = cur.homes.find(
            (h) => h.path.replace(/\\/g, '/').toLowerCase() === dedicatedPath!.replace(/\\/g, '/').toLowerCase(),
          )
          if (existing) {
            resolvedHomeId = existing.id
          } else {
            const home: DshHome = { id: mockNewId('h'), name, path: dedicatedPath! }
            cur.homes.push(home)
            resolvedHomeId = home.id
          }
        }
        // Install version record if missing.
        let ver = cur.versions.find((v) => v.version === version)
        if (!ver) {
          ver = {
            id: mockNewId('v'),
            version,
            dir: `C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\versions\\${version}`,
          }
          cur.versions.push(ver)
        }
        const inst: DshInstance = {
          id: mockNewId('i'),
          name,
          version_id: ver.id,
          home_id: resolvedHomeId!,
          env_overrides: {},
          default_profile: null,
          last_profile: null,
        }
        cur.instances.push(inst)
        saveDb(cur)
        const doneTask = mockTasks.get(task.id)
        if (doneTask) {
          doneTask.state = 'done'
          doneTask.percent = 100
          doneTask.instance_id = inst.id
        }
        emitTaskProgress({ id: task.id, state: 'done', percent: 100, message: null, instance_id: inst.id })
      }, 600)

      return task.id as T
    }
    case 'get_runtime_status': {
      // Browser preview: assume Node + pnpm are available so the UI is usable.
      const mockRuntime: RuntimeStatus = {
        node: { installed: true, version: 'v22.14.0', path: null },
        pnpm: { installed: true, version: '9.15.4', path: null },
      }
      return mockRuntime as T
    }
    case 'list_tasks': {
      return [...mockTasks.values()].sort((a, b) => b.created_at - a.created_at) as T
    }
    case 'remove_task': {
      const id = String(args?.id)
      const t = mockTasks.get(id)
      if (!t) fail('任务不存在')
      if (t.state === 'running' || t.state === 'queued') fail('任务仍在运行或排队中，请先取消')
      mockTasks.delete(id)
      return undefined as T
    }
    case 'cancel_task': {
      const id = String(args?.id)
      const t = mockTasks.get(id)
      if (!t) fail('任务不存在')
      t.state = 'cancelled'
      t.message = '已取消'
      emitTaskProgress({ id, state: 'cancelled', percent: t.percent, message: '已取消', instance_id: null })
      return undefined as T
    }
    case 'remove_version': {
      const id = String(args?.id)
      if (db.instances.some((i) => i.version_id === id)) fail('该版本仍被实例引用，无法删除')
      db.versions = db.versions.filter((v) => v.id !== id)
      saveDb(db)
      return undefined as T
    }
    case 'list_instances':
      return db.instances as T
    case 'create_instance': {
      const input = args?.input as NewInstanceInput
      if (db.instances.some((i) => i.name === input.name)) fail('同名实例已存在')
      const inst: DshInstance = {
        id: `i-${uuid()}`,
        name: input.name,
        version_id: input.version_id,
        home_id: input.home_id,
        env_overrides: input.env_overrides ?? {},
        default_profile: input.default_profile ?? null,
        last_profile: null,
      }
      db.instances.push(inst)
      saveDb(db)
      return inst as T
    }
    case 'update_instance': {
      const input = args?.input as DshInstance
      if (db.instances.some((i) => i.name === input.name && i.id !== input.id)) fail('同名实例已存在')
      db.instances = db.instances.map((i) => (i.id === input.id ? input : i))
      saveDb(db)
      return input as T
    }
    case 'delete_instance': {
      const id = String(args?.id)
      delete db.running[id]
      db.instances = db.instances.filter((i) => i.id !== id)
      if (db.settings.last_instance_id === id) db.settings.last_instance_id = null
      saveDb(db)
      return undefined as T
    }
    case 'copy_instance': {
      const input = args?.input as CopyInstanceInput
      const source = db.instances.find((i) => i.id === input.source_id)
      if (!source) fail('源实例不存在')
      if (db.instances.some((i) => i.name === input.name)) fail('同名实例已存在')
      let homeId = source.home_id
      if (input.new_home) {
        const path = `C:\\Users\\Administrator\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\homes\\${input.name.replace(/[^\w一-龥.-]+/g, '_')}`
        const existing = db.homes.find(
          (h) => h.path.replace(/\\/g, '/').toLowerCase() === path.replace(/\\/g, '/').toLowerCase(),
        )
        if (existing) {
          homeId = existing.id
        } else {
          const home: DshHome = { id: `h-${uuid()}`, name: input.name, path }
          db.homes.push(home)
          homeId = home.id
        }
      }
      const inst: DshInstance = {
        id: `i-${uuid()}`,
        name: input.name,
        version_id: source.version_id,
        home_id: homeId,
        env_overrides: { ...source.env_overrides },
        default_profile: source.default_profile,
        last_profile: null,
      }
      db.instances.push(inst)
      saveDb(db)
      return inst as T
    }
    case 'list_profiles': {
      const homeId = String(args?.home_id)
      const home = db.homes.find((h) => h.id === homeId)
      if (!home) fail('DSH_HOME 不存在')
      // Mock: combine the default set with previously created profiles.
      const base: string[] = home.path.endsWith('.dsh') ? ['web', 'demo', 'pack'] : ['web']
      const extras = mockProfiles[homeId] ?? []
      return [...base, ...extras] as T
    }
    case 'create_profile': {
      const homeId = String(args?.home_id)
      const name = String(args?.name ?? '').trim()
      if (!name) fail('Profile 名称不能为空')
      if (name === '__temp__' || name === 'node_modules') fail(`「${name}」为保留名称，不能使用`)
      if (!/^[A-Za-z0-9._-]+$/.test(name)) fail('Profile 名称只能包含字母、数字、-、_、.')
      const base: string[] = []
      const home = db.homes.find((h) => h.id === homeId)
      if (home && home.path.endsWith('.dsh')) base.push(...['web', 'demo', 'pack'])
      mockProfiles[homeId] = mockProfiles[homeId] ?? []
      if (base.includes(name) || mockProfiles[homeId].includes(name)) fail(`Profile「${name}」已存在`)
      mockProfiles[homeId].push(name)
      return name as T
    }
    case 'copy_profile': {
      const homeId = String(args?.home_id)
      const source = String(args?.source ?? '')
      const name = String(args?.name ?? '').trim()
      if (!name) fail('Profile 名称不能为空')
      if (name === '__temp__' || name === 'node_modules') fail(`「${name}」为保留名称，不能使用`)
      if (!/^[A-Za-z0-9._-]+$/.test(name)) fail('Profile 名称只能包含字母、数字、-、_、.')
      const home = db.homes.find((h) => h.id === homeId)
      const base: string[] = []
      if (home && home.path.endsWith('.dsh')) base.push(...['web', 'demo', 'pack'])
      mockProfiles[homeId] = mockProfiles[homeId] ?? []
      const exists = (n: string) => base.includes(n) || mockProfiles[homeId].includes(n)
      if (!exists(source)) fail(`Profile「${source}」不存在`)
      if (exists(name)) fail(`Profile「${name}」已存在`)
      mockProfiles[homeId].push(name)
      return name as T
    }
    case 'rename_profile': {
      const homeId = String(args?.home_id)
      const oldName = String(args?.old_name ?? '')
      const newName = String(args?.new_name ?? '').trim()
      if (!newName) fail('Profile 名称不能为空')
      if (newName === '__temp__' || newName === 'node_modules') fail(`「${newName}」为保留名称，不能使用`)
      if (!/^[A-Za-z0-9._-]+$/.test(newName)) fail('Profile 名称只能包含字母、数字、-、_、.')
      const home = db.homes.find((h) => h.id === homeId)
      const base: string[] = []
      if (home && home.path.endsWith('.dsh')) base.push(...['web', 'demo', 'pack'])
      mockProfiles[homeId] = mockProfiles[homeId] ?? []
      const exists = (n: string) => base.includes(n) || mockProfiles[homeId].includes(n)
      if (!exists(oldName)) fail(`Profile「${oldName}」不存在`)
      if (exists(newName)) fail(`Profile「${newName}」已存在`)
      const idx = mockProfiles[homeId].indexOf(oldName)
      if (idx >= 0) mockProfiles[homeId][idx] = newName
      // Keep instance references in sync.
      for (const inst of db.instances) {
        if (inst.home_id === homeId) {
          if (inst.default_profile === oldName) inst.default_profile = newName
          if (inst.last_profile === oldName) inst.last_profile = newName
        }
      }
      saveDb(db)
      return newName as T
    }
    case 'delete_profile': {
      const homeId = String(args?.home_id)
      const name = String(args?.name ?? '')
      if (name === '__temp__' || name === 'node_modules') fail(`「${name}」为保留名称，不能删除`)
      const home = db.homes.find((h) => h.id === homeId)
      const base: string[] = []
      if (home && home.path.endsWith('.dsh')) base.push(...['web', 'demo', 'pack'])
      mockProfiles[homeId] = mockProfiles[homeId] ?? []
      const exists = (n: string) => base.includes(n) || mockProfiles[homeId].includes(n)
      if (!exists(name)) fail(`Profile「${name}」不存在`)
      mockProfiles[homeId] = mockProfiles[homeId].filter((n) => n !== name)
      for (const inst of db.instances) {
        if (inst.home_id === homeId) {
          if (inst.default_profile === name) inst.default_profile = null
          if (inst.last_profile === name) inst.last_profile = null
        }
      }
      saveDb(db)
      return undefined as T
    }
    case 'start_instance': {
      const id = String(args?.id)
      const profile = String(args?.profile)
      if (db.running[id]?.state === 'running' || db.running[id]?.state === 'starting') fail('实例已在运行')
      const inst = db.instances.find((i) => i.id === id)
      if (!inst) fail('实例不存在')
      inst.last_profile = profile
      const starting: InstanceStatus = { id, state: 'starting', url: null, profile, exit_code: null }
      db.running[id] = starting
      saveDb(db)
      emitStatus(starting)
      setTimeout(() => {
        const cur = loadDb()
        if (cur.running[id]?.state !== 'starting') return
        const running: InstanceStatus = {
          id,
          state: 'running',
          url: `http://127.0.0.1:${30000 + Math.floor(Math.random() * 20000)}`,
          profile,
          exit_code: null,
        }
        cur.running[id] = running
        saveDb(cur)
        emitStatus(running)
      }, 1500)
      return undefined as T
    }
    case 'stop_instance': {
      const id = String(args?.id)
      const stopped: InstanceStatus = { id, state: 'stopped', url: null, profile: null, exit_code: 0 }
      delete db.running[id]
      saveDb(db)
      emitStatus(stopped)
      return undefined as T
    }
    case 'open_instance_window': {
      // Browser preview has no Tauri webview windows; deliberately do NOT
      // window.open here so the browser never navigates to a profile page.
      return undefined as T
    }
    case 'open_external':
      // Browser preview: a real new tab is the expected behavior here.
      window.open(String(args?.url ?? ''), '_blank', 'noopener,noreferrer')
      return undefined as T
    case 'export_modpack': {
      const input = args?.input as { out_dir?: string; profile?: string } | undefined
      return `${input?.out_dir ?? '.'}/${input?.profile ?? 'profile'}-1.0.0.tgz` as T
    }
    case 'start_import_modpack_task':
      return 'task-mock-modpack' as T
    case 'pending_deep_link':
      return null as T
    case 'create_launch_shortcut':
      return undefined as T
    case 'set_instance_icon':
    case 'clear_instance_icon':
      return undefined as T
    case 'read_instance_icon':
      return null as T
    case 'list_instance_skills':
      return [
        {
          name: 'conventional-commits',
          description: 'Conventional Commits 提交规范',
          kind: 'dir',
          origin: {
            repo: 'https://github.com/Gu-ZT/skills#/conventional-commits',
            commit: '0123456789abcdef0123456789abcdef01234567',
            tag: null,
          },
        },
      ] as T
    case 'install_skill_repo':
      return ['conventional-commits'] as T
    case 'list_repo_skills':
      return [
        {
          name: 'conventional-commits',
          description: 'Conventional Commits 提交规范',
          subpath: 'conventional-commits',
        },
        {
          name: 'minecraft-modder-neoforge',
          description: '提供 Minecraft 模组开发辅助。默认使用 NeoForge 26.1.2 和 Minecraft 26.1.2。',
          subpath: 'minecraft-modder-neoforge',
        },
      ] as T
    case 'check_skill_updates':
      return [
        { name: 'conventional-commits', current: '0123456', latest: '89abcde' },
      ] as T
    case 'import_skill_zip':
      return ['my-skill'] as T
    case 'update_skill':
      return 'v1.0.0' as T
    case 'delete_skill':
      return undefined as T
    case 'import_skill_file':
    case 'create_skill':
      return 'my-skill' as T
    // ---- MCP servers (browser preview keeps them in the mock db) ----
    case 'list_mcp_servers':
      return (db.mcp[mcpScopeKey(args)] ?? []) as T
    case 'save_mcp_server': {
      const key = mcpScopeKey(args)
      const list = [...(db.mcp[key] ?? [])]
      const server = { ...(args?.server as McpServer) }
      const originalId = String(args?.originalId ?? '')
      const index = originalId ? list.findIndex((s) => s.id === originalId) : -1
      const taken = list.filter((_, i) => i !== index).map((s) => s.id)
      server.id = taken.includes(`mcp-${server.serverName}`)
        ? `mcp-${server.serverName}-2`
        : `mcp-${server.serverName}`
      if (index >= 0) list[index] = server
      else list.push(server)
      db.mcp[key] = list
      saveDb(db)
      return list as T
    }
    case 'delete_mcp_server': {
      const key = mcpScopeKey(args)
      db.mcp[key] = (db.mcp[key] ?? []).filter((s) => s.id !== String(args?.id))
      saveDb(db)
      return db.mcp[key] as T
    }
    case 'read_modpack_manifest':
      return {
        manifestVersion: 3,
        name: 'all-about-whales',
        displayName: { 'en-US': 'All About Whales', 'zh-CN': '大肥鱼套装' },
        version: '1.0.0',
        description: '',
        author: 'hxh230802',
        dshVersion: '0.1.1-rc.2',
        profileName: 'all-about-whales',
        bundles: ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app'],
        dependencies: { 'dsh-pet': '0.2.0' },
      } as T
    case 'list_instance_status':
      return Object.values(db.running) as T
    case 'check_instance_health':
      // Browser preview has no real dependency tree: report healthy.
      return {
        instance_id: String(args?.instance_id ?? ''),
        profile: String(args?.profile ?? ''),
        findings: [],
      } as T
    case 'get_settings':
      return db.settings as T
    case 'fetch_news': {
      // Browser preview: return sample markdown instead of fetching.
      return [
        '# DSH Launcher 新闻',
        '',
        '- 支持 **GFM** 表格、任务列表与`行内代码`',
        '- 支持 <b>内联 HTML</b>（已过滤 XSS）',
        '',
        '| 版本 | 状态 |',
        '| ---- | ---- |',
        '| rc.6 | ✅   |',
      ].join('\n') as T
    }
    case 'update_settings': {
      db.settings = { ...db.settings, ...(args?.settings as Partial<LauncherSettings>) }
      saveDb(db)
      return db.settings as T
    }
    case 'check_launcher_update':
      return {
        current: '0.2.0-dev.1',
        channel: 'dev',
        up_to_date: false,
        latest: '0.2.0-dev.2',
        url: 'https://github.com/dsh-plugins/dsh-launcher/releases',
        published_at: new Date().toISOString(),
      } as T
    // ---- Plugin marketplace mocks (browser preview) ----
    case 'fetch_plugin_market': {
      const q = ((args?.query as string) ?? '').trim().toLowerCase()
      const all: MarketPlugin[] = [
        {
          id: '@dsh-plugin/dsh-approve-for-me',
          name: 'DSH Approve For Me',
          source: 'dsh-plugins',
          description: [{ language: 'zh-CN', content: '审查并自动批准命令执行，新增「替我同意」沙箱权限选项' }],
          urls: {
            homepage: 'https://github.com/dsh-plugins/dsh-approve-for-me',
            repository: 'https://github.com/dsh-plugins/dsh-approve-for-me',
            issues: 'https://github.com/dsh-plugins/dsh-approve-for-me/issues',
          },
          relationship: [{ kind: 'dependency', id: '@dsh-plugin/dsh-loader', versions: '>=1.3.0' }],
        },
        {
          id: '@dsh-plugin/dsh-auxiliary',
          name: 'DSH Auxiliary',
          source: 'dsh-plugins',
          description: [{ language: 'zh-CN', content: '辅助工具集：图像描述、任务看板等' }],
          urls: {
            repository: 'https://github.com/dsh-plugins/dsh-auxiliary',
            issues: 'https://github.com/dsh-plugins/dsh-auxiliary/issues',
          },
          relationship: [{ kind: 'dependency', id: '@dsh-plugin/dsh-loader', versions: '>=1.3.0' }],
        },
        {
          id: '@dsh-plugin/dsh-loader',
          name: 'DSH Loader',
          source: 'dsh-plugins',
          description: [{ language: 'zh-CN', content: 'DSH 插件加载器，所有插件的基础' }],
          urls: { repository: 'https://github.com/dsh-plugins/dsh-loader' },
        },
        {
          id: '@furongjun1999/dsh-memory',
          name: 'dsh-memory',
          source: 'awesome-dsh-plugin',
          category: 'agi',
          stars: 35,
          downloads: 1856,
          description: [
            { language: 'en', content: 'White-box AGI architecture exploration.' },
            { language: 'zh', content: '白箱AGI架构探索：元认知、持续学习、世界模型。' },
          ],
          urls: { repository: 'https://github.com/FuRongJun-1999/dsh-memory' },
        },
        {
          id: 'github:0imzero/dsh-workspace-menu',
          name: 'dsh-workspace-menu',
          source: 'awesome-dsh-plugin',
          category: 'ui',
          description: [
            { language: 'en', content: 'Workspace/chat context menu for the DSH home page.' },
            { language: 'zh', content: 'DSH 主页工作区/会话增强菜单。' },
          ],
          urls: { repository: 'https://github.com/0imzero/dsh-workspace-menu' },
        },
      ]
      if (!q) return all as T
      return all.filter((p) => {
        const desc = Array.isArray(p.description)
          ? p.description.map((d) => d.content).join(' ')
          : (p.description ?? '')
        return (p.id + p.name + desc).toLowerCase().includes(q)
      }) as T
    }
    case 'fetch_plugin_versions': {
      const pluginId = args?.plugin_id as string
      const channel = args?.channel as PluginChannel
      const page = (args?.page as number) ?? 1
      if (channel === 'alpha') {
        // Simulate pagination: each page yields 2 fake commits; 3 pages total.
        const totalPages = 3
        const per = 2
        const start = (page - 1) * per
        const items = [
          { version: 'abc1234def5678', label: '2026-04-10 · fix: something' },
          { version: 'bbb2222ccc3333', label: '2026-04-09 · feat: another' },
          { version: 'ccc3333ddd4444', label: '2026-04-08 · chore: deps' },
          { version: 'ddd4444eee5555', label: '2026-04-07 · fix: typo' },
          { version: 'eee5555fff6666', label: '2026-04-06 · feat: api' },
          { version: 'fff6666aaa1111', label: '2026-04-05 · docs: readme' },
        ]
        const slice = items.slice(start, start + per)
        return {
          versions: slice.map((c, i) => ({
            version: c.version,
            channel: 'alpha',
            label: c.label,
            is_default: page === 1 && i === 0,
          })),
          has_more: page < totalPages,
        } as T
      }
      void pluginId
      if (channel === 'beta') {
        return {
          versions: [
            { version: '0.4.0-next.1', channel: 'beta', label: '2026-04-08', is_default: true },
            { version: '0.4.0-next.0', channel: 'beta', label: '2026-04-01', is_default: false },
          ],
          has_more: false,
        } as T
      }
      return {
        versions: [
          { version: '1.3.0', channel: 'stable', label: '2026-04-05', is_default: true },
          { version: '1.2.0', channel: 'stable', label: '2026-03-20', is_default: false },
        ],
        has_more: false,
      } as T
    }
    case 'list_installed_plugins': {
      return [
        { id: '@dsh-plugin/dsh-auxiliary', version: '^0.4.1', enabled: true, cordis_id: 'dsh-auxiliary' },
        { id: '@dsh-plugin/dsh-thought-buddy', version: '^0.3.1', enabled: false, cordis_id: 'dsh-thought-buddy' },
      ] as T
    }
    case 'set_plugins_enabled':
      return undefined as T
    case 'uninstall_plugin':
      return undefined as T
    case 'start_terminal_session': {
      const input = args?.input as StartTerminalInput
      return {
        instanceId: input.instanceId,
        running: true,
        exitCode: null,
      } as T
    }
    case 'write_terminal_input':
    case 'resize_terminal_session':
    case 'close_terminal_session':
      return undefined as T
    case 'start_install_plugin_task': {
      const input = args?.input as InstallPluginInput
      const id = mockNewId('t')
      const task: TaskInfo = {
        id,
        kind: 'install-plugin',
        label: `安装插件 ${input.pluginId}@${input.version}`,
        version: input.version,
        state: 'done',
        percent: 100,
        created_at: Date.now(),
        message: null,
        instance_id: input.instanceId,
        instance_name: null,
        logs: [`mock install ${input.pluginId}@${input.version}`],
      }
      mockTasks.set(id, task)
      return id as T
    }
    default:
      fail(`mock: unknown command ${cmd}`)
  }
}
// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export const api = {
  isTauri,

  getRuntimeStatus: () => call<RuntimeStatus>('get_runtime_status'),

  listHomes: () => call<DshHome[]>('list_homes'),
  createHome: (name: string, path: string) => call<DshHome>('create_home', { name, path }),
  removeHome: (id: string) => call<void>('remove_home', { id }),
  defaultDedicatedHomePath: (name: string) => call<string>('default_dedicated_home_path', { name }),

  listVersions: () => call<DshVersion[]>('list_versions'),
  fetchAvailableVersions: () => call<RemoteVersion[]>('fetch_available_versions'),
  removeVersion: (id: string) => call<void>('remove_version', { id }),

  startCreateInstanceTask: (name: string, version: string, homeId: string | null, dedicated: boolean) =>
    call<string>('start_create_instance_task', { name, version, home_id: homeId, dedicated }),
  listTasks: () => call<TaskInfo[]>('list_tasks'),
  removeTask: (id: string) => call<void>('remove_task', { id }),
  cancelTask: (id: string) => call<void>('cancel_task', { id }),

  listInstances: () => call<DshInstance[]>('list_instances'),
  createInstance: (input: NewInstanceInput) => call<DshInstance>('create_instance', { input }),
  updateInstance: (input: DshInstance) => call<DshInstance>('update_instance', { input }),
  deleteInstance: (id: string) => call<void>('delete_instance', { id }),
  copyInstance: (input: CopyInstanceInput) => call<DshInstance>('copy_instance', { input }),

  listProfiles: (homeId: string) => call<string[]>('list_profiles', { home_id: homeId }),
  createProfile: (homeId: string, name: string) =>
    call<string>('create_profile', { home_id: homeId, name }),
  copyProfile: (homeId: string, source: string, name: string) =>
    call<string>('copy_profile', { home_id: homeId, source, name }),
  renameProfile: (homeId: string, oldName: string, newName: string) =>
    call<string>('rename_profile', { home_id: homeId, old_name: oldName, new_name: newName }),
  deleteProfile: (homeId: string, name: string) =>
    call<void>('delete_profile', { home_id: homeId, name }),
  /** Sets an instance icon from a local image path or http(s) URL. */
  setInstanceIcon: (instanceId: string, source: string) =>
    call<void>('set_instance_icon', { instanceId, source }),
  /** Restores the launcher default icon for an instance. */
  clearInstanceIcon: (instanceId: string) => call<void>('clear_instance_icon', { instanceId }),
  /** Resolves the displayable icon (URL or data URL); null = launcher default. */
  readInstanceIcon: (instanceId: string) => call<string | null>('read_instance_icon', { instanceId }),
  /** Lists skills in an instance HOME's skills directory. */
  listInstanceSkills: (homeId: string) => call<SkillInfo[]>('list_instance_skills', { homeId }),
  /** Installs skill(s) from a source repo URL; resolves to installed skill names. */
  installSkillRepo: (homeId: string, url: string) =>
    call<string[]>('install_skill_repo', { homeId, url }),
  /** Lists the skills a source repo offers (for the install picker). */
  listRepoSkills: (url: string) => call<RepoSkillInfo[]>('list_repo_skills', { url }),
  /** Compares recorded commits with remote HEADs; resolves to outdated skills. */
  checkSkillUpdates: (homeId: string) => call<SkillUpdateInfo[]>('check_skill_updates', { homeId }),
  /** Imports skills from a ZIP (root SKILL.md or top-level dirs with SKILL.md). */
  importSkillZip: (homeId: string, path: string) =>
    call<string[]>('import_skill_zip', { homeId, path }),
  /** Reinstalls a repo-sourced skill from its origin; resolves to the new version. */
  updateSkill: (homeId: string, name: string) => call<string>('update_skill', { homeId, name }),
  deleteSkill: (homeId: string, name: string) => call<void>('delete_skill', { homeId, name }),
  /** Imports a local SKILL.md into the HOME. */
  importSkillFile: (homeId: string, path: string) =>
    call<string>('import_skill_file', { homeId, path }),
  /** Creates a skill from pasted content (frontmatter auto-added when missing). */
  createSkill: (homeId: string, name: string, description: string, content: string) =>
    call<string>('create_skill', { homeId, name, description, content }),
  /**
   * MCP servers of one scope: `profile: null` reads `<HOME>/cordis.patch.yml`,
   * a profile name reads `<HOME>/profiles/<profile>/cordis.patch.yml`.
   */
  listMcpServers: (homeId: string, profile: string | null) =>
    call<McpServer[]>('list_mcp_servers', { homeId, profile }),
  /**
   * Creates or updates one MCP server; `originalId` names the row being edited
   * (null when adding). Rejects before writing when validation fails, and
   * resolves to the scope's servers as re-read from disk.
   */
  saveMcpServer: (homeId: string, profile: string | null, server: McpServer, originalId: string | null) =>
    call<McpServer[]>('save_mcp_server', { homeId, profile, server, originalId }),
  /** Deletes one MCP server row; resolves to the scope's remaining servers. */
  deleteMcpServer: (homeId: string, profile: string | null, id: string) =>
    call<McpServer[]>('delete_mcp_server', { homeId, profile, id }),
  exportModpack: (input: ExportModpackInput) => call<string>('export_modpack', { input }),
  /** Pre-reads a modpack's manifest before installing (for the confirm dialog). */
  readModpackManifest: (source: string) => call<ModpackManifest>('read_modpack_manifest', { source }),
  /** Cold-start deep link from process argv (null when launched normally). */
  pendingDeepLink: () => call<string | null>('pending_deep_link'),
  /** Writes a Windows .url shortcut launching an instance via dsh-launcher://launch. */
  createLaunchShortcut: (instanceId: string, profile: string, destPath: string) =>
    call<void>('create_launch_shortcut', { instanceId, profile, destPath }),
  /** Imports a modpack (.tgz path or URL) as a background task creating a new instance. */
  startImportModpackTask: (input: ImportModpackInput) =>
    call<string>('start_import_modpack_task', { input }),

  startInstance: (id: string, profile: string) => call<void>('start_instance', { id, profile }),
  checkInstanceHealth: (instanceId: string, profile: string) =>
    call<DoctorReport>('check_instance_health', { instance_id: instanceId, profile }),
  stopInstance: (id: string) => call<void>('stop_instance', { id }),
  openInstanceWindow: (id: string) => call<void>('open_instance_window', { id }),
  /** Opens an external http(s) URL in the system browser. */
  openExternal: (url: string) => call<void>('open_external', { url }),
  listInstanceStatus: () => call<InstanceStatus[]>('list_instance_status'),

  getSettings: () => call<LauncherSettings>('get_settings'),
  updateSettings: (settings: Partial<LauncherSettings>) => call<LauncherSettings>('update_settings', { settings }),
  fetchNews: (source: string) => call<string>('fetch_news', { source }),

  checkLauncherUpdate: () => call<LauncherUpdateInfo>('check_launcher_update'),
  /** The running launcher's own version (stamped at build time by CI). */
  async getLauncherVersion(): Promise<string> {
    if (isTauri) {
      const { getVersion } = await import('@tauri-apps/api/app')
      return getVersion()
    }
    return '0.2.0-dev.1'
  },

  // Plugin marketplace
  fetchPluginMarket: (query?: string) => call<MarketPlugin[]>('fetch_plugin_market', { query: query ?? null }),
  fetchPluginVersions: (pluginId: string, channel: PluginChannel, page = 1) =>
    call<PluginVersionPage>('fetch_plugin_versions', { plugin_id: pluginId, channel, page }),
  listInstalledPlugins: (instanceId: string, profile: string) =>
    call<InstalledPlugin[]>('list_installed_plugins', { instance_id: instanceId, profile }),
  setPluginsEnabled: (input: SetPluginsEnabledInput) => call<void>('set_plugins_enabled', { input }),
  uninstallPlugin: (input: UninstallPluginInput) => call<void>('uninstall_plugin', { input }),
  startInstallPluginTask: (input: InstallPluginInput) => call<string>('start_install_plugin_task', { input }),

  // Embedded terminal (PTY session)
  startTerminalSession: (input: StartTerminalInput) =>
    call<TerminalStatus>('start_terminal_session', { input }),
  writeTerminalInput: (input: TerminalIpcInput) =>
    call<void>('write_terminal_input', { input }),
  resizeTerminalSession: (input: TerminalIpcInput) =>
    call<void>('resize_terminal_session', { input }),
  closeTerminalSession: (input: TerminalIpcInput) =>
    call<void>('close_terminal_session', { input }),
  async onTerminalData(cb: Listener<TerminalData>): Promise<() => void> {
    if (isTauri) {
      const { listen } = await import('@tauri-apps/api/event')
      const un = await listen<TerminalData>('terminal://data', (e) => cb(e.payload))
      return un
    }
    terminalDataListeners.add(cb)
    return () => terminalDataListeners.delete(cb)
  },
  async onTerminalStatus(cb: Listener<TerminalStatus>): Promise<() => void> {
    if (isTauri) {
      const { listen } = await import('@tauri-apps/api/event')
      const un = await listen<TerminalStatus>('terminal://status', (e) => cb(e.payload))
      return un
    }
    terminalStatusListeners.add(cb)
    return () => terminalStatusListeners.delete(cb)
  },

  async onInstanceStatus(cb: Listener<InstanceStatus>): Promise<() => void> {
    if (isTauri) {
      const { listen } = await import('@tauri-apps/api/event')
      const un = await listen<InstanceStatus>('instance://status', (e) => cb(e.payload))
      return un
    }
    statusListeners.add(cb)
    return () => statusListeners.delete(cb)
  },

  async onTaskProgress(cb: Listener<TaskProgress>): Promise<() => void> {
    if (isTauri) {
      const { listen } = await import('@tauri-apps/api/event')
      const un = await listen<TaskProgress>('task://progress', (e) => cb(e.payload))
      return un
    }
    taskProgressListeners.add(cb)
    return () => taskProgressListeners.delete(cb)
  },

  async onTaskLog(cb: Listener<TaskLog>): Promise<() => void> {
    if (isTauri) {
      const { listen } = await import('@tauri-apps/api/event')
      const un = await listen<TaskLog>('task://log', (e) => cb(e.payload))
      return un
    }
    taskLogListeners.add(cb)
    return () => taskLogListeners.delete(cb)
  },
}
