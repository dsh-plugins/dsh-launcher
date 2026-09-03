<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'

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
const busy = ref(false)

const dedicated = computed(() => homeId.value === DEDICATED)

watch(homeId, async (v) => {
  if (v === DEDICATED && !dedicatedPath.value) {
    dedicatedPath.value = await api.defaultDedicatedHomePath(instanceName.value.trim() || version.value)
  }
}, { immediate: true })

watch(instanceName, async (v) => {
  if (dedicated.value) {
    dedicatedPath.value = await api.defaultDedicatedHomePath(v.trim() || version.value)
  }
})

const canConfirm = computed(
  () =>
    !busy.value &&
    instanceName.value.trim().length > 0 &&
    (wslSelected.value ? !isSourceBuild.value : !!homeId.value) &&
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
      )
    }
    // Pull the task list so the task page shows the new task immediately.
    await store.refreshTasks()
    Message.success(t('download.taskAdded'))
    router.push({ name: 'tasks' })
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
        <h3>{{ t('download.runtimeEnv') }}</h3>
      </div>
      <a-radio-group v-model="runtime" type="button">
        <a-radio :value="WINDOWS">{{ t('download.runtimeWindows') }}</a-radio>
        <a-radio v-for="d in distros" :key="d" :value="d">WSL（{{ d }}）</a-radio>
      </a-radio-group>
      <a-alert v-if="wslSelected && isSourceBuild" type="warning" class="dedicated-hint">
        {{ t('download.wslAlphaUnsupported') }}
      </a-alert>
      <a-alert v-else-if="wslSelected" type="info" class="dedicated-hint">
        {{ t('download.wslHomeHint', { distro: runtime }) }}
      </a-alert>
    </div>

    <!-- DSH_HOME selection -->
    <div v-if="!wslSelected" class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('download.chooseHome') }}</h3>
      </div>
      <a-select v-model="homeId" style="width: 100%; max-width: 480px">
        <a-option :value="DEDICATED">{{ t('download.dedicatedHome') }}</a-option>
        <a-option v-for="h in store.homes" :key="h.id" :value="h.id">
          {{ h.name }}（{{ h.path }}）
        </a-option>
      </a-select>
      <a-alert v-if="dedicated" type="info" class="dedicated-hint">
        {{ t('download.dedicatedHomeHint', { path: dedicatedPath }) }}
      </a-alert>
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

.confirm-button {
  min-width: 220px;
  height: 48px;
  border-radius: 24px;
  font-size: 16px;
  font-weight: 600;
}
</style>
