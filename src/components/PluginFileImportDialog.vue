<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'

const { message: Message } = useFeedback()

const props = defineProps<{
  visible: boolean
  /** Local .tgz path (drag-drop or file picker). */
  filePath?: string
  /** Prefilled instance (e.g. the instance page the file was dropped on). */
  initialInstanceId?: string
}>()
const emit = defineEmits<{ 'update:visible': [boolean] }>()

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const instanceId = ref<string | undefined>(undefined)
const profile = ref('web')
const busy = ref(false)

/** Tarball basename for display. */
const fileName = computed(() => {
  const p = props.filePath ?? ''
  return p.split(/[\\/]/).pop() || p
})

const canConfirm = computed(() => !!instanceId.value && !!profile.value.trim() && !busy.value)

watch(
  () => props.visible,
  (v) => {
    if (!v) return
    instanceId.value =
      props.initialInstanceId && store.instances.some((i) => i.id === props.initialInstanceId)
        ? props.initialInstanceId
        : store.instances[0]?.id
    profile.value = 'web'
  },
)

async function confirm() {
  if (!canConfirm.value || !props.filePath) return
  busy.value = true
  try {
    await api.startInstallPluginFileTask(instanceId.value!, profile.value.trim(), props.filePath)
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
    :title="t('pluginFile.importTitle')"
    :ok-loading="busy"
    :ok-button-props="{ disabled: !canConfirm }"
    @ok="confirm"
    @cancel="close"
  >
    <dsh-form :model="{ instanceId, profile }" layout="vertical">
      <dsh-form-item :label="t('pluginFile.file')">
        <dsh-alert type="info">{{ fileName }}</dsh-alert>
      </dsh-form-item>
      <dsh-form-item :label="t('pluginFile.instance')" required>
        <dsh-select v-model="instanceId" :placeholder="t('pluginFile.instanceHint')">
          <dsh-option v-for="inst in store.instances" :key="inst.id" :value="inst.id">
            {{ inst.name }}（{{ store.versionById(inst.version_id)?.version }}）
          </dsh-option>
        </dsh-select>
      </dsh-form-item>
      <dsh-form-item :label="t('pluginFile.profile')" required>
        <dsh-input v-model="profile" placeholder="web" />
      </dsh-form-item>
    </dsh-form>
  </dsh-modal>
</template>
