<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const version = computed(() => String(route.params.version ?? ''))
const installedVersion = computed(() => store.versions.find((v) => v.version === version.value && !v.wsl))
const isSourceBuild = computed(
  () =>
    !installedVersion.value &&
    store.remoteVersions.some((v) => v.version === version.value && v.source === 'github'),
)

// Runtime environment (issue #19): Windows local or a WSL distro.
const WINDOWS = '__windows__'
const runtime = ref<string>(WINDOWS)
const distros = ref<string[]>([])
const wslSelected = computed(() => runtime.value !== WINDOWS)

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

async function refreshDedicatedPath() {
  dedicatedPath.value = await api.defaultDedicatedHomePath(
    dedicatedName.value.trim() || instanceName.value.trim() || version.value,
  )
}

watch(homeId, async (v) => {
  if (v === DEDICATED && !dedicatedPath.value) {
    await refreshDedicatedPath()
  }
}, { immediate: true })

watch(instanceName, (v) => {
  if (!dedicatedNameTouched.value) {
    dedicatedName.value = v
  }
})

watch(dedicatedName, async () => {
  if (dedicated.value) {
    await refreshDedicatedPath()
  }
})

function onDedicatedNameInput(v: string) {
  dedicatedName.value = v
  dedicatedNameTouched.value = true
}

const canConfirm = computed(
  () =>
    !busy.value &&
    instanceName.value.trim().length > 0 &&
    (wslSelected.value ? !isSourceBuild.value : !!homeId.value) &&
    (!dedicated.value || dedicatedName.value.trim().length > 0) &&
    !store.instances.some((i) => i.name === instanceName.value.trim()) &&
    !store.instanceNameBusy(instanceName.value.trim()),
)

async function onConfirm() {
  if (!canConfirm.value) return
  busy.value = true
  try {
    if (wslSelected.value) {
      await api.startCreateWslInstanceTask(instanceName.value.trim(), version.value, runtime.value)
    } else {
      await api.startCreateInstanceTask(
        instanceName.value.trim(),
        version.value,
        dedicated.value ? null : homeId.value!,
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
      <a-button type="text" class="back-button" @click="router.push({ name: 'download-create' })">
        ←
      </a-button>
      <span class="version-icon">◆</span>
      <a-input
        v-model="instanceName"
        :placeholder="t('download.instanceName')"
        class="name-input"
        size="large"
      />
    </div>

    <!-- Runtime environment: Windows local or a WSL distro (issue #19) -->
    <div v-if="distros.length" class="dl-card">
      <div class="dl-card-title">
        <h3>
          {{ t('download.runtimeEnv') }}
          <HintIcon v-if="wslSelected" :content="t('download.wslHomeHint', { distro: runtime })" />
        </h3>
      </div>
      <a-radio-group v-model="runtime" type="button">
        <a-radio :value="WINDOWS">{{ t('download.runtimeWindows') }}</a-radio>
        <a-radio v-for="d in distros" :key="d" :value="d">WSL（{{ d }}）</a-radio>
      </a-radio-group>
      <a-alert v-if="wslSelected && isSourceBuild" type="warning" class="dedicated-hint">
        {{ t('download.wslAlphaUnsupported') }}
      </a-alert>
    </div>

    <!-- DSH_HOME selection -->
    <div v-if="!wslSelected" class="dl-card">
      <div class="dl-card-title">
        <h3>
          {{ t('download.chooseHome') }}
          <HintIcon v-if="dedicated" :content="t('download.dedicatedHomeHint', { path: dedicatedPath })" />
        </h3>
      </div>
      <a-select v-model="homeId" style="width: 100%; max-width: 480px">
        <a-option :value="DEDICATED">{{ t('download.dedicatedHome') }}</a-option>
        <a-option v-for="h in store.homes" :key="h.id" :value="h.id">
          {{ h.name }}（{{ h.path }}）
        </a-option>
      </a-select>
      <div v-if="dedicated" class="dedicated-name-row">
        <span class="dedicated-name-label">{{ t('download.dedicatedHomeName') }}</span>
        <a-input
          :model-value="dedicatedName"
          :placeholder="t('download.dedicatedHomeNamePlaceholder')"
          style="max-width: 320px"
          @update:model-value="onDedicatedNameInput"
        />
      </div>
    </div>

    <!-- Action -->
    <div class="confirm-area">
      <a-alert v-if="!wslSelected && isSourceBuild" type="warning" class="confirm-hint">
        {{ t('download.sourceBuildHint') }}
      </a-alert>
      <a-alert v-if="!wslSelected && installedVersion" type="info" class="confirm-hint">
        {{ t('download.alreadyInstalled') }}
      </a-alert>
      <a-alert v-else-if="!wslSelected" type="info" class="confirm-hint">
        {{ t('download.willInstall', { version }) }}
      </a-alert>
      <a-button
        type="primary"
        size="large"
        class="confirm-button"
        :disabled="!canConfirm"
        :loading="busy"
        @click="onConfirm"
      >
        {{
          wslSelected
            ? t('download.createWslInstance')
            : installedVersion
              ? t('download.createOnly')
              : t('download.startDownload')
        }}
      </a-button>
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
  min-height: calc(100vh - var(--dl-header-height) - 120px);
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
