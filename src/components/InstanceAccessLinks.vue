<script setup lang="ts">
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import type { InstanceStatus } from '@/api/types'

const { message: Message } = useFeedback()

const props = defineProps<{ status: InstanceStatus; centered?: boolean }>()
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
  <span v-if="status.url" class="access-links" :class="{ 'access-links-centered': centered }">
    <dsh-link class="access-url" href="#" :title="status.url" @click.prevent="openDesktop">{{ status.url }}</dsh-link>
    <span class="access-actions">
      <dsh-button variant="ghost" size="sm" @click="copyLink">{{ t('network.copy') }}</dsh-button>
      <dsh-button v-if="status.web_mode && status.web_mode !== 'local'" variant="ghost" size="sm" @click="openShare">
        {{ t('network.openShare') }}
      </dsh-button>
    </span>
  </span>
</template>

<style scoped>
.access-links {
  display: grid;
  gap: 4px;
}

.access-url {
  justify-self: start;
}

.access-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.access-links-centered {
  width: 100%;
  text-align: center;
}

.access-links-centered .access-url {
  max-width: 100%;
  justify-self: center;
  overflow-wrap: anywhere;
}

.access-links-centered .access-actions {
  justify-content: center;
}
</style>
