<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { ProxyEndpoint, ProxyPasswordScope } from '@/api/types'

const props = defineProps<{ scope: ProxyPasswordScope; instanceId?: string; disabled?: boolean }>()
const endpoint = defineModel<ProxyEndpoint>({ required: true })
const password = defineModel<string | undefined>('password')
const hasPassword = ref<boolean>()
const { t } = useI18n()
const { message } = useFeedback()
const passwordStatus = computed(() => {
  if (password.value === '') { return t('browser.passwordClearPending') }
  if (hasPassword.value === undefined) { return t('browser.passwordUnknown') }
  return t(hasPassword.value ? 'browser.passwordStored' : 'browser.passwordEmpty')
})

onMounted(async () => {
  try {
    hasPassword.value = await api.getProxyPasswordStatus(props.scope, props.instanceId)
  } catch (error) {
    message.error(String(error))
  }
})
</script>

<template>
  <dsh-form-item :label="t('settings.proxy.url')" :extra="t('browser.endpointHint')">
    <dsh-input v-model="endpoint.url" placeholder="http://127.0.0.1" :disabled="disabled" />
  </dsh-form-item>
  <dsh-form-item :label="t('settings.proxy.port')">
    <dsh-input-number v-model="endpoint.port" :min="1" :max="65535" :precision="0" :disabled="disabled" />
  </dsh-form-item>
  <dsh-form-item :label="t('settings.proxy.noProxy')" :extra="t('settings.proxy.noProxyHint')">
    <dsh-input v-model="endpoint.no_proxy" placeholder="127.0.0.1,localhost,::1" :disabled="disabled" />
  </dsh-form-item>
  <dsh-form-item :label="t('browser.username')" :extra="t('browser.usernameHint')">
    <dsh-input v-model="endpoint.username" autocomplete="off" :disabled="disabled" />
  </dsh-form-item>
  <dsh-form-item :label="t('browser.password')" :extra="passwordStatus">
    <dsh-input-group class="proxy-password">
      <dsh-input-password :model-value="password ?? ''" autocomplete="new-password" :disabled="disabled"
        @update:model-value="(value: string) => password = value || undefined" />
      <dsh-button v-if="password === ''" variant="secondary" :disabled="disabled" @click="password = undefined">
        {{ t('browser.keepPassword') }}
      </dsh-button>
      <dsh-button v-else variant="secondary" :disabled="disabled || hasPassword === false" @click="password = ''">
        {{ t('browser.clearPassword') }}
      </dsh-button>
    </dsh-input-group>
  </dsh-form-item>
</template>

<style scoped>
.proxy-password { display: flex; width: 100%; }
.proxy-password > :first-child { min-width: 0; flex: 1; }
</style>
