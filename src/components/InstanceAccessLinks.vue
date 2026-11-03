<script setup lang="ts">
import { Message } from '@arco-design/web-vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import type { InstanceStatus } from '@/api/types'

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
    <a-link :title="status.url" @click="openDesktop">{{ status.url }}</a-link>
    <a-button size="mini" type="text" @click="copyLink">{{ t('network.copy') }}</a-button>
    <a-button v-if="status.web_mode && status.web_mode !== 'local'" size="mini" type="text" @click="openShare">
      {{ t('network.openShare') }}
    </a-button>
  </span>
</template>

<style scoped>
.access-links { display: flex; align-items: center; flex-wrap: wrap; gap: 4px; min-width: 0; }
.access-links .arco-link { overflow-wrap: anywhere; min-width: 0; }
</style>
