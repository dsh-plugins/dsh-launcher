<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ToggleButtonGroup, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const { message: Message } = useFeedback()

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const version = computed(() => String(route.params.version ?? ''))
const installedVersion = computed(() => store.versions.find((v) => v.version === version.value && !v.wsl))
const isSourceBuild = computed(() =>
  store.remoteVersions.some((v) => v.version === version.value && v.source === 'github'),
)

const WINDOWS = 'windows'
const DOWNLOAD_ONLY = 'download-only'
const WSL_PREFIX = 'wsl:'
const mode = ref<string>(WINDOWS)
const distros = ref<string[]>([])
const modes = computed(() => [
  { value: WINDOWS, label: t('download.runtimeWindows') },
  ...distros.value.map((distro) => ({
    value: `${WSL_PREFIX}${distro}`,
    label: t('download.runtimeWsl', { distro }),
  })),
  { value: DOWNLOAD_ONLY, label: t('download.downloadOnly') },
])
const windowsSelected = computed(() => mode.value === WINDOWS)
const downloadOnly = computed(() => mode.value === DOWNLOAD_ONLY)
const wslDistro = computed(() => mode.value.startsWith(WSL_PREFIX) ? mode.value.slice(WSL_PREFIX.length) : '')
const wslSourceUnsupported = computed(() =>
  !!wslDistro.value && isSourceBuild.value &&
  !store.versions.some((v) => v.version === version.value && v.wsl === wslDistro.value),
)
const versionDownloading = computed(() => store.downloadVersionBusy(version.value))

onMounted(async () => {
  try {
    distros.value = await api.listWslDistros()
  } catch {
    distros.value = []
  }
})

// Default instance name: version string, deduplicated against existing names.
function suggestName(): string {
  let candidate = version.value
  let n = 2
  while (store.instances.some((i) => i.name === candidate)) {
    candidate = `${version.value}-${n}`
    n += 1
  }
  return candidate
}

const instanceName = ref(suggestName())
const DEDICATED = '__dedicated__'
const homeId = ref<string | undefined>(DEDICATED)
const dedicatedPath = ref('')
// Dedicated HOME display name (issue #26): syncs from the instance name
// until the user edits it manually, then stays decoupled.
const dedicatedName = ref(instanceName.value)
const dedicatedNameTouched = ref(false)
const busy = ref(false)

const dedicated = computed(() => homeId.value === DEDICATED)

watch([windowsSelected, dedicated, dedicatedName], async ([isWindows, isDedicated, name], _, onCleanup) => {
  if (!isWindows || !isDedicated) {
    return
  }
  let stale = false
  onCleanup(() => { stale = true })
  try {
    const path = await api.defaultDedicatedHomePath(name.trim() || instanceName.value.trim() || version.value)
    if (!stale) {
      dedicatedPath.value = path
    }
  } catch (e) {
    if (!stale) {
      Message.error(String(e))
    }
  }
}, { immediate: true })

watch(instanceName, (v) => {
  if (!dedicatedNameTouched.value) {
    dedicatedName.value = v
  }
})

function onDedicatedNameInput(v: string) {
  dedicatedName.value = v
  dedicatedNameTouched.value = true
}

const canConfirm = computed(() => {
  if (busy.value || !version.value) {
    return false
  }
  if (downloadOnly.value) {
    return !installedVersion.value && !versionDownloading.value
  }
  const name = instanceName.value.trim()
  if (!name || store.instances.some((i) => i.name === name) || store.instanceNameBusy(name)) {
    return false
  }
  if (wslDistro.value) {
    return !wslSourceUnsupported.value
  }
  return windowsSelected.value && Boolean(homeId.value) && (!dedicated.value || dedicatedName.value.trim().length > 0)
})

async function onConfirm() {
  if (!canConfirm.value) {
    return
  }
  busy.value = true
  try {
    if (downloadOnly.value) {
      await api.startDownloadVersionTask(version.value)
    } else if (wslDistro.value) {
      await api.startCreateWslInstanceTask(instanceName.value.trim(), version.value, wslDistro.value)
    } else {
      await api.startCreateInstanceTask(
        instanceName.value.trim(),
        version.value,
        dedicated.value ? null : (homeId.value ?? null),
        dedicated.value,
        dedicated.value ? dedicatedName.value.trim() : null,
      )
    }
    // Pull the task list so the badge/fly animation has data, then return to
    // the version list (kept alive) instead of jumping to the task page.
    await store.refreshTasks()
    store.notifyTaskQueued()
    Message.success(t('download.taskAdded'))
    router.push({ name: 'download-create' })
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="name-page">
    <!-- Header: back + version icon + name input -->
    <div class="dl-card name-header">
      <dsh-button variant="ghost" class="back-button" @click="router.push({ name: 'download-create' })">
        ←
      </dsh-button>
      <span class="version-icon">◆</span>
      <strong v-if="downloadOnly">{{ version }}</strong>
      <dsh-input
        v-else
        v-model="instanceName"
        :placeholder="t('download.instanceName')"
        class="name-input"
        size="large"
      />
    </div>

    <div class="dl-card">
      <div class="dl-card-title">
        <h3>
          {{ t('download.runtimeEnv') }}
          <HintIcon v-if="wslDistro" :content="t('download.wslHomeHint', { distro: wslDistro })" />
        </h3>
      </div>
      <ToggleButtonGroup
        v-model="mode"
        :options="modes"
        :label="t('download.runtimeEnv')"
        :allow-empty="false"
        :disabled="busy"
        class="runtime-modes"
      />
      <dsh-alert v-if="wslSourceUnsupported" type="warning" class="dedicated-hint">
        {{ t('download.wslAlphaUnsupported') }}
      </dsh-alert>
    </div>

    <!-- DSH_HOME selection -->
    <div v-if="windowsSelected" class="dl-card">
      <div class="dl-card-title">
        <h3>
          {{ t('download.chooseHome') }}
          <HintIcon v-if="dedicated" :content="t('download.dedicatedHomeHint', { path: dedicatedPath })" />
        </h3>
      </div>
      <dsh-select v-model="homeId" style="width: 100%; max-width: 480px">
        <dsh-option :value="DEDICATED">{{ t('download.dedicatedHome') }}</dsh-option>
        <dsh-option v-for="h in store.homes" :key="h.id" :value="h.id">
          {{ h.name }}（{{ h.path }}）
        </dsh-option>
      </dsh-select>
      <div v-if="dedicated" class="dedicated-name-row">
        <span class="dedicated-name-label">{{ t('download.dedicatedHomeName') }}</span>
        <dsh-input
          :model-value="dedicatedName"
          :placeholder="t('download.dedicatedHomeNamePlaceholder')"
          style="max-width: 320px"
          @update:model-value="onDedicatedNameInput"
        />
      </div>
    </div>

    <!-- Action -->
    <div class="confirm-area">
      <dsh-alert v-if="!wslDistro && !installedVersion && isSourceBuild" type="warning" class="confirm-hint">
        {{ t('download.sourceBuildHint') }}
      </dsh-alert>
      <dsh-alert v-if="downloadOnly && installedVersion" type="info" class="confirm-hint">
        {{ t('download.downloadOnlyInstalled') }}
      </dsh-alert>
      <dsh-alert v-else-if="downloadOnly && versionDownloading" type="info" class="confirm-hint">
        {{ t('download.downloadInProgress', { version }) }}
      </dsh-alert>
      <dsh-alert v-else-if="downloadOnly" type="info" class="confirm-hint">
        {{ t('download.willDownloadOnly', { version }) }}
      </dsh-alert>
      <dsh-alert v-else-if="windowsSelected && installedVersion" type="info" class="confirm-hint">
        {{ t('download.alreadyInstalled') }}
      </dsh-alert>
      <dsh-alert v-else-if="windowsSelected" type="info" class="confirm-hint">
        {{ t('download.willInstall', { version }) }}
      </dsh-alert>
      <dsh-button variant="primary"
        size="lg"
        class="confirm-button"
        :disabled="!canConfirm"
        :loading="busy"
        @click="onConfirm"
      >
        {{
          wslDistro
            ? t('download.createWslInstance')
            : windowsSelected && installedVersion
              ? t('download.createOnly')
              : t('download.startDownload')
        }}
      </dsh-button>
    </div>
  </div>
</template>

<style lang="scss" scoped>
.name-page {
  max-width: 860px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-height: calc(var(--dl-content-height) - 120px);
}

.name-header {
  display: flex;
  align-items: center;
  gap: 12px;
}

.back-button {
  font-size: 18px;
  padding: 0 8px;
}

.version-icon {
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #0fc6c2, #165dff);
  color: #fff;
  font-size: 16px;
  flex-shrink: 0;
}

.name-input {
  flex: 1;
}

.runtime-modes {
  flex-wrap: wrap;
  border-radius: 999px;

  :deep(.dsh-toggle-button) {
    border-radius: 999px;
  }
}

.confirm-area {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
  padding-top: 24px;
}

.confirm-hint {
  max-width: 520px;
}

.dedicated-hint {
  margin-top: 12px;
  max-width: 480px;
}

.dedicated-name-row {
  margin-top: 12px;
  display: flex;
  align-items: center;
  gap: 10px;
}

.dedicated-name-label {
  flex-shrink: 0;
  color: var(--color-text-2);
}

.confirm-button {
  min-width: 220px;
  height: 48px;
  border-radius: 24px;
  font-size: 16px;
  font-weight: 600;
}
</style>
