<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { ModpackManifest } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'

const { message: Message } = useFeedback()

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
// Issue #49 G5: the dshhome form can target a WSL distro, so its profile
// dependencies are installed inside the distro (Linux native binaries).
const wslDistro = ref<string | undefined>(undefined)

/** manifest v5 dshhome: a whole-DSH_HOME snapshot — always a fresh instance,
 * profiles come from the manifest (no profileName/force overrides). */
const isDshhome = computed(() => manifest.value?.type === 'dshhome')

/** Distros available for a dshhome import (only that form supports WSL). */
const distros = ref<string[]>([])

// WSL is optional (and absent on many machines): a failure just leaves the
// picker empty, the local-Windows path stays available.
onMounted(async () => {
  try {
    distros.value = await api.listWslDistros()
  } catch {
    distros.value = []
  }
})

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
    wslDistro.value = undefined
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
    wslDistro.value = undefined
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
      instance_name: instanceName.value.trim() || undefined,
      profile_name: isDshhome.value ? undefined : profileName.value.trim() || undefined,
      existing_instance_id:
        !isDshhome.value && importMode.value === 'existing' ? existingInstanceId.value : undefined,
      // Only the dshhome form honours this (single-profile packs target an
      // existing instance, whose HOME already fixes the distro).
      wsl_distro: isDshhome.value ? wslDistro.value : undefined,
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
  <dsh-modal
    :visible="visible"
    :title="t('modpack.importTitle')"
    :ok-loading="busy"
    :ok-button-props="{ disabled: !canConfirm }"
    @ok="confirm"
    @cancel="close"
  >
    <dsh-form :model="{ source, instanceName, profileName }" layout="vertical">
      <dsh-form-item :label="t('modpack.source')" required>
        <dsh-input
          v-model="source"
          :placeholder="t('modpack.sourceHint')"
          allow-clear
          @press-enter="loadManifest"
        >
          <template #append>
            <dsh-button variant="secondary" @click="pickFile">{{ t('modpack.pickFile') }}</dsh-button>
          </template>
        </dsh-input>
      </dsh-form-item>
      <dsh-form-item>
        <dsh-button variant="secondary" size="sm" :loading="loading" :disabled="!source.trim()" @click="loadManifest">
          {{ t('modpack.load') }}
        </dsh-button>
      </dsh-form-item>

      <template v-if="manifest">
        <dsh-alert type="info" class="modpack-summary">
          {{ manifest.name }} v{{ manifest.version }}
          <template v-if="manifest.author"> · {{ manifest.author }}</template>
          <template v-if="manifest.dshVersion"> · DSH {{ manifest.dshVersion }}</template>
          <template v-if="manifest.files?.length">
            · {{ t('modpack.filesCount', { count: manifest.files.length }) }}
          </template>
        </dsh-alert>
        <dsh-alert v-if="isDshhome" type="warning" class="modpack-summary">
          {{
            t('modpack.dshhomeHint', {
              count: Object.keys(manifest.profiles ?? {}).length,
              default: manifest.defaultProfile,
            })
          }}
        </dsh-alert>
        <dsh-form-item v-if="isDshhome && distros.length > 0" :label="t('modpack.wslTarget')">
          <dsh-select v-model="wslDistro" allow-clear :placeholder="t('modpack.wslTargetHint')">
            <dsh-option v-for="d in distros" :key="d" :value="d">{{ d }}</dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-alert v-if="isDshhome && wslDistro" type="info" class="modpack-summary">
          {{ t('modpack.wslInstallHint', { distro: wslDistro }) }}
        </dsh-alert>
        <dsh-form-item v-if="!isDshhome" :label="t('modpack.target')">
          <dsh-radio-group v-model="importMode" type="button">
            <dsh-radio value="new">{{ t('modpack.targetNew') }}</dsh-radio>
            <dsh-radio value="existing" :disabled="eligibleInstances.length === 0">
              {{ t('modpack.targetExisting') }}
            </dsh-radio>
          </dsh-radio-group>
        </dsh-form-item>
        <dsh-form-item v-if="importMode === 'existing'" :label="t('modpack.existingInstance')" required>
          <dsh-select v-model="existingInstanceId" :placeholder="t('modpack.existingInstanceHint')">
            <dsh-option v-for="inst in eligibleInstances" :key="inst.id" :value="inst.id">
              {{ inst.name }}（{{ store.versionById(inst.version_id)?.version }}）
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item v-else :label="t('modpack.instanceName')" required>
          <dsh-input v-model="instanceName" />
        </dsh-form-item>
        <dsh-form-item v-if="!isDshhome" :label="t('modpack.profileName')">
          <dsh-input v-model="profileName" :placeholder="'pack'" />
        </dsh-form-item>
        <dsh-form-item v-if="!isDshhome">
          <dsh-checkbox v-model="force">{{ t('modpack.force') }}</dsh-checkbox>
        </dsh-form-item>
      </template>
    </dsh-form>
  </dsh-modal>
</template>

<style scoped>
.modpack-summary {
  margin-bottom: 12px;
}
</style>
