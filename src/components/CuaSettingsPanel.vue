<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { HintIcon, useDialog, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { CuaRelease, CuaSettings } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'

const { t } = useI18n()
const { modal } = useDialog()
const { message } = useFeedback()
const store = useLauncherStore()
const settings = ref<CuaSettings | null>(null)
const directory = ref('')
const releases = ref<CuaRelease[]>([])
const selected = ref('')
const loading = ref(false)
const listing = ref(false)
const saving = ref(false)
const starting = ref(false)
const settingsError = ref('')
const directoryError = ref('')
const releaseError = ref('')
const activeDownload = computed(() => store.taskList.some(task => task.kind === 'download-cua'
  && task.version === selected.value && (task.state === 'running' || task.state === 'queued')))

const downloadHint = computed(() => {
  const target = settings.value?.target
  if (!target) {
    return ''
  }
  const platform = t('cua.platform', { target })
  return target.startsWith('win-') ? platform + ' ' + t('cua.wsl') : platform
})

async function loadReleases() {
  if (listing.value || !settings.value?.target) {
    return
  }
  listing.value = true
  releaseError.value = ''
  try {
    releases.value = await api.listCuaReleases()
    if (!releases.value.some(row => row.version === selected.value)) {
      selected.value = ''
    }
  } catch (error) {
    releaseError.value = String(error)
  } finally {
    listing.value = false
  }
}

async function loadSettings() {
  loading.value = true
  settingsError.value = ''
  try {
    settings.value = await api.getCuaSettings()
    directory.value = settings.value.root
    void loadReleases()
  } catch (error) {
    settingsError.value = String(error)
  } finally {
    loading.value = false
  }
}

onMounted(loadSettings)

async function toggleAuto(value: unknown) {
  if (typeof value !== 'boolean' || saving.value) {
    return
  }
  saving.value = true
  try {
    settings.value = await api.updateCuaSettings({ autoComplete: value })
  } catch (error) {
    message.error(String(error))
  } finally {
    saving.value = false
  }
}

async function pickDirectory() {
  try {
    const picked = await api.pickDataDir()
    if (picked) {
      directory.value = picked
      directoryError.value = ''
    }
  } catch (error) {
    directoryError.value = String(error)
  }
}

function confirmDirectory(root: string | null) {
  if (saving.value || !settings.value) {
    return
  }
  const current = settings.value
  const target = root || current.default_root
  if (target === current.root) {
    directory.value = target
    return
  }
  modal.confirm({
    title: t('cua.changeRoot'),
    content: t('cua.confirmRoot', { path: target }),
    okText: t('common.save'),
    cancelText: t('common.cancel'),
    onBeforeOk: async () => {
      saving.value = true
      directoryError.value = ''
      try {
        settings.value = await api.updateCuaSettings({ root })
        directory.value = settings.value.root
        message.success(t('settings.saved'))
      } catch (error) {
        directory.value = current.root
        directoryError.value = t('cua.rootFailed', { error: String(error) })
      } finally {
        saving.value = false
      }
      return true
    },
  })
}

async function download() {
  if (!selected.value || !settings.value?.target || starting.value || activeDownload.value) {
    return
  }
  starting.value = true
  try {
    await api.startDownloadCuaTask(selected.value)
    await store.refreshTasks()
    store.notifyTaskQueued()
    message.success(t('download.taskAdded'))
  } catch (error) {
    message.error(String(error))
  } finally {
    starting.value = false
  }
}

watch(() => store.taskList.filter(task => task.kind === 'download-cua' && task.state === 'done'), (current, previous) => {
  const previousIds = new Set(previous.map(task => task.id))
  for (const task of current) {
    if (previousIds.has(task.id)) continue
    const release = releases.value.find(row => row.version === task.version)
    if (release) release.installed = true
  }
})
</script>

<template>
  <div class="dl-card">
    <div class="dl-card-title"><h3>{{ t('cua.title') }}</h3></div>
    <p v-if="loading" role="status">{{ t('common.loading') }}</p>
    <div v-else-if="settingsError" role="alert">
      <p>{{ settingsError }}</p>
      <dsh-button @click="loadSettings">{{ t('common.retry') }}</dsh-button>
    </div>
    <dsh-form v-else-if="settings" :model="settings" layout="vertical" class="cua-form">
      <dsh-form-item>
        <template #label>
          {{ t('cua.auto') }}
          <HintIcon :content="t('cua.autoHint')" />
        </template>
        <dsh-switch :model-value="settings.auto_complete" :aria-label="t('cua.auto')"
          :disabled="saving || !settings.target" @change="toggleAuto" />
      </dsh-form-item>
      <dsh-form-item>
        <template #label>
          {{ t('cua.root') }}
          <HintIcon :content="t('cua.rootHint')" />
        </template>
        <div class="cua-controls">
          <dsh-input v-model="directory" :aria-label="t('cua.root')" :disabled="saving" @input="directoryError = ''" />
          <dsh-button variant="secondary" :disabled="saving" @click="pickDirectory">{{ t('settings.pickDir') }}</dsh-button>
          <dsh-button variant="primary" :disabled="saving || (directory.trim() || settings.default_root) === settings.root"
            @click="confirmDirectory(directory.trim() || null)">{{ t('common.save') }}</dsh-button>
          <dsh-button variant="ghost" :disabled="saving || settings.root === settings.default_root"
            @click="confirmDirectory(null)">{{ t('cua.resetRoot') }}</dsh-button>
        </div>
      </dsh-form-item>
      <p v-if="directoryError" role="alert" class="cua-error">{{ directoryError }}</p>
    </dsh-form>
  </div>
  <div v-if="settings" class="dl-card">
    <div class="dl-card-title">
      <h3>
        {{ t('cua.manual') }}
        <HintIcon v-if="settings.target" :content="downloadHint" />
      </h3>
      <dsh-button variant="ghost" size="sm" :disabled="!settings.target || listing" :loading="listing"
        @click="loadReleases">{{ t('common.refresh') }}</dsh-button>
    </div>
    <p v-if="!settings.target" role="status" class="cua-hint">
      {{ t('cua.unsupported') }}<HintIcon :content="t('cua.supportedPlatforms')" />
    </p>
    <p v-if="releaseError" role="alert" class="cua-error">{{ releaseError }}</p>
    <p v-else-if="settings.target && !listing && !releases.length" role="status" class="cua-hint">{{ t('cua.empty') }}</p>
    <div class="cua-controls">
      <dsh-select v-model="selected" :aria-label="t('cua.version')" :placeholder="t('cua.version')"
        :disabled="!settings.target || !releases.length || listing" allow-search>
        <dsh-option v-for="row in releases" :key="row.version" :value="row.version">
          {{ row.version }}{{ row.prerelease ? ' · ' + t('download.prerelease') : '' }}{{ row.installed ? ' · ' + t('download.installedTag') : '' }}
        </dsh-option>
      </dsh-select>
      <dsh-button variant="primary" :disabled="!settings.target || !selected || saving || starting || activeDownload"
        :loading="starting || activeDownload" @click="download">{{ t('cua.download') }}</dsh-button>
    </div>
  </div>
</template>

<style scoped>
.cua-form { max-width: 850px; }
.cua-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; width: 100%; }
.cua-controls :deep(.arco-input-wrapper) { flex: 1 1 280px; }
.cua-controls :deep(.arco-select) { width: min(100%, 320px); }
.cua-hint, .cua-error { margin: 0 0 12px; overflow-wrap: anywhere; }
.cua-hint { color: var(--color-text-2); }
.cua-error { color: rgb(var(--danger-6)); }
</style>
