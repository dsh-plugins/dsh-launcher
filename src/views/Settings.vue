<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { LauncherUpdateInfo, LogLevel, ThemeMode } from '@/api/types'
import { SUPPORTED_LOCALES } from '@/i18n'
import { useLauncherStore } from '@/stores/launcher'
import ImportScanDialog from '@/components/ImportScanDialog.vue'

const { t } = useI18n()
const store = useLauncherStore()

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
  try {
    launcherVersion.value = await api.getLauncherVersion()
  } catch {
    launcherVersion.value = '?'
  }
  try {
    dataDir.value = await api.getLauncherDirectory()
  } catch {
    dataDir.value = ''
  }
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
}

// --- Data directory ---------------------------------------------------------

const dataDir = ref('')

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
  } catch (e) {
    Message.error(String(e))
  }
}

const homeColumns = computed(() => [
  { title: t('settings.homeName'), dataIndex: 'name', width: 180 },
  { title: t('settings.homePath'), dataIndex: 'path', ellipsis: true, tooltip: true },
  { title: t('instances.table.actions'), slotName: 'actions', width: 110, align: 'center' as const },
])
</script>

<template>
  <div class="dl-page">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.importScan.title') }}</h3>
      </div>
      <p class="news-source-hint">{{ t('settings.importScan.hint') }}</p>
      <a-button type="primary" @click="importScanVisible = true">
        {{ t('settings.importScan.open') }}
      </a-button>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.general') }}</h3>
      </div>
      <a-form :model="store.settings" layout="vertical" class="settings-form">
        <a-form-item :label="t('settings.language')">
          <a-select
            :model-value="store.settings.locale"
            style="width: 220px"
            @change="onLocaleChange"
          >
            <a-option v-for="l in SUPPORTED_LOCALES" :key="l.value" :value="l.value">
              {{ l.label }}
            </a-option>
          </a-select>
        </a-form-item>
        <a-form-item :label="t('settings.theme.label')">
          <a-select
            :model-value="store.settings.theme"
            style="width: 220px"
            @change="onThemeChange"
          >
            <a-option v-for="o in THEME_OPTIONS" :key="o.value" :value="o.value">
              {{ o.label }}
            </a-option>
          </a-select>
        </a-form-item>
        <a-form-item>
          <a-switch :model-value="store.settings.minimize_to_tray" @change="onTrayChange" />
          <span class="switch-label">{{ t('settings.minimizeToTray') }}</span>
        </a-form-item>
        <a-form-item>
          <a-switch :model-value="store.settings.autostart" @change="onAutostartChange" />
          <span class="switch-label">{{ t('settings.autostart') }}</span>
        </a-form-item>
        <a-form-item :label="t('settings.logLevel.label')">
          <a-select
            :model-value="store.settings.log_level"
            style="width: 220px"
            @change="onLogLevelChange"
          >
            <a-option v-for="o in LOG_LEVEL_OPTIONS" :key="o.value" :value="o.value">
              {{ o.label }}
            </a-option>
          </a-select>
          <p class="news-source-hint">{{ t('settings.logLevel.hint') }}</p>
        </a-form-item>
        <a-form-item :label="t('settings.newsSource')">
          <a-input
            v-model="newsSource"
            :placeholder="t('settings.newsSourcePlaceholder')"
            allow-clear
            @blur="onNewsSourceSave"
            @press-enter="onNewsSourceSave"
          />
          <p class="news-source-hint">{{ t('settings.newsSourceHint') }}</p>
        </a-form-item>
      </a-form>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.proxy.title') }}</h3>
      </div>
      <a-form :model="store.settings" layout="vertical" class="settings-form">
        <a-form-item>
          <a-switch :model-value="store.settings.proxy_enabled" @change="onProxyEnabledChange" />
          <span class="switch-label">{{ t('settings.proxy.enabled') }}</span>
          <p class="news-source-hint">{{ t('settings.proxy.enabledHint') }}</p>
        </a-form-item>
        <a-form-item :label="t('settings.proxy.url')">
          <a-input
            v-model="proxyUrl"
            :disabled="!store.settings.proxy_enabled"
            placeholder="http://127.0.0.1"
            @blur="onProxyFieldsSave"
            @press-enter="onProxyFieldsSave"
          />
        </a-form-item>
        <a-form-item :label="t('settings.proxy.port')">
          <a-input-number
            v-model="proxyPort"
            :disabled="!store.settings.proxy_enabled"
            :min="1"
            :max="65535"
            style="width: 220px"
            @blur="onProxyFieldsSave"
          />
        </a-form-item>
        <a-form-item :label="t('settings.proxy.noProxy')">
          <a-input
            v-model="noProxy"
            :disabled="!store.settings.proxy_enabled"
            placeholder="127.0.0.1,localhost,::1"
            @blur="onProxyFieldsSave"
            @press-enter="onProxyFieldsSave"
          />
          <p class="news-source-hint">{{ t('settings.proxy.noProxyHint') }}</p>
        </a-form-item>
        <a-form-item>
          <a-switch
            :model-value="store.settings.proxy_apply_dsh"
            :disabled="!store.settings.proxy_enabled"
            @change="onProxyApplyDshChange"
          />
          <span class="switch-label">{{ t('settings.proxy.applyDsh') }}</span>
          <p class="news-source-hint">{{ t('settings.proxy.applyDshHint') }}</p>
        </a-form-item>
      </a-form>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.skillRepos.title') }}</h3>
      </div>
      <p class="news-source-hint">{{ t('settings.skillRepos.hint') }}</p>
      <div class="skill-repo-add">
        <a-input
          v-model="newSkillRepo"
          :placeholder="t('settings.skillRepos.placeholder')"
          allow-clear
          @press-enter="onAddSkillRepo"
        />
        <a-button :loading="skillRepoBusy" :disabled="!newSkillRepo.trim()" @click="onAddSkillRepo">
          {{ t('settings.skillRepos.add') }}
        </a-button>
      </div>
      <a-list :data="store.settings.skill_repos" size="small">
        <template #item="{ item }">
          <a-list-item>
            <span class="skill-repo-url">{{ item }}</span>
            <template #actions>
              <a-button size="mini" status="danger" type="text" @click="onRemoveSkillRepo(item)">
                {{ t('instances.table.delete') }}
              </a-button>
            </template>
          </a-list-item>
        </template>
        <template #empty>
          <a-empty :description="t('settings.skillRepos.empty')" />
        </template>
      </a-list>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.update.title') }}</h3>
      </div>
      <div class="update-row">
        <span class="update-current">v{{ launcherVersion }}</span>
        <a-tag v-if="updateInfo?.channel === 'dev' || launcherVersion.includes('-dev.')" color="orange" size="small">
          {{ t('settings.update.devChannel') }}
        </a-tag>
        <a-select
          :model-value="updateChannel"
          class="update-channel-select"
          size="small"
          @change="onUpdateChannelChange"
        >
          <a-option v-for="o in UPDATE_CHANNEL_OPTIONS" :key="o.value" :value="o.value">
            {{ o.label }}
          </a-option>
        </a-select>
        <a-button size="small" :loading="checkingUpdate" @click="onCheckUpdate">
          {{ t('settings.update.check') }}
        </a-button>
      </div>
      <p class="news-source-hint">{{ t('settings.update.channelHint') }}</p>
      <div v-if="updateInfo && !updateInfo.up_to_date" class="update-result">
        <a-alert type="info" :show-icon="true">
          {{ t('settings.update.available', { version: updateInfo.latest }) }}
          <template v-if="updateInfo.url">
            <a-link class="update-link" @click="api.openExternal(updateInfo.url!)">
              {{ t('settings.update.viewRelease') }}
            </a-link>
          </template>
        </a-alert>
      </div>
      <div v-else-if="updateInfo?.up_to_date" class="update-result">
        <span class="update-up-to-date">{{ t('settings.update.upToDate') }}</span>
      </div>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.dataDir.title') }}</h3>
      </div>
      <p class="news-source-hint">{{ t('settings.dataDir.hint') }}</p>
      <div class="update-row">
        <span class="data-dir-path" :title="dataDir">{{ dataDir || t('settings.dataDir.unknown') }}</span>
        <a-button size="small" @click="onOpenDataDir">{{ t('settings.dataDir.open') }}</a-button>
        <a-button size="small" @click="onOpenLauncherLog">{{ t('settings.dataDir.viewLog') }}</a-button>
      </div>
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('settings.homes') }}</h3>
      </div>

      <div class="home-add-row">
        <a-input v-model="newHomeName" :placeholder="t('settings.homeNamePlaceholder')" style="width: 200px" />
        <a-input v-model="newHomePath" :placeholder="t('settings.homePathPlaceholder')" class="home-path-input" />
        <a-button @click="onPickDir">{{ t('settings.pickDir') }}</a-button>
        <a-button type="primary" :disabled="!newHomeName.trim() || !newHomePath.trim()" @click="onAddHome">
          {{ t('settings.addHome') }}
        </a-button>
      </div>

      <a-table :columns="homeColumns" :data="store.homes" :pagination="false" row-key="id">
        <template #actions="{ record }">
          <a-popconfirm
            :content="t('settings.confirmDeleteHome', { name: record.name })"
            @ok="onRemoveHome(record.id)"
          >
            <a-button size="small" status="danger">{{ t('settings.deleteHome') }}</a-button>
          </a-popconfirm>
        </template>
      </a-table>
    </div>

    <ImportScanDialog v-model:visible="importScanVisible" />
  </div>
</template>

<style lang="scss" scoped>
.skill-repo-add {
  display: flex;
  gap: 8px;
  margin: 12px 0;
}

.skill-repo-url {
  font-size: 13px;
  word-break: break-all;
}
.settings-form {
  max-width: 560px;
}

.switch-label {
  margin-left: 10px;
  color: var(--color-text-2);
}

.news-source-hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--color-text-3);
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
}

.update-channel-select {
  width: 140px;
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

.update-current {
  font-weight: 600;
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
</style>
