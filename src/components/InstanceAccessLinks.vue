<script setup lang="ts">
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import type { InstanceStatus } from '@/api/types'

const { message: Message } = useFeedback()

const props = defineProps<{ status: InstanceStatus }>()
const emit = defineEmits<{ opened: [id: string] }>()
const { t } = useI18n()

async function openDesktop() {
  try {
    await api.openInstanceWindow(props.status.id)
    emit('opened', props.status.id)
  } catch (error) {
    Message.error(String(error))
  }
}

async function copyLink() {
  try {
    const url = await api.getInstanceShareUrl(props.status.id)
    await navigator.clipboard.writeText(url)
    Message.success(t('common.copied'))
  } catch (error) {
    Message.error(String(error))
  }
}

async function openShare() {
  try {
    await api.openInstanceShareUrl(props.status.id)
  } catch (error) {
    Message.error(String(error))
  }
}
</script>

<template>
  <span v-if="status.url" class="access-links">
    <dsh-link href="#" :title="status.url" @click.prevent="openDesktop">{{ status.url }}</dsh-link>
    <dsh-button variant="ghost" size="sm" @click="copyLink">{{ t('network.copy') }}</dsh-button>
    <dsh-button variant="ghost" v-if="status.web_mode && status.web_mode !== 'local'" size="sm" @click="openShare">
      {{ t('network.openShare') }}
    </dsh-button>
  </span>
</template>

<style scoped>
.access-links { display: flex; align-items: center; flex-wrap: wrap; gap: 4px; min-width: 0; }
.access-links .dsh-link { overflow-wrap: anywhere; min-width: 0; }
</style>
