<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { ModpackManifest } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'

const props = defineProps<{
  visible: boolean
  /** Optional prefill (drag-drop path or deep-link URL); auto-loads the manifest. */
  initialSource?: string
}>()
const emit = defineEmits<{ 'update:visible': [boolean] }>()

const router = useRouter()
const { t, locale } = useI18n()
const store = useLauncherStore()

const source = ref('')
const loading = ref(false)
const manifest = ref<ModpackManifest | null>(null)
const instanceName = ref('')
const profileName = ref('')
const force = ref(false)
const busy = ref(false)
// Issue #11: import into an existing instance on the same version line.
const importMode = ref<'new' | 'existing'>('new')
const existingInstanceId = ref<string | undefined>(undefined)

/** Instances whose DSH version shares the manifest's version line. */
const eligibleInstances = computed(() => {
  const want = manifest.value?.dshVersion?.trim().replace(/^[>=^~\s]+/, '')
  return store.instances.filter((inst) => {
    if (!want) return true
    const have = store.versionById(inst.version_id)?.version
    if (!have) return false
    return have.split('-')[0] === want.split('-')[0]
  })
})

const canConfirm = computed(() => {
  if (!manifest.value || busy.value) return false
  if (importMode.value === 'existing') return !!existingInstanceId.value
  return instanceName.value.trim().length > 0
})

watch(
  () => props.visible,
  async (v) => {
    if (!v) return
    manifest.value = null
    importMode.value = 'new'
    existingInstanceId.value = undefined
    source.value = props.initialSource ?? ''
    force.value = false
    if (source.value) await loadManifest()
  },
)

/** Localized display name: string passthrough, or locale map with fallback. */
function localizedDisplayName(m: ModpackManifest): string | null {
  const d = m.displayName
  if (!d) return null
  if (typeof d === 'string') return d
  const map = d as Record<string, string>
  return map[locale.value] ?? map['en-US'] ?? Object.values(map)[0] ?? null
}

async function loadManifest() {
  if (!source.value.trim()) return
  loading.value = true
  manifest.value = null
  try {
    const m = await api.readModpackManifest(source.value.trim())
    manifest.value = m
    instanceName.value = localizedDisplayName(m) ?? m.name
    profileName.value = m.profileName?.trim() || 'pack'
    existingInstanceId.value = undefined
    importMode.value = 'new'
  } catch (e) {
    Message.error(String(e))
  } finally {
    loading.value = false
  }
}

async function pickFile() {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'DSH Modpack', extensions: ['dspack', 'tgz'] }],
  })
  if (typeof file === 'string') {
    source.value = file
    await loadManifest()
  }
}

async function confirm() {
  if (!canConfirm.value) return
  busy.value = true
  try {
    await api.startImportModpackTask({
      source: source.value.trim(),
      force: force.value,
      instance_name:
        importMode.value === 'new' ? instanceName.value.trim() : undefined,
      profile_name: profileName.value.trim() || undefined,
      existing_instance_id:
        importMode.value === 'existing' ? existingInstanceId.value : undefined,
    })
    emit('update:visible', false)
    await store.refreshTasks()
    Message.success(t('download.taskAdded'))
    router.push({ name: 'tasks' })
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = false
  }
}

function close() {
  emit('update:visible', false)
}
</script>

<template>
  <a-modal
    :visible="visible"
    :title="t('modpack.importTitle')"
    :ok-loading="busy"
    :ok-button-props="{ disabled: !canConfirm }"
    @ok="confirm"
    @cancel="close"
  >
    <a-form :model="{ source, instanceName, profileName }" layout="vertical">
      <a-form-item :label="t('modpack.source')" required>
        <a-input
          v-model="source"
          :placeholder="t('modpack.sourceHint')"
          allow-clear
          @press-enter="loadManifest"
        >
          <template #append>
            <a-button @click="pickFile">{{ t('modpack.pickFile') }}</a-button>
          </template>
        </a-input>
      </a-form-item>
      <a-form-item>
        <a-button size="small" :loading="loading" :disabled="!source.trim()" @click="loadManifest">
          {{ t('modpack.load') }}
        </a-button>
      </a-form-item>

      <template v-if="manifest">
        <a-alert type="info" class="modpack-summary">
          {{ manifest.name }} v{{ manifest.version }}
          <template v-if="manifest.author"> · {{ manifest.author }}</template>
          <template v-if="manifest.dshVersion"> · DSH {{ manifest.dshVersion }}</template>
          <template v-if="manifest.files?.length">
            · {{ t('modpack.filesCount', { count: manifest.files.length }) }}
          </template>
        </a-alert>
        <a-form-item :label="t('modpack.target')">
          <a-radio-group v-model="importMode" type="button">
            <a-radio value="new">{{ t('modpack.targetNew') }}</a-radio>
            <a-radio value="existing" :disabled="eligibleInstances.length === 0">
              {{ t('modpack.targetExisting') }}
            </a-radio>
          </a-radio-group>
        </a-form-item>
        <a-form-item v-if="importMode === 'existing'" :label="t('modpack.existingInstance')" required>
          <a-select v-model="existingInstanceId" :placeholder="t('modpack.existingInstanceHint')">
            <a-option v-for="inst in eligibleInstances" :key="inst.id" :value="inst.id">
              {{ inst.name }}（{{ store.versionById(inst.version_id)?.version }}）
            </a-option>
          </a-select>
        </a-form-item>
        <a-form-item v-else :label="t('modpack.instanceName')" required>
          <a-input v-model="instanceName" />
        </a-form-item>
        <a-form-item :label="t('modpack.profileName')">
          <a-input v-model="profileName" :placeholder="'pack'" />
        </a-form-item>
        <a-form-item>
          <a-checkbox v-model="force">{{ t('modpack.force') }}</a-checkbox>
        </a-form-item>
      </template>
    </a-form>
  </a-modal>
</template>

<style scoped>
.modpack-summary {
  margin-bottom: 12px;
}
</style>
