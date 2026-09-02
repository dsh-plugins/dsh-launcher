import { defineStore } from 'pinia'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type {
  DshHome,
  DshInstance,
  DshVersion,
  InstanceStatus,
  LauncherSettings,
  MarketPlugin,
  PluginChannel,
  PluginVersionInfo,
  RemoteVersion,
  RuntimeStatus,
  TaskInfo,
} from '@/api/types'

/** Wizard state carried across the plugin install flow. */
export interface PluginWizardState {
  plugin: MarketPlugin
  channel: PluginChannel
  version: PluginVersionInfo | null
}

/** Context handed to the modpack export page. */
export interface ModpackExportState {
  /** Instance id (for navigating back to the editor). */
  instanceId: string
  homeId: string
  profile: string
  /** Instance display name used as the default displayName field. */
  displayName: string
}

interface LauncherState {
  homes: DshHome[]
  versions: DshVersion[]
  instances: DshInstance[]
  settings: LauncherSettings
  statusById: Record<string, InstanceStatus>
  tasks: Record<string, TaskInfo>
  remoteVersions: RemoteVersion[]
  remoteLoading: boolean
  runtime: RuntimeStatus | null
  marketPlugins: MarketPlugin[]
  marketLoading: boolean
  marketLoadedAt: number | null
  pluginWizard: PluginWizardState | null
  modpackExport: ModpackExportState | null
  loaded: boolean
}

export const useLauncherStore = defineStore('launcher', {
  state: (): LauncherState => ({
    homes: [],
    versions: [],
    instances: [],
    settings: {
      locale: 'zh-CN',
      minimize_to_tray: true,
      autostart: false,
      last_instance_id: null,
      news_source: 'https://gist.githubusercontent.com/Gu-ZT/f08daa33afb82f4b375e604039b92742/raw/DSH_NEWS.md',
      theme: 'system',
      log_level: 'info',
      skill_repos: [],
      proxy_enabled: false,
      proxy_url: 'http://127.0.0.1',
      proxy_port: 7890,
      no_proxy: '127.0.0.1,localhost,::1',
      proxy_apply_dsh: false,
    },
    statusById: {},
    tasks: {},
    remoteVersions: [],
    remoteLoading: false,
    runtime: null,
    marketPlugins: [],
    marketLoading: false,
    marketLoadedAt: null,
    pluginWizard: null,
    modpackExport: null,
    loaded: false,
  }),

  getters: {
    versionById: (s) => (id: string) => s.versions.find((v) => v.id === id),
    homeById: (s) => (id: string) => s.homes.find((h) => h.id === id),
    instanceById: (s) => (id: string) => s.instances.find((i) => i.id === id),
    statusOf: (s) => (id: string): InstanceStatus =>
      s.statusById[id] ?? { id, state: 'stopped', url: null, profile: null, exit_code: null },
    taskList: (s) => Object.values(s.tasks).sort((a, b) => b.created_at - a.created_at),
    // A queued task is pending work too, so both counts treat it as active.
    runningTaskCount: (s) =>
      Object.values(s.tasks).filter((t) => t.state === 'running' || t.state === 'queued').length,
    instanceNameBusy: (s) => (name: string) =>
      Object.values(s.tasks).some(
        (t) => (t.state === 'running' || t.state === 'queued') && t.instance_name === name,
      ),
  },

  actions: {
    async init() {
      // Attach the status listener BEFORE the initial snapshot fetch, and
      // buffer events until the snapshot is applied. Otherwise an exit event
      // fired between fetch and listener attach is lost, leaving the UI
      // showing "running" for a process the backend already forgot (stop then
      // reported "实例未在运行").
      const pending: InstanceStatus[] = []
      let live = false
      const applyStatus = (st: InstanceStatus) => {
        if (st.state === 'stopped' || st.state === 'exited') {
          delete this.statusById[st.id]
        } else {
          this.statusById[st.id] = st
        }
      }
      await api.onInstanceStatus((st) => {
        if (live) applyStatus(st)
        else pending.push(st)
      })

      const [homes, versions, instances, settings, statuses, tasks, runtime] = await Promise.all([
        api.listHomes(),
        api.listVersions(),
        api.listInstances(),
        api.getSettings(),
        api.listInstanceStatus(),
        api.listTasks(),
        api.getRuntimeStatus(),
      ])
      this.homes = homes
      this.versions = versions
      this.instances = instances
      this.settings = settings
      this.statusById = Object.fromEntries(statuses.map((st) => [st.id, st]))
      this.tasks = Object.fromEntries(tasks.map((t) => [t.id, t]))
      this.runtime = runtime
      this.loaded = true
      live = true
      pending.forEach(applyStatus)

      await api.onTaskProgress((p) => {
        const existing = this.tasks[p.id]
        if (existing) {
          existing.state = p.state
          existing.percent = p.percent
          existing.message = p.message
          existing.instance_id = p.instance_id
        } else {
          // Event arrived before the task list did: seed a minimal entry so
          // the task manager never misses a just-created task.
          this.tasks[p.id] = {
            id: p.id,
            kind: 'create-instance',
            label: '',
            version: '',
            state: p.state,
            percent: p.percent,
            created_at: Date.now(),
            message: p.message,
            instance_id: p.instance_id,
            instance_name: null,
            logs: [],
          }
        }
        // A create-instance task finished: refresh instance/version lists.
        if (p.state === 'done' && p.instance_id) {
          this.refreshInstances()
          this.refreshVersions()
          this.refreshHomes()
        }
      })

      await api.onTaskLog((l) => {
        const existing = this.tasks[l.id]
        if (!existing) return
        if (existing.logs.length >= 1000) existing.logs.shift()
        existing.logs.push(l.line)
      })
    },

    async refreshInstances() {
      this.instances = await api.listInstances()
    },
    async refreshVersions() {
      this.versions = await api.listVersions()
    },
    async refreshHomes() {
      this.homes = await api.listHomes()
    },
    async refreshSettings() {
      this.settings = await api.getSettings()
    },
    async refreshTasks() {
      const tasks = await api.listTasks()
      this.tasks = Object.fromEntries(tasks.map((t) => [t.id, t]))
    },

    async checkRuntime() {
      this.runtime = await api.getRuntimeStatus()
    },

    async refreshRemoteVersions() {
      this.remoteLoading = true
      try {
        this.remoteVersions = await api.fetchAvailableVersions()
      } catch (e) {
        Message.error(String(e))
      } finally {
        this.remoteLoading = false
      }
    },

    /** Load the plugin marketplace catalog (cached until force=true). */
    async refreshMarketPlugins(force = false) {
      if (this.marketLoading) return
      if (!force && this.marketPlugins.length > 0 && this.marketLoadedAt) return
      this.marketLoading = true
      try {
        this.marketPlugins = await api.fetchPluginMarket()
        this.marketLoadedAt = Date.now()
      } catch (e) {
        Message.error(String(e))
      } finally {
        this.marketLoading = false
      }
    },
  },
})
