<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { InstalledPlugin } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const props = defineProps<{
  visible: boolean
  /** Target: the instance + profile receiving the plugins. */
  targetInstanceId: string
  targetProfile: string
  /** Plugin ids already installed in the target profile. */
  existingIds: string[]
}>()
const emit = defineEmits<{ 'update:visible': [boolean] }>()

const { t } = useI18n()
const store = useLauncherStore()

const sourceInstanceId = ref<string | undefined>(undefined)
const sourceProfile = ref<string | undefined>(undefined)
const sourceProfiles = ref<string[]>([])
const plugins = ref<InstalledPlugin[]>([])
const selected = ref<string[]>([])
const loading = ref(false)
const busy = ref(false)

/** Version strategy: keep the source semver range (default) or install latest. */
const versionStrategy = ref<'source' | 'latest'>('source')
/** Conflict strategy for plugins already present in the target profile. */
const conflictStrategy = ref<'skip' | 'reinstall'>('skip')
/** Also copy the source profile's enabled/disabled state. */
const syncDisabled = ref(true)

/** Local instances other than the target (WSL excluded, as in the install wizard). */
const sourceInstances = computed(() =>
  store.instances.filter((i) => i.id !== props.targetInstanceId && !store.homeById(i.home_id)?.wsl),
)

/** Non-registry sources (git tarballs, local files, links) cannot be reinstalled
 * from the marketplace — the user must reinstall them manually. */
function isUnmigratable(p: InstalledPlugin): boolean {
  const spec = (p.version ?? '').trim()
  return /^(github:|file:|tgz:|link:|workspace:)/.test(spec)
}

function existsInTarget(p: InstalledPlugin): boolean {
  return props.existingIds.includes(p.id)
}

function toggleSelected(id: string, value: boolean | (string | number | boolean)[]) {
  const idx = selected.value.indexOf(id)
  if (value === true && idx === -1) selected.value.push(id)
  if (value !== true && idx !== -1) selected.value.splice(idx, 1)
}

const migratable = computed(() => plugins.value.filter((p) => !isUnmigratable(p)))

const installCount = computed(
  () =>
    migratable.value.filter(
      (p) =>
        selected.value.includes(p.id) && !(conflictStrategy.value === 'skip' && existsInTarget(p)),
    ).length,
)

const canConfirm = computed(() => selected.value.length > 0 && !busy.value && !loading.value)

watch(
  () => props.visible,
  (v) => {
    if (!v) return
    sourceInstanceId.value = undefined
    sourceProfile.value = undefined
    sourceProfiles.value = []
    plugins.value = []
    selected.value = []
    versionStrategy.value = 'source'
    conflictStrategy.value = 'skip'
    syncDisabled.value = true
    // Prefill with the first eligible source instance (usually the old version).
    if (sourceInstances.value.length > 0) {
      sourceInstanceId.value = sourceInstances.value[0].id
    }
  },
)

watch(sourceInstanceId, async (id) => {
  sourceProfile.value = undefined
  sourceProfiles.value = []
  plugins.value = []
  selected.value = []
  if (!id) return
  const inst = store.instanceById(id)
  if (!inst) return
  try {
    sourceProfiles.value = await api.listProfiles(inst.home_id)
    // Preselect the instance default profile, else the first one.
    if (inst.default_profile && sourceProfiles.value.includes(inst.default_profile)) {
      sourceProfile.value = inst.default_profile
    } else if (sourceProfiles.value.length > 0) {
      sourceProfile.value = sourceProfiles.value[0]
    }
  } catch (e) {
    Message.error(String(e))
  }
})

async function loadPlugins() {
  if (!sourceInstanceId.value || !sourceProfile.value) return
  loading.value = true
  try {
    plugins.value = await api.listInstalledPlugins(sourceInstanceId.value, sourceProfile.value)
    selected.value = migratable.value.map((p) => p.id)
  } catch (e) {
    Message.error(String(e))
  } finally {
    loading.value = false
  }
}

function versionFor(p: InstalledPlugin): string {
  if (versionStrategy.value === 'latest') return 'latest'
  const spec = (p.version ?? '').trim()
  return spec && spec !== 'latest' ? spec : 'latest'
}

function close() {
  emit('update:visible', false)
}

async function confirm() {
  if (!canConfirm.value) return
  busy.value = true
  try {
    const chosen = migratable.value.filter((p) => selected.value.includes(p.id))
    const toInstall = chosen.filter(
      (p) => !(conflictStrategy.value === 'skip' && existsInTarget(p)),
    )
    let started = 0
    for (const p of toInstall) {
      // One task per plugin: they queue on the target profile's lock, the
      // task list shows per-plugin progress, and a failure does not abort
      // the rest (same pattern as batch update).
      await api.startInstallPluginTask({
        pluginId: p.id,
        version: versionFor(p),
        channel: 'stable',
        instanceId: props.targetInstanceId,
        profile: props.targetProfile,
      })
      started += 1
    }
    if (syncDisabled.value) {
      const disabledIds = chosen.filter((p) => !p.enabled).map((p) => p.id)
      if (disabledIds.length > 0) {
        await api.setPluginsEnabled({
          instanceId: props.targetInstanceId,
          profile: props.targetProfile,
          pluginIds: disabledIds,
          enabled: false,
        })
      }
    }
    if (started > 0) {
      await store.refreshTasks()
      store.notifyTaskQueued()
      Message.success(t('plugins.migrateStarted', { count: started }))
      Message.info(t('plugins.installRestartHint'))
    } else {
      Message.info(t('plugins.migrateNothing'))
    }
    close()
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <a-modal
    :visible="visible"
    :title="t('plugins.migrateTitle')"
    :ok-text="t('plugins.migrateConfirm', { count: installCount })"
    :cancel-text="t('instanceEdit.cancel')"
    :ok-button-props="{ disabled: !canConfirm, loading: busy }"
    width="640px"
    @ok="confirm"
    @cancel="close"
  >
    <a-form layout="vertical" :model="{}">
      <div class="migrate-source-row">
        <a-form-item :label="t('plugins.migrateSourceInstance')" class="migrate-source-col">
          <a-select v-model="sourceInstanceId" :placeholder="t('plugins.migrateSourceInstanceHint')">
            <a-option v-for="inst in sourceInstances" :key="inst.id" :value="inst.id">
              {{ inst.name }}（{{ store.versionById(inst.version_id)?.version ?? '?' }}）
            </a-option>
          </a-select>
        </a-form-item>
        <a-form-item :label="t('plugins.migrateSourceProfile')" class="migrate-source-col">
          <a-select
            v-model="sourceProfile"
            :disabled="!sourceInstanceId"
            :placeholder="t('plugins.migrateSourceProfileHint')"
          >
            <a-option v-for="p in sourceProfiles" :key="p" :value="p">{{ p }}</a-option>
          </a-select>
        </a-form-item>
        <a-form-item label=" ">
          <a-button :loading="loading" :disabled="!sourceProfile" @click="loadPlugins">
            {{ t('plugins.migrateLoad') }}
          </a-button>
        </a-form-item>
      </div>

      <div v-if="plugins.length > 0" class="migrate-list">
        <div v-for="p in plugins" :key="p.id" class="migrate-row">
          <a-checkbox
            :model-value="selected.includes(p.id)"
            :disabled="isUnmigratable(p)"
            @change="(v: boolean | (string | number | boolean)[]) => toggleSelected(p.id, v)"
          />
          <span class="migrate-id">{{ p.id }}</span>
          <a-tag v-if="p.version" size="small">{{ p.version }}</a-tag>
          <a-tag v-if="!p.enabled" color="gray" size="small">{{ t('plugins.migrateDisabledTag') }}</a-tag>
          <a-tag v-if="existsInTarget(p)" color="orange" size="small">
            {{ t('plugins.migrateExistsTag') }}
          </a-tag>
          <HintIcon v-if="isUnmigratable(p)" :content="t('plugins.migrateUnmigratableHint')" />
        </div>
      </div>
      <a-empty v-else-if="!loading" :description="t('plugins.migrateEmpty')" />

      <a-form-item>
        <template #label>
          {{ t('plugins.migrateVersionStrategy') }}
          <HintIcon :content="t('plugins.migrateVersionStrategyHint')" />
        </template>
        <a-radio-group v-model="versionStrategy" type="button">
          <a-radio value="source">{{ t('plugins.migrateKeepSource') }}</a-radio>
          <a-radio value="latest">{{ t('plugins.migrateUseLatest') }}</a-radio>
        </a-radio-group>
      </a-form-item>
      <a-form-item>
        <template #label>
          {{ t('plugins.migrateConflictStrategy') }}
          <HintIcon :content="t('plugins.migrateConflictStrategyHint')" />
        </template>
        <a-radio-group v-model="conflictStrategy" type="button">
          <a-radio value="skip">{{ t('plugins.migrateConflictSkip') }}</a-radio>
          <a-radio value="reinstall">{{ t('plugins.migrateConflictReinstall') }}</a-radio>
        </a-radio-group>
      </a-form-item>
      <a-form-item>
        <a-checkbox v-model="syncDisabled">
          {{ t('plugins.migrateSyncDisabled') }}
          <HintIcon :content="t('plugins.migrateSyncDisabledHint')" />
        </a-checkbox>
      </a-form-item>
    </a-form>
  </a-modal>
</template>

<style scoped>
.migrate-source-row {
  display: flex;
  gap: 12px;
  align-items: flex-end;
}

.migrate-source-col {
  flex: 1;
  min-width: 0;
}

.migrate-list {
  max-height: 260px;
  overflow-y: auto;
  border: 1px solid var(--color-border-2);
  border-radius: 6px;
  padding: 6px 10px;
  margin-bottom: 12px;
}

.migrate-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
}

.migrate-id {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
