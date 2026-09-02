<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'

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
  <a-modal
    :visible="visible"
    :title="t('pluginFile.importTitle')"
    :ok-loading="busy"
    :ok-button-props="{ disabled: !canConfirm }"
    @ok="confirm"
    @cancel="close"
  >
    <a-form :model="{ instanceId, profile }" layout="vertical">
      <a-form-item :label="t('pluginFile.file')">
        <a-alert type="info">{{ fileName }}</a-alert>
      </a-form-item>
      <a-form-item :label="t('pluginFile.instance')" required>
        <a-select v-model="instanceId" :placeholder="t('pluginFile.instanceHint')">
          <a-option v-for="inst in store.instances" :key="inst.id" :value="inst.id">
            {{ inst.name }}（{{ store.versionById(inst.version_id)?.version }}）
          </a-option>
        </a-select>
      </a-form-item>
      <a-form-item :label="t('pluginFile.profile')" required>
        <a-input v-model="profile" placeholder="web" />
      </a-form-item>
    </a-form>
  </a-modal>
</template>
