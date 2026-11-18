<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useFeedback, useDialog } from '@dsh-plugin/dsh-ui-components-vue'
import { IconArrowUp, IconArrowDown } from '@dsh-plugin/dsh-ui-components-vue/icons'
import { api } from '@/api'
import type {
  Confidence,
  LauncherUpdateInfo,
  LogLevel,
  PluginSourceConfig,
  SourceKind,
  ThemeMode,
} from '@/api/types'
import { SUPPORTED_LOCALES } from '@/i18n'
import { useLauncherStore } from '@/stores/launcher'
import ImportScanDialog from '@/components/ImportScanDialog.vue'
import HintIcon from '@/components/HintIcon.vue'
import PageScrollbar from '@/components/PageScrollbar.vue'

const { message: Message } = useFeedback()
const { modal: Modal } = useDialog()

const { t } = useI18n()
const store = useLauncherStore()

type SettingsSection = 'launch' | 'notifications' | 'personal' | 'network' | 'resources' | 'about'
const section = ref<SettingsSection>('launch')

/** Project links shown on the About section (issue #72). */
const REPO_URL = 'https://github.com/dsh-plugins/dsh-launcher'
const ISSUES_URL = `${REPO_URL}/issues`
const RELEASES_URL = `${REPO_URL}/releases`

/** Local-environment import wizard (issue #31). */
const importScanVisible = ref(false)

const THEME_OPTIONS = computed<{ value: ThemeMode; label: string }[]>(() => [
  { value: 'light', label: t('settings.theme.light') },
  { value: 'dark', label: t('settings.theme.dark') },
  { value: 'system', label: t('settings.theme.system') },
])

const LOG_LEVEL_OPTIONS = computed<{ value: LogLevel; label: string }[]>(() => [
  { value: 'debug', label: t('settings.logLevel.debug') },
  { value: 'info', label: t('settings.logLevel.info') },
  { value: 'warn', label: t('settings.logLevel.warn') },
  { value: 'error', label: t('settings.logLevel.error') },
])

// --- General settings -------------------------------------------------------

async function patchSettings(patch: Parameters<typeof api.updateSettings>[0]) {
  try {
    store.settings = await api.updateSettings(patch)
    Message.success(t('settings.saved'))
  } catch (e) {
    Message.error(String(e))
  }
}

async function onThemeChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ theme: String(value) as ThemeMode })
}

async function onLogLevelChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ log_level: String(value) as LogLevel })
}

// --- Launcher update check (GitHub releases) --------------------------------

const launcherVersion = ref('')
const checkingUpdate = ref(false)
const updateInfo = ref<LauncherUpdateInfo | null>(null)
/** Update channel: "dev" (includes prereleases) or "release" (stable only). */
const updateChannel = ref<'dev' | 'release'>('dev')

const UPDATE_CHANNEL_OPTIONS = computed<{ value: 'dev' | 'release'; label: string }[]>(() => [
  { value: 'dev', label: t('settings.update.channel.dev') },
  { value: 'release', label: t('settings.update.channel.release') },
])

onMounted(async () => {
  loadLauncherIcons()
  try {
    launcherVersion.value = await api.getLauncherVersion()
  } catch {
    launcherVersion.value = '?'
  }
  // The chosen update channel persists across restarts (issue: update notice).
  updateChannel.value = store.settings.update_channel ?? 'dev'
  try {
    dataDir.value = await api.getLauncherDirectory()
  } catch {
    dataDir.value = ''
  }
  refreshDataDirSource()
})

async function onCheckUpdate() {
  checkingUpdate.value = true
  try {
    updateInfo.value = await api.checkLauncherUpdate(updateChannel.value)
    if (updateInfo.value.up_to_date) Message.success(t('settings.update.upToDate'))
  } catch (e) {
    Message.error(String(e))
  } finally {
    checkingUpdate.value = false
  }
}

async function onUpdateChannelChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  const channel = String(value) === 'release' ? 'release' : 'dev'
  updateChannel.value = channel
  // A different channel invalidates the previous result; only a fresh check
  // is meaningful for the new channel.
  updateInfo.value = null
  // Remember the choice across restarts; the startup check uses it.
  await patchSettings({ update_channel: channel })
}

// --- Data directory (issue #43) ---------------------------------------------

const dataDir = ref('')
const dataDirSource = ref<'env' | 'pointer' | 'default'>('default')
const moveTarget = ref('')
const moveModalVisible = ref(false)
const moveBusy = ref(false)

async function refreshDataDirSource() {
  try {
    const info = await api.getDataDirSource()
    dataDir.value = info.path
    dataDirSource.value = info.source as 'env' | 'pointer' | 'default'
    if (info.notice) Message.warning(info.notice)
  } catch {
    /* source 鏈煡鏃朵繚鎸侀粯璁ゅ睍绀?*/
  }
}

async function onMoveDataDir() {
  try {
    const dir = await api.pickDataDir()
    if (!dir) return
    moveTarget.value = dir
    moveModalVisible.value = true
  } catch (e) {
    Message.error(String(e))
  }
}

async function onConfirmMove() {
  moveBusy.value = true
  try {
    const committed = await api.commitDataDir(moveTarget.value)
    moveModalVisible.value = false
    Message.success(t('settings.dataDir.migratedToast', [committed]))
    // 鎸囬拡宸插啓鍏?杩佺Щ鍦ㄤ笅娆″惎鍔ㄦ椂鍙戠敓
    refreshDataDirSource()
  } catch (e) {
    Message.error(String(e))
  } finally {
    moveBusy.value = false
  }
}

const dataDirSourceTip = computed(() => {
  if (dataDirSource.value === 'env') return t('settings.dataDir.sourceEnv')
  if (dataDirSource.value === 'pointer') return t('settings.dataDir.sourcePointer')
  return t('settings.dataDir.sourceDefault')
})

async function onOpenDataDir() {
  try {
    const dir = await api.openLauncherDirectory()
    dataDir.value = dir
  } catch (e) {
    Message.error(String(e))
  }
}

async function onOpenLauncherLog() {
  try {
    const path = await api.openLauncherLog()
    Message.success(t('settings.logOpened', { path }))
  } catch (e) {
    Message.error(String(e))
  }
}

async function onLocaleChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ locale: String(value) })
}

async function onTrayChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ minimize_to_tray: Boolean(value) })
}

async function onAutostartChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ autostart: Boolean(value) })
}

async function onAutoOpenChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ auto_open_on_launch: Boolean(value) })
}

async function onHideLauncherChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ hide_launcher_on_window_open: Boolean(value) })
}

async function onPerInstanceTrayChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ per_instance_tray: Boolean(value) })
}

// --- Silent autostart & auto-launch (issue #100) ------------------------------

/** Instances started in the background after an OS autostart launch. */
const autostartInstances = computed(() => store.settings.autostart_instances ?? [])
/** Instance choices for the picker: id -> display name (missing ones excluded). */
const autostartCandidates = computed(() =>
  store.instances.map((i) => ({ value: i.id, label: i.name })),
)
const newAutostartInstance = ref<string | undefined>(undefined)
const newAutostartProfile = ref('')

async function onSilentAutostartChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ silent_autostart: Boolean(value) })
}

async function onAutostartOpenWindowsChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ autostart_open_windows: Boolean(value) })
}

async function onAddAutostartInstance() {
  const id = newAutostartInstance.value
  if (!id) return
  if (autostartInstances.value.some((a) => a.instance_id === id)) {
    Message.warning(t('settings.silentAutostart.duplicated'))
    return
  }
  const profile = newAutostartProfile.value.trim()
  await patchSettings({
    autostart_instances: [
      ...autostartInstances.value,
      { instance_id: id, ...(profile ? { profile } : {}) },
    ],
  })
  newAutostartInstance.value = undefined
  newAutostartProfile.value = ''
}

async function onRemoveAutostartInstance(instanceId: string) {
  await patchSettings({
    autostart_instances: autostartInstances.value.filter((a) => a.instance_id !== instanceId),
  })
}

/** Display name of an autostart entry's instance; falls back to the id. */
function autostartInstanceName(instanceId: string): string {
  return store.instances.find((i) => i.id === instanceId)?.name ?? instanceId
}

// News source: saved on blur / Enter so typing is not interrupted.
const newsSource = ref(store.settings.news_source ?? '')
watch(
  () => store.settings.news_source,
  (v) => {
    if ((v ?? '') !== newsSource.value) newsSource.value = v ?? ''
  },
)

async function onNewsSourceSave() {
  const value = newsSource.value.trim()
  if (value === (store.settings.news_source ?? '')) return
  await patchSettings({ news_source: value })
}

// --- Launcher icons (issue #59): custom window (title bar / taskbar) and
// tray icons, stored under <data>/icons by the backend.
const launcherIcons = ref<{ window: string | null; tray: string | null }>({ window: null, tray: null })
const launcherIconBusy = ref('')

async function loadLauncherIcons() {
  try {
    const [window, tray] = await Promise.all([
      api.readLauncherIcon('window'),
      api.readLauncherIcon('tray'),
    ])
    launcherIcons.value = { window, tray }
  } catch (e) {
    Message.error(String(e))
  }
}

async function onPickLauncherIcon(kind: 'window' | 'tray') {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'Image', extensions: ['png', 'jpg', 'jpeg', 'ico'] }],
  })
  if (typeof file !== 'string') return
  launcherIconBusy.value = kind
  try {
    await api.setLauncherIcon(kind, file)
    launcherIcons.value[kind] = await api.readLauncherIcon(kind)
    Message.success(t('settings.launcherIcon.saved'))
  } catch (e) {
    Message.error(String(e))
  } finally {
    launcherIconBusy.value = ''
  }
}

async function onClearLauncherIcon(kind: 'window' | 'tray') {
  launcherIconBusy.value = kind
  try {
    await api.clearLauncherIcon(kind)
    launcherIcons.value[kind] = null
  } catch (e) {
    Message.error(String(e))
  } finally {
    launcherIconBusy.value = ''
  }
}

// --- SKILL source repos (issue #10) ---------------------------------------------

const newSkillRepo = ref('')
const skillRepoBusy = ref(false)

async function onAddSkillRepo() {
  const url = newSkillRepo.value.trim()
  if (!url) return
  if (store.settings.skill_repos.includes(url)) {
    Message.warning(t('settings.skillRepoExists'))
    return
  }
  skillRepoBusy.value = true
  try {
    await patchSettings({ skill_repos: [...store.settings.skill_repos, url] })
    newSkillRepo.value = ''
  } finally {
    skillRepoBusy.value = false
  }
}

async function onRemoveSkillRepo(url: string) {
  await patchSettings({ skill_repos: store.settings.skill_repos.filter((r) => r !== url) })
}

// --- Plugin sources -----------------------------------------------------------

const SOURCE_KIND_OPTIONS = computed<{ value: SourceKind; label: string }[]>(() => [
  { value: 'primary', label: t('settings.pluginSources.sourceKinds.primary') },
  { value: 'awesome', label: t('settings.pluginSources.sourceKinds.awesome') },
  { value: 'dsh-get', label: t('settings.pluginSources.sourceKinds.dshGet') },
  { value: 'github-topic', label: t('settings.pluginSources.sourceKinds.githubTopic') },
])

const CONFIDENCE_TAG_COLORS: Record<Confidence, string> = {
  official: 'green',
  curated: 'purple',
  aggregated: 'blue',
  unverified: 'orangered',
}

const newSourceId = ref('')
const newSourceKind = ref<SourceKind>('primary')
const newSourceUrl = ref('')
const pluginSourceBusy = ref(false)

// --- Plugin updates -----------------------------------------------------------

const pluginUpdateChannel = computed<'release' | 'beta'>(
  () => store.settings.plugin_update_channel ?? 'release',
)

const PLUGIN_UPDATE_CHANNEL_OPTIONS = computed<{ value: 'release' | 'beta'; label: string }[]>(() => [
  { value: 'release', label: t('settings.pluginUpdates.channel.release') },
  { value: 'beta', label: t('settings.pluginUpdates.channel.beta') },
])

async function onPluginUpdateChannelChange(value: unknown) {
  const channel = value === 'beta' ? 'beta' : 'release'
  await patchSettings({ plugin_update_channel: channel })
}

/** Order is the array index; renumber on every mutation. */
function renumberSources(list: PluginSourceConfig[]): PluginSourceConfig[] {
  return list.map((s, i) => ({ ...s, order: i }))
}

async function savePluginSources(list: PluginSourceConfig[]) {
  pluginSourceBusy.value = true
  try {
    await patchSettings({ plugin_sources: renumberSources(list) })
    // The market's source filter reads a cached copy; invalidate it so edits
    // here are reflected there without a reload.
    store.pluginSourcesLoadedAt = null
  } finally {
    pluginSourceBusy.value = false
  }
}

async function onAddPluginSource() {
  const id = newSourceId.value.trim()
  const url = newSourceUrl.value.trim()
  if (!id) {
    Message.warning(t('settings.pluginSources.invalidId'))
    return
  }
  // github-topic sources are discovered dynamically and have no static URL.
  if (newSourceKind.value !== 'github-topic' && !/^https?:\/\//i.test(url)) {
    Message.warning(t('settings.pluginSources.invalidUrl'))
    return
  }
  if (store.settings.plugin_sources.some((s) => s.id === id)) {
    Message.warning(t('settings.pluginSources.duplicateId'))
    return
  }
  await savePluginSources([
    ...store.settings.plugin_sources,
    {
      id,
      url,
      kind: newSourceKind.value,
      enabled: true,
      // Trust is launcher-assigned: custom sources are always unverified
      // (the backend locks this in sanitize_plugin_sources regardless).
      confidence: 'unverified' as Confidence,
      order: store.settings.plugin_sources.length,
    },
  ])
  // The save swallows backend errors, so only clear the form once the source
  // actually landed in the refreshed settings.
  if (!store.settings.plugin_sources.some((s) => s.id === id)) return
  newSourceId.value = ''
  newSourceUrl.value = ''
}

async function onPluginSourceEnabledChange(
  source: PluginSourceConfig,
  value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[],
) {
  if (pluginSourceBusy.value) return
  await savePluginSources(
    store.settings.plugin_sources.map((s) => (s.id === source.id ? { ...s, enabled: Boolean(value) } : s)),
  )
}

async function onRemovePluginSource(id: string) {
  if (pluginSourceBusy.value) return
  await savePluginSources(store.settings.plugin_sources.filter((s) => s.id !== id))
}

async function onMovePluginSource(index: number, delta: number) {
  if (pluginSourceBusy.value) return
  const list = [...store.settings.plugin_sources]
  const target = index + delta
  if (target < 0 || target >= list.length) return
  ;[list[index], list[target]] = [list[target], list[index]]
  await savePluginSources(list)
}

// --- Proxy settings -----------------------------------------------------------

async function onProxyEnabledChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ proxy_enabled: Boolean(value) })
}

async function onProxyApplyDshChange(value: string | number | boolean | Record<string, unknown> | (string | number | boolean | Record<string, unknown>)[]) {
  await patchSettings({ proxy_apply_dsh: Boolean(value) })
}

// Text fields save on blur / Enter so typing is not interrupted.
const proxyUrl = ref(store.settings.proxy_url ?? '')
const proxyPort = ref(store.settings.proxy_port ?? 7890)
const noProxy = ref(store.settings.no_proxy ?? '')
watch(
  () => [store.settings.proxy_url, store.settings.proxy_port, store.settings.no_proxy] as const,
  ([url, port, np]) => {
    const u = String(url ?? '')
    if (u !== proxyUrl.value) proxyUrl.value = u
    const p = Number(port ?? 7890)
    if (p !== proxyPort.value) proxyPort.value = p
    const n = String(np ?? '')
    if (n !== noProxy.value) noProxy.value = n
  },
)

async function onProxyFieldsSave() {
  const patch: Parameters<typeof api.updateSettings>[0] = {}
  const url = proxyUrl.value.trim()
  if (url && url !== store.settings.proxy_url) patch.proxy_url = url
  if (proxyPort.value && proxyPort.value !== store.settings.proxy_port) patch.proxy_port = proxyPort.value
  const np = noProxy.value.trim()
  if (np !== (store.settings.no_proxy ?? '')) patch.no_proxy = np
  if (Object.keys(patch).length > 0) await patchSettings(patch)
}

// --- DSH_HOME management ------------------------------------------------------

const newHomeName = ref('')
const newHomePath = ref('')

async function onPickDir() {
  if (api.isTauri) {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const dir = await open({ directory: true, multiple: false })
      if (typeof dir === 'string') newHomePath.value = dir
    } catch (e) {
      Message.error(String(e))
    }
  } else {
    Message.info(t('settings.browserPickHint'))
  }
}

async function onAddHome() {
  try {
    await api.createHome(newHomeName.value, newHomePath.value)
    newHomeName.value = ''
    newHomePath.value = ''
    await store.refreshHomes()
    Message.success(t('settings.saved'))
  } catch (e) {
    Message.error(String(e))
  }
}

async function onRemoveHome(id: string) {
  try {
    await api.removeHome(id)
    await store.refreshHomes()
    return true
  } catch (e) {
    Message.error(String(e))
    return false
  }
}

function confirmRemoveHome(id: string, name: string) {
  Modal.confirm({
    title: t('settings.deleteHome'),
    content: t('settings.confirmDeleteHome', { name }),
    okText: t('settings.deleteHome'),
    cancelText: t('common.cancel'),
    okButtonProps: { status: 'danger' },
    onBeforeOk: () => onRemoveHome(id),
  })
}

const homeColumns = computed(() => [
  { title: t('settings.homeName'), dataIndex: 'name', width: 180 },
  { title: t('settings.homePath'), dataIndex: 'path', ellipsis: true, tooltip: true },
  { title: t('instances.table.actions'), slotName: 'actions', width: 110, align: 'center' as const, fixed: 'right' as const },
])

// Plugin sources rendered as a table (like the DSH_HOME card): long URLs get
// ellipsis + tooltip, the action column is pinned right and never squeezed
// away, and headers stay strictly aligned with their columns (issue #13).
const pluginSourceColumns = computed(() => [
  { title: t('settings.pluginSources.enable'), slotName: 'psEnable', width: 90 },
  { title: t('settings.pluginSources.id'), dataIndex: 'id', width: 160, ellipsis: true, tooltip: true },
  { title: t('settings.pluginSources.kind'), slotName: 'psKind', width: 120 },
  { title: t('settings.pluginSources.confidence'), slotName: 'psConf', width: 120 },
  { title: t('settings.pluginSources.url'), dataIndex: 'url', ellipsis: true, tooltip: true },
  { title: t('instances.table.actions'), slotName: 'psActions', width: 180, align: 'center' as const, fixed: 'right' as const },
])
</script>

<template>
  <div class="dl-page settings-page dl-split-page">
    <aside class="settings-sidebar dl-sidebar">
      <dsh-menu
        :selected-keys="[section]"
        @menu-item-click="(key: string) => (section = key as SettingsSection)"
      >
        <dsh-menu-item key="launch">{{ t('settings.sections.launch') }}</dsh-menu-item>
        <dsh-menu-item key="notifications">{{ t('settings.notifications.title') }}</dsh-menu-item>
        <dsh-menu-item key="personal">{{ t('settings.sections.personal') }}</dsh-menu-item>
        <dsh-menu-item key="network">{{ t('settings.sections.network') }}</dsh-menu-item>
        <dsh-menu-item key="resources">{{ t('settings.sections.resources') }}</dsh-menu-item>
        <dsh-menu-item key="about">{{ t('settings.sections.about') }}</dsh-menu-item>
      </dsh-menu>
    </aside>
    <section class="settings-content dl-page-content">
      <PageScrollbar>
        <div class="settings-inner">
    <template v-if="section === 'personal'">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.general') }}</h3>
      </div>
      <dsh-form :model="store.settings" layout="vertical" class="settings-form">
        <dsh-form-item :label="t('settings.language')">
          <dsh-select
            :model-value="store.settings.locale"
            style="width: 220px"
            @change="onLocaleChange"
          >
            <dsh-option v-for="l in SUPPORTED_LOCALES" :key="l.value" :value="l.value">
              {{ l.label }}
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item :label="t('settings.theme.label')">
          <dsh-select
            :model-value="store.settings.theme"
            style="width: 220px"
            @change="onThemeChange"
          >
            <dsh-option v-for="o in THEME_OPTIONS" :key="o.value" :value="o.value">
              {{ o.label }}
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item><template #label>{{ t('settings.newsSource') }}<HintIcon :content="t('settings.newsSourceHint')" /></template>
          <dsh-input
            v-model="newsSource"
            :placeholder="t('settings.newsSourcePlaceholder')"
            allow-clear
            @blur="onNewsSourceSave"
            @press-enter="onNewsSourceSave"
          />
          </dsh-form-item>
        <dsh-form-item>
          <template #label>
            {{ t('settings.launcherIcon.window') }}
            <HintIcon :content="t('settings.launcherIcon.windowHint')" />
          </template>
          <dsh-space>
            <img v-if="launcherIcons.window" :src="launcherIcons.window" class="launcher-icon-preview" alt="" />
            <dsh-button variant="secondary" size="sm" :loading="launcherIconBusy === 'window'" @click="onPickLauncherIcon('window')">
              {{ t('settings.launcherIcon.pick') }}
            </dsh-button>
            <dsh-button variant="danger"
              v-if="launcherIcons.window"
              size="sm"
              :loading="launcherIconBusy === 'window'"
              @click="onClearLauncherIcon('window')"
            >
              {{ t('settings.launcherIcon.clear') }}
            </dsh-button>
          </dsh-space>
        </dsh-form-item>
        <dsh-form-item>
          <template #label>
            {{ t('settings.launcherIcon.tray') }}
            <HintIcon :content="t('settings.launcherIcon.trayHint')" />
          </template>
          <dsh-space>
            <img v-if="launcherIcons.tray" :src="launcherIcons.tray" class="launcher-icon-preview" alt="" />
            <dsh-button variant="secondary" size="sm" :loading="launcherIconBusy === 'tray'" @click="onPickLauncherIcon('tray')">
              {{ t('settings.launcherIcon.pick') }}
            </dsh-button>
            <dsh-button variant="danger"
              v-if="launcherIcons.tray"
              size="sm"
              :loading="launcherIconBusy === 'tray'"
              @click="onClearLauncherIcon('tray')"
            >
              {{ t('settings.launcherIcon.clear') }}
            </dsh-button>
          </dsh-space>
        </dsh-form-item>
      </dsh-form>
    </div>
    </template>

    <template v-else-if="section === 'notifications'">
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('settings.notifications.title') }}</h3></div>
        <p>{{ t('settings.notifications.hint') }}</p>
        <dsh-space direction="vertical" size="large">
          <dsh-checkbox :model-value="store.settings.notification_system" @change="(value) => patchSettings({ notification_system: Boolean(value) })">
            {{ t('settings.notifications.system') }}
          </dsh-checkbox>
          <dsh-checkbox :model-value="store.settings.notification_sound" @change="(value) => patchSettings({ notification_sound: Boolean(value) })">
            {{ t('settings.notifications.sound') }}
          </dsh-checkbox>
          <dsh-checkbox :model-value="store.settings.notification_tray" @change="(value) => patchSettings({ notification_tray: Boolean(value) })">
            {{ t('settings.notifications.tray') }}
          </dsh-checkbox>
        </dsh-space>
      </div>
    </template>
    <template v-else-if="section === 'launch'">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.launchBehavior.title') }}</h3>
      </div>
      <dsh-form :model="store.settings" layout="vertical" class="settings-form">
        <dsh-form-item>
          <dsh-switch :model-value="store.settings.autostart" @change="onAutostartChange" />
          <span class="switch-label">{{ t('settings.autostart') }}</span>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-switch :model-value="store.settings.minimize_to_tray" @change="onTrayChange" />
          <span class="switch-label">
            {{ t('settings.minimizeToTray') }}
            <HintIcon :content="t('settings.minimizeToTrayHint')" />
          </span>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-switch :model-value="store.settings.auto_open_on_launch" @change="onAutoOpenChange" />
          <span class="switch-label">
            {{ t('settings.launchBehavior.autoOpenOnLaunch') }}
            <HintIcon :content="t('settings.launchBehavior.autoOpenOnLaunchHint')" />
          </span>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-switch
            :model-value="store.settings.hide_launcher_on_window_open"
            @change="onHideLauncherChange"
          />
          <span class="switch-label">
            {{ t('settings.launchBehavior.hideLauncherOnWindowOpen') }}
            <HintIcon :content="t('settings.launchBehavior.hideLauncherOnWindowOpenHint')" />
          </span>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-switch
            :model-value="store.settings.per_instance_tray"
            @change="onPerInstanceTrayChange"
          />
          <span class="switch-label">
            {{ t('settings.perInstanceTray') }}
            <HintIcon :content="t('settings.perInstanceTrayHint')" />
          </span>
        </dsh-form-item>
      </dsh-form>
    </div>

    <!-- Silent autostart + instance auto-launch (issue #100). Only meaningful
         together with the OS autostart entry; shown regardless so the intent
         is discoverable, with a nudge when autostart itself is off. -->
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>
          {{ t('settings.silentAutostart.title') }}
          <HintIcon :content="t('settings.silentAutostart.hint')" />
        </h3>
      </div>
      <dsh-alert v-if="!store.settings.autostart" type="info" class="autostart-note">
        {{ t('settings.silentAutostart.needsAutostart') }}
      </dsh-alert>
      <dsh-form :model="store.settings" layout="vertical" class="settings-form">
        <dsh-form-item>
          <dsh-switch
            :model-value="store.settings.silent_autostart"
            :disabled="!store.settings.autostart"
            @change="onSilentAutostartChange"
          />
          <span class="switch-label">
            {{ t('settings.silentAutostart.silent') }}
            <HintIcon :content="t('settings.silentAutostart.silentHint')" />
          </span>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-switch
            :model-value="store.settings.autostart_open_windows"
            :disabled="!store.settings.autostart"
            @change="onAutostartOpenWindowsChange"
          />
          <span class="switch-label">
            {{ t('settings.silentAutostart.openWindows') }}
            <HintIcon :content="t('settings.silentAutostart.openWindowsHint')" />
          </span>
        </dsh-form-item>
      </dsh-form>

      <div class="autostart-instances">
        <h4>{{ t('settings.silentAutostart.instances') }}</h4>
        <p class="autostart-desc">{{ t('settings.silentAutostart.instancesHint') }}</p>
        <div v-if="autostartInstances.length === 0" class="autostart-empty">
          {{ t('settings.silentAutostart.empty') }}
        </div>
        <div v-for="entry in autostartInstances" :key="entry.instance_id" class="autostart-row">
          <span class="autostart-name">{{ autostartInstanceName(entry.instance_id) }}</span>
          <dsh-tag v-if="entry.profile" size="small">{{ entry.profile }}</dsh-tag>
          <dsh-tag v-else size="small" color="gray">{{ t('settings.silentAutostart.defaultProfile') }}</dsh-tag>
          <dsh-button variant="danger-soft"
            size="sm"
            :disabled="!store.settings.autostart"
            @click="onRemoveAutostartInstance(entry.instance_id)"
          >
            {{ t('settings.silentAutostart.remove') }}
          </dsh-button>
        </div>
        <div class="autostart-add">
          <dsh-select
            v-model="newAutostartInstance"
            :placeholder="t('settings.silentAutostart.pickInstance')"
            :options="autostartCandidates"
            allow-clear
            :disabled="!store.settings.autostart"
            class="autostart-pick"
          />
          <dsh-input
            v-model="newAutostartProfile"
            :placeholder="t('settings.silentAutostart.profilePlaceholder')"
            allow-clear
            :disabled="!store.settings.autostart"
            class="autostart-profile"
          />
          <dsh-button variant="primary"
            :disabled="!store.settings.autostart || !newAutostartInstance"
            @click="onAddAutostartInstance"
          >
            {{ t('settings.silentAutostart.add') }}
          </dsh-button>
        </div>
      </div>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.dataDir.title') }}<HintIcon :content="t('settings.dataDir.hint')" /></h3>
      </div>
      <div class="update-row">
        <span class="data-dir-path" :title="dataDir">{{ dataDir || t('settings.dataDir.unknown') }}</span>
        <dsh-button variant="secondary" size="sm" @click="onOpenDataDir">{{ t('settings.dataDir.open') }}</dsh-button>
        <dsh-button variant="primary" size="sm" @click="onMoveDataDir">{{ t('settings.dataDir.moveTo') }}</dsh-button>
      </div>
      <div class="data-dir-source" :class="`source-${dataDirSource}`">
        {{ dataDirSourceTip }}
      </div>
      <dsh-modal
        v-model:visible="moveModalVisible"
        :title="t('settings.dataDir.moveTo')"
        :ok-text="t('common.confirm')"
        :cancel-text="t('common.cancel')"
        :confirm-loading="moveBusy"
        @ok="onConfirmMove"
      >
        <p class="move-target">{{ moveTarget }}</p>
        <p class="move-hint">{{ t('settings.dataDir.restartHint') }}</p>
      </dsh-modal>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.homes') }}</h3>
      </div>

      <div class="home-add-row">
        <dsh-input v-model="newHomeName" :placeholder="t('settings.homeNamePlaceholder')" style="width: 200px" />
        <dsh-input v-model="newHomePath" :placeholder="t('settings.homePathPlaceholder')" class="home-path-input" />
        <dsh-button variant="secondary" @click="onPickDir">{{ t('settings.pickDir') }}</dsh-button>
        <dsh-button variant="primary" :disabled="!newHomeName.trim() || !newHomePath.trim()" @click="onAddHome">
          {{ t('settings.addHome') }}
        </dsh-button>
      </div>

      <dsh-table :columns="homeColumns" :data="store.homes" :pagination="false" :scroll="{ x: 700 }" row-key="id">
        <template #actions="{ record }">
          <dsh-button variant="danger" size="sm" @click="confirmRemoveHome(record.id, record.name)">
            {{ t('settings.deleteHome') }}
          </dsh-button>
        </template>
      </dsh-table>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.importScan.title') }}</h3>
      </div>
      <p class="news-source-hint">{{ t('settings.importScan.hint') }}</p>
      <dsh-button variant="primary" @click="importScanVisible = true">
        {{ t('settings.importScan.open') }}
      </dsh-button>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.log.title') }}<HintIcon :content="t('settings.logLevel.hint')" /></h3>
      </div>
      <dsh-form :model="store.settings" layout="vertical" class="settings-form">
        <dsh-form-item :label="t('settings.logLevel.label')">
          <dsh-select
            :model-value="store.settings.log_level"
            style="width: 220px"
            @change="onLogLevelChange"
          >
            <dsh-option v-for="o in LOG_LEVEL_OPTIONS" :key="o.value" :value="o.value">
              {{ o.label }}
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item>
          <dsh-button variant="secondary" size="sm" @click="onOpenLauncherLog">{{ t('settings.dataDir.viewLog') }}</dsh-button>
        </dsh-form-item>
      </dsh-form>
    </div>
    </template>

    <template v-else-if="section === 'network'">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.proxy.title') }}</h3>
      </div>
      <dsh-form :model="store.settings" layout="vertical" class="settings-form">
        <dsh-form-item>
          <dsh-switch :model-value="store.settings.proxy_enabled" @change="onProxyEnabledChange" />
          <span class="switch-label">{{ t('settings.proxy.enabled') }}<HintIcon :content="t('settings.proxy.enabledHint')" /></span>
        </dsh-form-item>
        <dsh-form-item :label="t('settings.proxy.url')">
          <dsh-input
            v-model="proxyUrl"
            :disabled="!store.settings.proxy_enabled"
            placeholder="http://127.0.0.1"
            @blur="onProxyFieldsSave"
            @press-enter="onProxyFieldsSave"
          />
        </dsh-form-item>
        <dsh-form-item :label="t('settings.proxy.port')">
          <dsh-input-number
            v-model="proxyPort"
            :disabled="!store.settings.proxy_enabled"
            :min="1"
            :max="65535"
            style="width: 220px"
            @blur="onProxyFieldsSave"
          />
        </dsh-form-item>
        <dsh-form-item><template #label>{{ t('settings.proxy.noProxy') }}<HintIcon :content="t('settings.proxy.noProxyHint')" /></template>
          <dsh-input
            v-model="noProxy"
            :disabled="!store.settings.proxy_enabled"
            placeholder="127.0.0.1,localhost,::1"
            @blur="onProxyFieldsSave"
            @press-enter="onProxyFieldsSave"
          />
          </dsh-form-item>
        <dsh-form-item>
          <dsh-switch
            :model-value="store.settings.proxy_apply_dsh"
            :disabled="!store.settings.proxy_enabled"
            @change="onProxyApplyDshChange"
          />
          <span class="switch-label">{{ t('settings.proxy.applyDsh') }}<HintIcon :content="t('settings.proxy.applyDshHint')" /></span>
        </dsh-form-item>
      </dsh-form>
    </div>
    </template>

    <template v-else-if="section === 'resources'">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.skillRepos.title') }}<HintIcon :content="t('settings.skillRepos.hint')" /></h3>
      </div>
      <div class="skill-repo-add">
        <dsh-input
          v-model="newSkillRepo"
          :placeholder="t('settings.skillRepos.placeholder')"
          allow-clear
          @press-enter="onAddSkillRepo"
        />
        <dsh-button variant="secondary" :loading="skillRepoBusy" :disabled="!newSkillRepo.trim()" @click="onAddSkillRepo">
          {{ t('settings.skillRepos.add') }}
        </dsh-button>
      </div>
      <dsh-list :data="store.settings.skill_repos" size="small">
        <template #item="{ item }">
          <dsh-list-item>
            <span class="skill-repo-url">{{ item }}</span>
            <template #actions>
              <dsh-button variant="danger-soft" size="sm" @click="onRemoveSkillRepo(item)">
                {{ t('instances.table.delete') }}
              </dsh-button>
            </template>
          </dsh-list-item>
        </template>
        <template #empty>
          <dsh-empty :description="t('settings.skillRepos.empty')" />
        </template>
      </dsh-list>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.pluginSources.title') }}<HintIcon :content="t('settings.pluginSources.hint')" /></h3>
      </div>
      <div class="plugin-source-add">
        <dsh-input
          v-model="newSourceId"
          :placeholder="t('settings.pluginSources.id')"
          allow-clear
          style="width: 170px"
        />
        <dsh-select v-model="newSourceKind" style="width: 150px">
          <dsh-option v-for="o in SOURCE_KIND_OPTIONS" :key="o.value" :value="o.value">
            {{ o.label }}
          </dsh-option>
        </dsh-select>
        <dsh-input
          v-model="newSourceUrl"
          :placeholder="t('settings.pluginSources.url')"
          allow-clear
          class="plugin-source-url-input"
        />
        <dsh-button variant="secondary"
          :loading="pluginSourceBusy"
          :disabled="!newSourceId.trim() || (newSourceKind !== 'github-topic' && !newSourceUrl.trim())"
          @click="onAddPluginSource"
        >
          {{ t('settings.pluginSources.add') }}
        </dsh-button>
      </div>
      <dsh-table
        :columns="pluginSourceColumns"
        :data="store.settings.plugin_sources"
        :pagination="false"
        :scroll="{ x: 980 }"
        row-key="id"
        size="small"
      >
        <template #psEnable="{ record }">
          <dsh-switch
            :model-value="record.enabled"
            :disabled="pluginSourceBusy"
            @change="(v) => onPluginSourceEnabledChange(record, v)"
          />
        </template>
        <template #psKind="{ record }">
          <dsh-tag size="small">{{ t(`settings.pluginSources.sourceKinds.${
            record.kind === 'dsh-get' ? 'dshGet' : record.kind === 'github-topic' ? 'githubTopic' : record.kind
          }`) }}</dsh-tag>
        </template>
        <template #psConf="{ record }">
          <dsh-tag size="small" :color="CONFIDENCE_TAG_COLORS[record.confidence as Confidence]">
            {{ t(`plugins.confidence.${record.confidence}`) }}
          </dsh-tag>
        </template>
        <template #psActions="{ record, rowIndex }">
          <dsh-space :size="4">
            <dsh-button variant="ghost" size="sm" :title="t('settings.pluginSources.moveUp')" :aria-label="t('settings.pluginSources.moveUp')" :disabled="pluginSourceBusy || rowIndex === 0" @click="onMovePluginSource(rowIndex, -1)">
              <IconArrowUp />
            </dsh-button>
            <dsh-button variant="ghost"
              size="sm"
              :title="t('settings.pluginSources.moveDown')"
              :aria-label="t('settings.pluginSources.moveDown')"
              :disabled="pluginSourceBusy || rowIndex === store.settings.plugin_sources.length - 1"
              @click="onMovePluginSource(rowIndex, 1)"
            >
              <IconArrowDown />
            </dsh-button>
            <dsh-button variant="danger-soft" size="sm" :disabled="pluginSourceBusy" @click="onRemovePluginSource(record.id)">
              {{ t('settings.pluginSources.delete') }}
            </dsh-button>
          </dsh-space>
        </template>
        <template #empty>
          <dsh-empty :description="t('settings.pluginSources.empty')" />
        </template>
      </dsh-table>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.pluginUpdates.title') }}<HintIcon :content="t('settings.pluginUpdates.hint')" /></h3>
      </div>
      <div class="update-row">
        <dsh-select
          :model-value="pluginUpdateChannel"
          style="width: 200px"
          @change="onPluginUpdateChannelChange"
        >
          <dsh-option v-for="o in PLUGIN_UPDATE_CHANNEL_OPTIONS" :key="o.value" :value="o.value">
            {{ o.label }}
          </dsh-option>
        </dsh-select>
        <HintIcon :content="t('settings.pluginUpdates.channelHint')" />
      </div>
    </div>

    </template>

    <template v-else>
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.about.title') }}</h3>
      </div>
      <div class="update-row">
        <span class="update-current">v{{ launcherVersion }}</span>
        <dsh-tag v-if="updateInfo?.channel === 'dev' || launcherVersion.includes('-dev.')" color="orange" size="small">
          {{ t('settings.update.devChannel') }}
        </dsh-tag>
        <dsh-select
          :model-value="updateChannel"
          class="update-channel-select"
          size="small"
          @change="onUpdateChannelChange"
        >
          <dsh-option v-for="o in UPDATE_CHANNEL_OPTIONS" :key="o.value" :value="o.value">
            {{ o.label }}
          </dsh-option>
        </dsh-select>
        <dsh-button variant="secondary" size="sm" :loading="checkingUpdate" @click="onCheckUpdate">
          {{ t('settings.update.check') }}
        </dsh-button>
      </div>
      <div class="update-result">
        <span class="about-channel-hint">{{ t('settings.update.channelHint') }}</span>
      </div>
      <div v-if="updateInfo && !updateInfo.up_to_date" class="update-result">
        <dsh-alert type="info" :show-icon="true">
          {{ t('settings.update.available', { version: updateInfo.latest }) }}
          <template v-if="updateInfo.url">
            <dsh-link href="#" class="update-link" @click.prevent="api.openExternal(updateInfo.url!)">
              {{ t('settings.update.viewRelease') }}
            </dsh-link>
          </template>
        </dsh-alert>
      </div>
      <div v-else-if="updateInfo?.up_to_date" class="update-result">
        <span class="update-up-to-date">{{ t('settings.update.upToDate') }}</span>
      </div>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.about.project') }}</h3>
      </div>
      <div class="about-links">
        <dsh-button variant="secondary" size="sm" @click="api.openExternal(REPO_URL)">
          {{ t('settings.about.repository') }}
        </dsh-button>
        <dsh-button variant="secondary" size="sm" @click="api.openExternal(ISSUES_URL)">
          {{ t('settings.about.issues') }}
        </dsh-button>
        <dsh-button variant="secondary" size="sm" @click="api.openExternal(RELEASES_URL)">
          {{ t('settings.about.releases') }}
        </dsh-button>
      </div>
      <p class="about-meta">{{ t('settings.about.license') }}</p>
    </div>
    </template>
        </div>
      </PageScrollbar>
    </section>

    <ImportScanDialog v-model:visible="importScanVisible" />
  </div>
</template>

<style lang="scss" scoped>
// Sidebar-sectioned settings page (issue #70), mirroring the instance editor.
.settings-page {
  display: flex;
  height: var(--dl-content-height);
  max-width: none;
  margin: 0;
}

.settings-sidebar {
  width: 200px;
  flex-shrink: 0;
}

.settings-content {
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.settings-inner {
  padding: 20px 20px 60px 0;
  max-width: var(--dl-content-max);
  margin: 0 auto;
}

.skill-repo-add {
  display: flex;
  gap: 8px;
  margin: 12px 0;
}

.skill-repo-url {
  font-size: 13px;
  word-break: break-all;
}

.plugin-source-add {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin: 12px 0;
}

.plugin-source-url-input {
  flex: 1;
  min-width: 220px;
}

.settings-form {
  max-width: 560px;
}

.switch-label {
  margin-left: 10px;
  color: var(--color-text-2);
}

.home-add-row {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
}

.home-path-input {
  flex: 1;
}

.update-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 8px;
  /* Version tag and channel select must never wrap or squeeze the version
     string (e.g. v0.2.4-dev.133) onto two lines (issue #13). */
  flex-wrap: nowrap;
}

.update-channel-select {
  width: 140px;
  flex-shrink: 0;
}

.update-row :deep(.arco-tag) {
  flex-shrink: 0;
  white-space: nowrap;
}

.data-dir-path {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  color: var(--color-text-3);
  font-family: monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.data-dir-source {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 4px;
}

.move-target {
  font-family: monospace;
  word-break: break-all;
  background: var(--color-fill-2);
  padding: 8px;
  border-radius: 4px;
}

.move-hint {
  color: var(--color-text-3);
  font-size: 13px;
}

.update-current {
  font-weight: 600;
  white-space: nowrap;
  flex-shrink: 0;
}

.about-links {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 4px;
}

.about-channel-hint,
.about-meta {
  color: var(--color-text-3);
  font-size: 13px;
}

.about-meta {
  margin-top: 12px;
}

.update-result {
  margin-top: 8px;
}

.update-link {
  margin-left: 8px;
}

.update-up-to-date {
  color: var(--color-text-3);
  font-size: 13px;
}

.launcher-icon-preview {
  width: 32px;
  height: 32px;
  border-radius: 6px;
  border: 1px solid var(--color-border-2);
  object-fit: contain;
}

/* Silent autostart / instance auto-launch (issue #100) */
.autostart-note {
  margin-bottom: 12px;
}

.autostart-instances {
  margin-top: 4px;
}

.autostart-desc {
  color: var(--color-text-3);
  font-size: 13px;
  margin: 4px 0 12px;
}

.autostart-empty {
  color: var(--color-text-3);
  font-size: 13px;
  padding: 8px 0;
}

.autostart-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  border-bottom: 1px solid var(--color-border-1);
}

.autostart-name {
  font-weight: 500;
}

.autostart-add {
  display: flex;
  gap: 8px;
  margin-top: 12px;
}

.autostart-pick {
  width: 220px;
}

.autostart-profile {
  width: 160px;
}
</style>
