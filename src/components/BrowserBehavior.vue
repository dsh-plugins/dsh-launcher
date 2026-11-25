<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { defaultBrowserSettings } from '@/api/types'
import type { BrowserOverrides, BrowserProxyMode, BrowserStorage, DshInstance, ProxyEndpoint } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import { validProxyEndpoint } from '@/utils/proxy-endpoint'
import ProxyFields from './ProxyFields.vue'

const props = defineProps<{ instance?: DshInstance }>()
const store = useLauncherStore()
const { t } = useI18n()
const { message } = useFeedback()
const globalSettings = computed(() => store.settings.browser ?? defaultBrowserSettings())
const storage = ref<BrowserStorage | 'inherit'>('isolated')
const uaMode = ref<'inherit' | 'platform' | 'custom'>('platform')
const userAgent = ref('')
const proxyMode = ref<BrowserProxyMode | 'inherit'>('launcher')
const proxy = ref<ProxyEndpoint>(defaultBrowserSettings().proxy)
const password = ref<string>()
const revision = ref(0)
const busy = ref(false)
const scopes: BrowserStorage[] = ['isolated', 'session', 'shared']
const modes: BrowserProxyMode[] = ['direct', 'launcher', 'custom']

watch(() => props.instance ? props.instance.browser : globalSettings.value, (value) => {
  storage.value = value?.storage ?? 'inherit'
  userAgent.value = value?.user_agent ?? ''
  uaMode.value = value?.user_agent == null ? 'inherit' : value.user_agent ? 'custom' : 'platform'
  proxyMode.value = value?.proxy?.mode ?? 'inherit'
  proxy.value = { ...(value?.proxy ?? globalSettings.value.proxy) }
  password.value = undefined
}, { immediate: true })

const effectiveStorage = computed(() => storage.value === 'inherit' ? globalSettings.value.storage : storage.value)
const shared = computed(() => !!props.instance && effectiveStorage.value === 'shared')
const usesGlobalProxy = computed(() => shared.value || proxyMode.value === 'inherit')
const displayedMode = computed(() => shared.value ? globalSettings.value.proxy.mode : proxyMode.value)

const proxyDescription = computed(() => {
  const selected = usesGlobalProxy.value ? globalSettings.value.proxy : { ...proxy.value, mode: proxyMode.value }
  let url = selected.url
  let port = selected.port
  if (selected.mode === 'direct' || (selected.mode === 'launcher' && !store.settings.proxy_enabled)) {
    return t('browser.directDescription')
  }
  if (selected.mode === 'launcher') {
    url = store.settings.proxy_url
    port = store.settings.proxy_port
  }
  try {
    const endpoint = new URL(url)
    return t('browser.activeEndpoint', { address: endpoint.protocol + '//' + endpoint.hostname + ':' + port })
  } catch (error) {
    if (error instanceof TypeError) { return t('browser.invalidEndpoint') }
    throw error
  }
})

async function save() {
  if (uaMode.value === 'custom' && (!userAgent.value.trim() || /[\r\n]/u.test(userAgent.value))) {
    message.error(t('browser.invalidUa'))
    return
  }
  if (!shared.value && proxyMode.value === 'custom' && !validProxyEndpoint(proxy.value)) {
    message.error(t('browser.invalidEndpoint'))
    return
  }
  busy.value = true
  try {
    const browser: BrowserOverrides = {
      storage: storage.value === 'inherit' ? null : storage.value,
      user_agent: uaMode.value === 'inherit' ? null : uaMode.value === 'platform' ? '' : userAgent.value.trim(),
      proxy: null,
    }
    if (shared.value) {
      browser.proxy = props.instance?.browser?.proxy ?? null
    } else if (proxyMode.value !== 'inherit') {
      const endpoint = proxyMode.value === 'custom' ? proxy.value : (props.instance?.browser?.proxy ?? globalSettings.value.proxy)
      browser.proxy = { ...endpoint, mode: proxyMode.value, url: endpoint.url.trim(), no_proxy: endpoint.no_proxy.trim() }
    }
    const editPassword = !shared.value && proxyMode.value === 'custom'
    await api.saveBrowserSettings(props.instance?.id ?? null, browser, editPassword ? password.value : undefined)
    password.value = undefined
    revision.value++
    if (props.instance) {
      await store.refreshInstances()
    } else {
      await store.refreshSettings()
    }
    message.success(t('settings.saved'))
  } catch (error) {
    message.error(String(error))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="dl-card edit-card">
    <div class="dl-card-title"><h3>{{ t('browser.title') }}</h3></div>
    <p>{{ t('browser.scope') }}</p>
    <dsh-form layout="vertical" :model="{ storage, userAgent, proxyMode }" class="browser-form">
      <dsh-form-item :label="t('browser.storage')" :extra="t('browser.storageHints.' + effectiveStorage)">
        <dsh-select v-model="storage" :disabled="busy">
          <dsh-option v-if="instance" value="inherit">{{ t('browser.inheritValue', { value: t('browser.storageModes.' + globalSettings.storage) }) }}</dsh-option>
          <dsh-option v-for="mode in scopes" :key="mode" :value="mode">{{ t('browser.storageModes.' + mode) }}</dsh-option>
        </dsh-select>
      </dsh-form-item>
      <dsh-form-item :label="t('browser.ua')" :extra="uaMode === 'inherit' ? globalSettings.user_agent || t('browser.platformUa') : undefined">
        <dsh-select v-model="uaMode" :disabled="busy">
          <dsh-option v-if="instance" value="inherit">{{ t('browser.inherit') }}</dsh-option>
          <dsh-option value="platform">{{ t('browser.platformUa') }}</dsh-option>
          <dsh-option value="custom">{{ t('browser.customUa') }}</dsh-option>
        </dsh-select>
      </dsh-form-item>
      <dsh-form-item v-if="uaMode === 'custom'" :label="t('browser.uaValue')">
        <dsh-input v-model="userAgent" :disabled="busy" placeholder="Mozilla/5.0 …" />
      </dsh-form-item>
      <dsh-form-item :label="t('browser.proxy')" :extra="shared ? t('browser.sharedProxy', { value: proxyDescription }) : proxyMode === 'custom' ? undefined : proxyDescription">
        <dsh-select :model-value="displayedMode" :disabled="shared || busy" @change="value => proxyMode = value as typeof proxyMode">
          <dsh-option v-if="instance" value="inherit">{{ t('browser.inherit') }}</dsh-option>
          <dsh-option v-for="mode in modes" :key="mode" :value="mode">{{ t('browser.proxyModes.' + mode) }}</dsh-option>
        </dsh-select>
      </dsh-form-item>
      <ProxyFields v-if="!shared && proxyMode === 'custom'" :key="revision" v-model="proxy" v-model:password="password"
        :scope="instance ? 'instance' : 'browser'" :instance-id="instance?.id" :disabled="busy" />
      <p class="browser-hint">{{ t('browser.applyProxy') }}</p>
      <dsh-button variant="primary" :loading="busy" @click="save">{{ t('common.save') }}</dsh-button>
    </dsh-form>
  </div>
</template>

<style scoped>
.browser-form { max-width: 620px; margin-top: 16px; }
.browser-hint { color: var(--color-text-2); margin: 8px 0 16px; }
</style>
