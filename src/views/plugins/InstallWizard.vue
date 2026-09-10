<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const state = computed(() => store.pluginWizard)

const instanceId = ref<string>('')
const profile = ref<string>('')
const allowBuildScripts = ref(true)
const submitting = ref(false)

const profiles = ref<string[]>([])
const profilesLoading = ref(false)

const instances = computed(() =>
  // Plugin installs operate on local files; WSL instances are unsupported.
  store.instances.filter((i) => !store.homeById(i.home_id)?.wsl),
)
const selectedInstance = computed(() => store.instanceById(instanceId.value))

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
  () => !!state.value && !!instanceId.value && !!profile.value && !submitting.value,
)

async function startInstall() {
  const s = state.value
  if (!s || !s.version) return
  submitting.value = true
  try {
    const taskId = await api.startInstallPluginTask({
      pluginId: s.plugin.id,
      version: s.version.version,
      channel: s.channel,
      instanceId: instanceId.value,
      profile: profile.value,
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
    <a-page-header
      class="wizard-header"
      :title="t('plugins.installTitle')"
      :sub-title="state ? `${state.plugin.name} ${displayVersion(state.version?.version ?? '')}` : ''"
      @back="router.back()"
    />

    <div v-if="!state" class="wizard-empty">
      <a-empty :description="t('plugins.noMatch')" />
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

      <!-- Instance + profile -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('plugins.chooseInstance') }}</h3></div>
        <div v-if="instances.length === 0" class="no-instance">
          <a-empty :description="t('plugins.noInstance')">
            <template #actions>
              <a-button type="primary" size="small" @click="router.push({ name: 'download-create' })">
                {{ t('plugins.goDownload') }}
              </a-button>
            </template>
          </a-empty>
        </div>
        <template v-else>
          <a-select
            v-model="instanceId"
            :placeholder="t('plugins.chooseInstance')"
            style="max-width: 320px"
            allow-clear
          >
            <a-option v-for="inst in instances" :key="inst.id" :value="inst.id">
              <span class="option-line">
                {{ inst.name }}
                <a-tag
                  v-if="store.statusOf(inst.id).state === 'running'"
                  size="small"
                  color="green"
                >
                  {{ t('home.status.running') }}
                </a-tag>
              </span>
            </a-option>
          </a-select>

          <div class="profile-section">
            <div class="profile-label">{{ t('plugins.chooseProfile') }}</div>
            <a-select
              v-model="profile"
              :loading="profilesLoading"
              :placeholder="t('plugins.chooseProfile')"
              style="max-width: 320px"
            >
              <a-option v-for="p in profiles" :key="p" :value="p">{{ p }}</a-option>
            </a-select>
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
          <a-switch v-model="allowBuildScripts" :disabled="true" :checked="true" />
        </div>
      </div>

      <div class="wizard-actions">
        <a-button @click="router.back()">{{ t('download.back') }}</a-button>
        <a-button
          type="primary"
          :disabled="!canSubmit"
          :loading="submitting"
          @click="startInstall"
        >
          {{ t('plugins.startInstall') }}
        </a-button>
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

.no-instance {
  padding: 8px 0;
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
