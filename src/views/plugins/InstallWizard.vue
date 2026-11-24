<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { compatibility, pluginName, rangeParts, useCompatibilityAck } from '@/utils/market'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const { message: Message } = useFeedback()

const router = useRouter()
const { t, locale } = useI18n()
const store = useLauncherStore()

const state = computed(() => store.pluginWizard)

/** 插件的显示名称，按界面语言从多语言名称中取值。 */
const pluginTitle = computed(() => pluginName(state.value?.plugin.name, locale.value))

const instanceId = ref<string>('')
const profile = ref<string>('')
const allowBuildScripts = ref(true)
const submitting = ref(false)
/** Unverified sources must be explicitly acknowledged before installing. */
const unverifiedAck = ref(false)

const needsUnverifiedAck = computed(() => {
  const c = state.value?.plugin.confidence
  return c === undefined || c === 'unverified'
})

// A different plugin means a fresh acknowledgement.
watch(
  () => state.value?.plugin.id,
  () => {
    unverifiedAck.value = false
  },
)

const profiles = ref<string[]>([])
const profilesLoading = ref(false)

const instances = computed(() => store.instances)
const selectedInstance = computed(() => store.instanceById(instanceId.value))
const runtimeVersion = computed(() => store.versionById(selectedInstance.value?.version_id ?? '')?.version ?? '')
const supportedVersions = computed(() => state.value?.plugin.catalog
  ? state.value.version?.support_versions : state.value?.plugin.support_versions)
const compatibilityResult = computed(() => compatibility(supportedVersions.value, runtimeVersion.value))
const compatibilitySelection = computed(() => JSON.stringify([
  state.value?.plugin.id, state.value?.version, instanceId.value, runtimeVersion.value, supportedVersions.value,
]))
const compatibilityAck = useCompatibilityAck(compatibilitySelection)

const scopedRegistry = computed(() => {
  const source = state.value?.version?.install_source
  if (source?.type !== 'npm' || !source.registry) return ''
  return /^https:\/\/registry\.npmjs\.org\/?$/i.test(source.registry) ? '' : source.registry
})
const scopeSelection = computed(() => JSON.stringify([
  state.value?.plugin.id, state.value?.plugin.source, state.value?.version?.version,
  scopedRegistry.value, instanceId.value, profile.value,
]))
const scopedRegistryAck = useCompatibilityAck(scopeSelection)

/** Distro of the selected instance, or null for a local Windows instance. */
const selectedDistro = computed(() => {
  const inst = selectedInstance.value
  if (!inst) return null
  return store.homeById(inst.home_id)?.wsl ?? null
})

/**
 * 版本号显示：alpha（开发版）是 Git commit 哈希，只显示前 7 位；提交安装
 * 时仍使用完整的哈希（state.version.version 原样传给后端）。
 */
function displayVersion(version: string): string {
  if (state.value?.channel === 'alpha' && /^[0-9a-f]{40}$/i.test(version)) {
    return version.slice(0, 7)
  }
  return version
}

watch(instanceId, async (id) => {
  profile.value = ''
  profiles.value = []
  if (!id) return
  const inst = store.instanceById(id)
  if (!inst) return
  profilesLoading.value = true
  try {
    profiles.value = await api.listProfiles(inst.home_id)
    // Preselect the profile currently selected on the launch page, then the
    // instance default.
    if (store.homeProfile && profiles.value.includes(store.homeProfile)) {
      profile.value = store.homeProfile
    } else if (inst.default_profile && profiles.value.includes(inst.default_profile)) {
      profile.value = inst.default_profile
    }
  } catch (e) {
    Message.error(String(e))
  } finally {
    profilesLoading.value = false
  }
})

onMounted(async () => {
  if (!state.value) {
    router.replace({ name: 'download-plugins' })
    return
  }
  await store.refreshInstances()
  // Preselect the instance currently selected on the launch page (persisted
  // as last_instance_id), falling back to the first installable one.
  if (instances.value.length > 0 && !instanceId.value) {
    const last = store.settings.last_instance_id
    instanceId.value =
      (last && instances.value.some((i) => i.id === last)) ? last : instances.value[0].id
  }
})

const canSubmit = computed(
  () =>
    !!state.value &&
    !!instanceId.value &&
    !!profile.value &&
    !submitting.value &&
    (!needsUnverifiedAck.value || unverifiedAck.value) &&
    (!scopedRegistry.value || scopedRegistryAck.value) &&
    (compatibilityResult.value.status !== 'incompatible' || compatibilityAck.value),
)

async function startInstall() {
  const s = state.value
  if (!s || !s.version || !canSubmit.value) return
  submitting.value = true
  try {
    const taskId = await api.startInstallPluginTask({
      installSource: s.version.install_source,
      scopedRegistryCatalog: scopedRegistry.value && scopedRegistryAck.value ? s.plugin.source : undefined,
      pluginId: s.plugin.id,
      version: s.version.version,
      channel: s.channel,
      instanceId: instanceId.value,
      profile: profile.value,
      repo: s.plugin.repo ?? s.plugin.urls?.repository ?? null,
    })
    Message.success(t('plugins.installTaskAdded'))
    Message.info(t('plugins.installRestartHint'))
    await store.refreshTasks()
    // Stay in the plugin flow: return to the marketplace (kept alive, so the
    // search/scroll position is exactly where the user left it) and play the
    // fly-to-tasks animation instead of jumping to the task list.
    store.notifyTaskQueued()
    router.push({ name: 'download-plugins' })
  } catch (e) {
    Message.error(String(e))
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <div class="install-wizard">
    <dsh-page-header
      class="wizard-header"
      :title="t('plugins.installTitle')"
      :sub-title="state ? `${pluginTitle} ${displayVersion(state.version?.version ?? '')}` : ''"
      @back="router.back()"
    />

    <div v-if="!state" class="wizard-empty">
      <dsh-empty :description="t('plugins.noMatch')" />
    </div>

    <div v-else class="wizard-body">
      <!-- Summary -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('plugins.installTitle') }}</h3></div>
        <div class="summary-row">
          <span class="summary-label">{{ t('plugins.chooseVersion') }}</span>
          <span v-if="state.version" class="summary-value">
            <span class="channel-letter"
              :style="{ background: state.channel === 'stable' ? 'green' : state.channel === 'beta' ? 'orange' : 'red' }"
            >{{ state.channel === 'stable' ? 'R' : state.channel === 'beta' ? 'B' : 'A' }}</span>
            {{ displayVersion(state.version.version) }}
          </span>
        </div>
      </div>

      <!-- Unverified source: explicit acknowledgement required -->
      <div v-if="needsUnverifiedAck" class="unverified-block">
        <dsh-alert type="warning" :show-icon="true">{{ t('plugins.unverifiedConfirm') }}</dsh-alert>
        <dsh-checkbox v-model="unverifiedAck" class="unverified-ack">
          {{ t('plugins.unverifiedAck') }}
        </dsh-checkbox>
      </div>

      <div v-if="scopedRegistry" class="unverified-block">
        <dsh-alert type="warning" :show-icon="true">
          {{ t('plugins.scopeRegistryWarning', {
            scope: state.plugin.id.split('/')[0],
            registry: scopedRegistry,
          }) }}
        </dsh-alert>
        <dsh-checkbox v-model="scopedRegistryAck" class="unverified-ack">
          {{ t('plugins.scopeRegistryAck') }}
        </dsh-checkbox>
      </div>

      <!-- Instance + profile -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('plugins.chooseInstance') }}</h3></div>
        <div v-if="instances.length === 0" class="no-instance">
          <dsh-empty :description="t('plugins.noInstance')">
            <template #actions>
              <dsh-button variant="primary" size="sm" @click="router.push({ name: 'download-create' })">
                {{ t('plugins.goDownload') }}
              </dsh-button>
            </template>
          </dsh-empty>
        </div>
        <template v-else>
          <dsh-select
            v-model="instanceId"
            :placeholder="t('plugins.chooseInstance')"
            style="max-width: 320px"
            allow-clear
          >
            <dsh-option v-for="inst in instances" :key="inst.id" :value="inst.id">
              <span class="option-line">
                {{ inst.name }}
                <dsh-tag
                  v-if="store.statusOf(inst.id).state === 'running'"
                  size="small"
                  color="green"
                >
                  {{ t('home.status.running') }}
                </dsh-tag>
              </span>
            </dsh-option>
          </dsh-select>

          <!-- WSL (issue #49): dependencies are installed inside the distro,
               so native modules resolve to Linux binaries. Say so up front —
               the resulting node_modules is not interchangeable with a
               Windows instance's. -->
          <dsh-alert v-if="selectedDistro" type="info" class="wsl-install-hint">
            {{ t('plugins.wslInstallHint', { distro: selectedDistro }) }}
          </dsh-alert>

          <div v-if="selectedInstance" class="compatibility-block">
            <dsh-tag v-for="part in rangeParts(supportedVersions)" :key="part" size="small">{{ part }}</dsh-tag>
            <dsh-alert v-if="compatibilityResult.status === 'incompatible'" type="warning">
              {{ t('plugins.marketIncompatible', { version: runtimeVersion, range: compatibilityResult.reason }) }}
            </dsh-alert>
            <dsh-checkbox v-if="compatibilityResult.status === 'incompatible'" v-model="compatibilityAck">
              {{ t('plugins.marketCompatibilityAck') }}
            </dsh-checkbox>
            <dsh-alert v-if="compatibilityResult.status === 'unknown'" type="warning">
              {{ t('plugins.marketCompatibilityUnknown', { reason: compatibilityResult.reason }) }}
            </dsh-alert>
            <div v-if="state.plugin.catalog" class="profile-hint">
              {{ state.plugin.catalog.index_url }}
              {{ state.version?.label }}
            </div>
          </div>
          <div class="profile-section">
            <div class="profile-label">{{ t('plugins.chooseProfile') }}</div>
            <dsh-select
              v-model="profile"
              :loading="profilesLoading"
              :placeholder="t('plugins.chooseProfile')"
              style="max-width: 320px"
            >
              <dsh-option v-for="p in profiles" :key="p" :value="p">{{ p }}</dsh-option>
            </dsh-select>
            <div v-if="profiles.length === 0 && instanceId" class="profile-hint">
              {{ t('plugins.noProfile') }}
            </div>
          </div>
        </template>
      </div>

      <!-- buildScripts -->
      <div class="dl-card">
        <div class="dl-card-title">
          <h3>
            buildScripts
            <HintIcon :content="t('plugins.buildScriptsHint')" />
          </h3>
        </div>
        <div class="build-row">
          <dsh-switch v-model="allowBuildScripts" :disabled="true" :checked="true" />
        </div>
      </div>

      <div class="wizard-actions">
        <dsh-button variant="secondary" @click="router.back()">{{ t('download.back') }}</dsh-button>
        <dsh-button variant="primary"
          :disabled="!canSubmit"
          :loading="submitting"
          @click="startInstall"
        >
          {{ t('plugins.startInstall') }}
        </dsh-button>
      </div>
    </div>
  </div>
</template>

<style lang="scss" scoped>
.install-wizard {
  max-width: 720px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.wizard-header {
  padding: 0;
  background: transparent;
}

.wizard-empty {
  padding: 40px 0;
}

.wizard-body {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.summary-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 4px 0;
}

.summary-label {
  color: var(--color-text-3);
  font-size: 13px;
  width: 80px;
  flex-shrink: 0;
}

.summary-value {
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}

.channel-letter {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 5px;
  color: #fff;
  font-size: 12px;
  font-weight: 700;
}

.unverified-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.unverified-ack {
  align-self: flex-start;
}

.no-instance {
  padding: 8px 0;
}

.wsl-install-hint {
  margin: 12px 0;
}

.compatibility-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 12px;
}

.profile-section {
  margin-top: 16px;
}

.profile-label {
  font-size: 13px;
  color: var(--color-text-2);
  margin-bottom: 8px;
}

.profile-hint {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 6px;
}

.build-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.option-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.wizard-actions {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
}
</style>
