<script setup lang="ts">
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { HintIcon, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import { validProxyEndpoint } from '@/utils/proxy-endpoint'
import ProxyFields from './ProxyFields.vue'

const store = useLauncherStore()
const { t } = useI18n()
const { message } = useFeedback()
const enabled = ref(false)
const applyDsh = ref(false)
const endpoint = ref({ url: '', port: 7890, no_proxy: '', username: '' })
const password = ref<string>()
const busy = ref(false)
const revision = ref(0)

watch(() => store.settings, (settings) => {
  enabled.value = settings.proxy_enabled
  applyDsh.value = settings.proxy_apply_dsh
  endpoint.value = { url: settings.proxy_url, port: settings.proxy_port, no_proxy: settings.no_proxy, username: settings.proxy_username ?? '' }
}, { immediate: true })

async function save() {
  if (enabled.value && !validProxyEndpoint(endpoint.value)) {
    message.error(t('browser.invalidEndpoint'))
    return
  }
  busy.value = true
  try {
    const { url, port, no_proxy, username } = endpoint.value
    store.settings = await api.updateSettings({
      proxy_enabled: enabled.value, proxy_apply_dsh: applyDsh.value, proxy_url: url.trim(), proxy_port: port,
      no_proxy: no_proxy.trim(), proxy_username: username, proxy_password: password.value,
    })
    password.value = undefined
    revision.value++
    message.success(t('settings.saved'))
  } catch (error) {
    message.error(String(error))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="dl-card">
    <div class="dl-card-title"><h3>{{ t('settings.proxy.title') }}</h3></div>
    <dsh-form layout="vertical" :model="endpoint" class="proxy-form">
      <dsh-form-item>
        <template #label>
          {{ t('settings.proxy.enabled') }}
          <HintIcon :content="t('settings.proxy.enabledHint')" />
        </template>
        <dsh-switch v-model="enabled" :disabled="busy" />
      </dsh-form-item>
      <ProxyFields :key="revision" v-model="endpoint" v-model:password="password" scope="launcher" :disabled="!enabled || busy" />
      <dsh-form-item>
        <template #label>
          {{ t('settings.proxy.applyDsh') }}
          <HintIcon :content="t('settings.proxy.applyDshHint')" />
        </template>
        <dsh-switch v-model="applyDsh" :disabled="!enabled || busy" />
      </dsh-form-item>
      <dsh-button variant="primary" :loading="busy" @click="save">{{ t('common.save') }}</dsh-button>
    </dsh-form>
  </div>
</template>

<style scoped>
.proxy-form { max-width: 620px; }
</style>
