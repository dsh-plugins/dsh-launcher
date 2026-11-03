<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import type { DshInstance, WebAccess, WebInterface, WebMode } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import { validNetworkRanges } from '@/utils/network-ranges'
import InstanceAccessLinks from './InstanceAccessLinks.vue'

const props = defineProps<{ instance: DshInstance }>()
const store = useLauncherStore()
const { t } = useI18n()
const saved = props.instance.web_access
const mode = ref<WebMode>(saved?.mode ?? 'local')
const modes: WebMode[] = ['local', 'lan', 'direct', 'proxy']
const port = ref(props.instance.port?.toString() ?? '')
const address = ref(saved?.address ?? '')
const publicUrl = ref(saved?.public_url ?? '')
const networks = ref(saved?.networks.join('\n') ?? '')
const hosts = ref(saved?.trusted_hosts.join('\n') ?? '')
const tls = ref(!!saved?.tls)
const cert = ref(saved?.tls?.cert ?? '')
const key = ref(saved?.tls?.key ?? '')
const interfaces = ref<WebInterface[]>([])
const busy = ref(false)
const status = computed(() => store.statusOf(props.instance.id))
const needsAddress = computed(() => mode.value === 'lan' || mode.value === 'direct')
const plainHttp = computed(() => mode.value === 'proxy'
  ? /^http:\/\//i.test(publicUrl.value) || (!!address.value && !tls.value)
  : !tls.value && mode.value !== 'local')

async function loadInterfaces() {
  try {
    interfaces.value = await api.listWebInterfaces()
  } catch (error) {
    Message.error(String(error))
  }
}

onMounted(loadInterfaces)

function chooseAddress(value: string) {
  const previous = interfaces.value.find((item) => item.address === address.value)?.subnet
  address.value = value
  if (mode.value === 'lan' && (!networks.value.trim() || networks.value.trim() === previous)) {
    networks.value = interfaces.value.find((item) => item.address === value)?.subnet ?? ''
  }
}

function chooseMode(value: WebMode) {
  if (value === 'proxy' && mode.value !== 'proxy') { address.value = '' }
  mode.value = value
  if (value === 'proxy' && !port.value.trim()) { port.value = '3080' }
  if (value === 'lan' && !networks.value.trim()) { chooseAddress(address.value) }
}

async function pickFile(which: 'cert' | 'key') {
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const file = await open({ multiple: false, directory: false })
    if (file) { (which === 'cert' ? cert : key).value = file }
  } catch (error) {
    Message.error(String(error))
  }
}

function entries(text: string) {
  return text.split(/[\s,]+/u).filter(Boolean)
}

async function save(restart: boolean) {
  const text = port.value.trim()
  const number = text ? Number(text) : null
  if ((text && (!/^\d+$/.test(text) || !number || number > 65535)) || (!number && mode.value === 'proxy')) {
    Message.error(t('network.invalidPort'))
    return
  }
  if (needsAddress.value && !address.value) { Message.error(t('network.chooseInterface')); return }
  if (mode.value === 'lan' && !validNetworkRanges(entries(networks.value))) { Message.error(t('network.emptyNetworks')); return }
  if (tls.value && (!cert.value.trim() || !key.value.trim())) { Message.error(t('network.tlsPair')); return }
  const id = props.instance.id
  const profile = status.value.profile
  const access: WebAccess = {
    mode: mode.value,
    address: (needsAddress.value || mode.value === 'proxy') ? address.value || null : null,
    networks: mode.value === 'lan' ? entries(networks.value) : [],
    public_url: mode.value === 'proxy' ? publicUrl.value : null,
    trusted_hosts: mode.value === 'local' ? [] : entries(hosts.value),
    tls: tls.value ? { cert: cert.value.trim(), key: key.value.trim() } : null,
  }
  busy.value = true
  try {
    const updated = await api.setInstanceWebAccess(id, access, number)
    const instance = store.instanceById(id)
    if (instance) {
      instance.web_access = updated.web_access
      instance.port = updated.port
    }
    networks.value = updated.web_access?.networks.join('\n') ?? ''
    hosts.value = updated.web_access?.trusted_hosts.join('\n') ?? ''
    publicUrl.value = updated.web_access?.public_url ?? ''
    if (restart && profile) {
      await api.stopInstance(id)
      await api.startInstance(id, profile)
    }
    Message.success(t('network.saved'))
  } catch (error) {
    Message.error(String(error))
  } finally {
    busy.value = false
  }
}

async function openGuide() {
  try {
    await api.openExternal('https://deepseek-harness.github.io/deepseek-harness/guide/public-deployments')
  } catch (error) {
    Message.error(String(error))
  }
}
</script>

<template>
  <div class="dl-card edit-card">
    <h3>{{ t('network.title') }}</h3>
    <p>{{ t('network.description') }}</p>
    <a-space v-if="status.web_mode" direction="vertical" fill>
      <a-tag color="green">{{ t('network.active', { mode: t('network.modes.' + status.web_mode) }) }}</a-tag>
      <span v-if="status.web_mode === 'lan'">{{ t('network.activeNetworks', { networks: status.allowed_networks?.join(', ') }) }}</span>
      <InstanceAccessLinks :status="status" />
    </a-space>
    <a-form :model="{ mode, address, port, publicUrl, networks, hosts, tls, cert, key }" layout="vertical" class="network-form">
      <a-form-item :label="t('network.mode')">
        <a-select :model-value="mode" @change="(value) => chooseMode(value as WebMode)">
          <a-option v-for="item in modes" :key="item" :value="item">{{ t('network.modes.' + item) }}</a-option>
        </a-select>
      </a-form-item>
      <a-form-item v-if="needsAddress || mode === 'proxy'" :label="t('network.interface')" :extra="mode === 'proxy' ? t('network.proxyAddress') : undefined">
        <a-space fill>
          <a-select :model-value="address || undefined" :allow-clear="mode === 'proxy'" :placeholder="mode === 'proxy' ? '127.0.0.1' : t('network.chooseInterface')" @change="(value) => chooseAddress(typeof value === 'string' ? value : '')">
            <a-option v-for="item in interfaces" :key="item.name + item.address" :value="item.address">
              {{ item.name }} · {{ item.address }}
            </a-option>
          </a-select>
          <a-button @click="loadInterfaces">{{ t('network.refresh') }}</a-button>
        </a-space>
      </a-form-item>
      <a-form-item :label="t('instanceEdit.port')" :extra="mode === 'proxy' ? t('network.fixedPort') : t('instanceEdit.portPlaceholder')">
        <a-input v-model="port" :placeholder="mode === 'proxy' ? '3080' : t('instanceEdit.portPlaceholder')" allow-clear />
      </a-form-item>
      <a-form-item v-if="mode === 'lan'" :label="t('network.networks')" :extra="t('network.networksHint')">
        <a-textarea v-model="networks" placeholder="192.168.1.0/24&#10;fd00::/64" />
      </a-form-item>
      <template v-if="mode === 'proxy'">
        <a-form-item :label="t('network.publicUrl')"><a-input v-model="publicUrl" placeholder="https://dsh.example/ui/" /></a-form-item>
        <a-alert>
          {{ t('network.proxyGuide') }}
          <a-link @click="openGuide">{{ t('network.guide') }}</a-link>
        </a-alert>
      </template>
      <a-form-item v-if="mode !== 'local'" :label="t('network.hosts')" :extra="t('network.hostsHint')">
        <a-textarea v-model="hosts" placeholder="dsh.example:8443" />
      </a-form-item>
      <a-form-item :label="t('network.tls')" :extra="t('network.tlsHint')"><a-switch v-model="tls" /></a-form-item>
      <template v-if="tls">
        <a-form-item :label="t('network.cert')"><a-input-group><a-input v-model="cert" /><a-button @click="pickFile('cert')">{{ t('network.browse') }}</a-button></a-input-group></a-form-item>
        <a-form-item :label="t('network.key')"><a-input-group><a-input v-model="key" /><a-button @click="pickFile('key')">{{ t('network.browse') }}</a-button></a-input-group></a-form-item>
      </template>
      <a-alert v-if="plainHttp" type="warning">{{ t('network.plainHttp') }}</a-alert>
      <a-space class="network-actions">
        <a-button type="primary" :loading="busy" @click="save(false)">{{ t('common.save') }}</a-button>
        <a-button v-if="status.state === 'running' && status.web_mode" :disabled="busy" @click="save(true)">{{ t('network.restart') }}</a-button>
      </a-space>
    </a-form>
  </div>
</template>

<style scoped>
.network-form { max-width: 620px; margin-top: 20px; }
.network-form .arco-alert { margin-bottom: 16px; }
.network-form .arco-input-group { display: flex; width: 100%; }
.network-actions { margin-top: 8px; }
</style>
