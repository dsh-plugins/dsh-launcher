<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type {
  DshInstance, MarketModpack, PackforgeArchive, PackforgeInstallation, PackforgeInstallDecision,
  PackforgeInstallPlan, PackforgeInstallResource, PackforgeInstallTarget, PackforgeResourceList,
} from '@/api/types'
import PackforgePluginList from '@/components/PackforgePluginList.vue'
import { useLauncherStore } from '@/stores/launcher'
import { packforgeText } from '@/utils/packforge'

const props = defineProps<{
  visible: boolean
  initialSource?: string
  marketPack?: MarketModpack
  updateInstance?: DshInstance
}>()
const emit = defineEmits<{ 'update:visible': [boolean] }>()
const { message } = useFeedback()
const { t, locale, te } = useI18n()
const router = useRouter()
const store = useLauncherStore()

const source = ref('')
const market = ref<MarketModpack>()
const marketId = ref<string>()
const marketError = ref('')
const loading = ref(false)
const busy = ref(false)
const archive = ref<PackforgeArchive>()
const instanceName = ref('')
const profileName = ref('')
const force = ref(false)
const importMode = ref<'new' | 'existing'>('new')
const existingInstanceId = ref<string>()
const wslDistro = ref<string>()
const distros = ref<string[]>([])

const step = ref<'source' | 'resources' | 'plan'>('source')
const installations = ref<PackforgeInstallation[]>([])
const installationsLoading = ref(false)
const installationsError = ref('')
const binding = ref<number>()
const legacyProfile = ref('')
const resources = ref<PackforgeResourceList>()
const plan = ref<PackforgeInstallPlan>()
const decisions = ref<Record<string, PackforgeInstallDecision | 'auto'>>(Object.create(null))
const credentials = ref<Record<string, Record<string, string>>>(Object.create(null))
const reuse = ref<Record<string, boolean>>(Object.create(null))
const resolutions = ref<Record<string, boolean>>(Object.create(null))
const switchVersion = ref(false)
const previousSource = ref('')
const previousSha256 = ref('')
let request = 0
let installationsRequest = 0
let marketRequest = 0

const manifest = computed(() => archive.value?.manifest)
const isV6 = computed(() => manifest.value?.manifestVersion === 6)
const isDshhome = computed(() => manifest.value?.type === 'dshhome')
const selectedInstallation = computed(() => {
  return binding.value === undefined ? undefined : installations.value[binding.value]
})
const marketChoices = computed(() => {
  const installed = selectedInstallation.value
  if (!installed) {
    return []
  }
  return store.marketModpacks.filter(pack => pack.id === installed.market_id)
})
const eligibleInstances = computed(() => {
  const preferred = manifest.value?.dshVersion?.trim().replace(/^[>=^~\s]+/, '')
  const versions = manifest.value?.dshVersions ?? (preferred ? [preferred] : [])
  return store.instances.filter(inst => {
    if (!versions.length) {
      return true
    }
    const installed = store.versionById(inst.version_id)?.version
    if (!installed) {
      return false
    }
    return isV6.value ? versions.includes(installed) : installed.split('-')[0] === preferred?.split('-')[0]
  })
})
const allowWsl = computed(() => !props.updateInstance && importMode.value === 'new' && (isV6.value || isDshhome.value))
const missingPreviousHash = computed(() => !!previousSource.value.trim() && !/^[a-f\d]{64}$/i.test(previousSha256.value.trim()))
const canConfirm = computed(() => {
  if (busy.value || loading.value || !manifest.value) {
    return false
  }
  if (step.value === 'plan') {
    return !!plan.value
  }
  if (step.value === 'resources') {
    return !!resources.value && !missingPreviousHash.value
      && (!resources.value.switch_required || (!resources.value.shared_home && switchVersion.value))
  }
  if (props.updateInstance) {
    return isV6.value && !installationsLoading.value && !installationsError.value
      && (installations.value.length === 0 || !!selectedInstallation.value)
      && (isDshhome.value || !!selectedInstallation.value?.profile_name || !!legacyProfile.value.trim())
  }
  return importMode.value === 'existing' ? !!existingInstanceId.value : !!instanceName.value.trim()
})
const title = computed(() => t(props.updateInstance ? 'modpack.updateTitle' : 'modpack.importTitle'))
const okText = computed(() => {
  if (step.value === 'source' && isV6.value) {
    return t('modpack.reviewResources')
  }
  if (step.value === 'resources') {
    return t('modpack.previewPlan')
  }
  return t(props.updateInstance ? 'modpack.applyUpdate' : 'modpack.marketInstall')
})

function discard(current = archive.value) {
  if (current) {
    void api.discardPackforgeArchive(current.archive_id).catch(() => {})
  }
}

function clearPlan() {
  plan.value = undefined
  resolutions.value = Object.create(null)
}

function clearChoices() {
  resources.value = undefined
  decisions.value = Object.create(null)
  credentials.value = Object.create(null)
  reuse.value = Object.create(null)
  switchVersion.value = false
  previousSource.value = ''
  previousSha256.value = ''
  clearPlan()
}

function resetArchive() {
  request += 1
  discard()
  archive.value = undefined
  clearChoices()
  step.value = 'source'
  loading.value = false
}

watch(source, value => {
  if (market.value && value.trim() !== market.value.downloadUrl) {
    market.value = undefined
    marketId.value = undefined
  }
  if (archive.value || loading.value) {
    resetArchive()
  }
}, { flush: 'sync' })

async function loadInstallations(id: string) {
  const current = ++installationsRequest
  installationsLoading.value = true
  installationsError.value = ''
  try {
    const list = await api.listPackforgeInstallations(id)
    if (current === installationsRequest && props.visible && props.updateInstance?.id === id) {
      installations.value = list
      binding.value = list.length === 1 ? 0 : undefined
    }
  } catch (error) {
    if (current === installationsRequest && props.visible && props.updateInstance?.id === id) {
      installationsError.value = String(error)
    }
  } finally {
    if (current === installationsRequest) {
      installationsLoading.value = false
    }
  }
}

async function refreshMarket() {
  const current = ++marketRequest
  marketError.value = ''
  try {
    await store.refreshModpackMarket(true)
  } catch (error) {
    if (current === marketRequest && props.visible) {
      marketError.value = String(error)
    }
  }
}

watch(() => [props.visible, props.initialSource, props.marketPack, props.updateInstance?.id] as const, ([visible]) => {
  resetArchive()
  installationsRequest += 1
  marketRequest += 1
  installationsLoading.value = false
  installationsError.value = ''
  marketError.value = ''
  if (!visible) {
    return
  }
  importMode.value = 'new'
  existingInstanceId.value = undefined
  wslDistro.value = undefined
  force.value = false
  installations.value = []
  binding.value = undefined
  legacyProfile.value = props.updateInstance?.default_profile ?? props.updateInstance?.last_profile ?? ''
  source.value = props.initialSource ?? props.marketPack?.downloadUrl ?? ''
  market.value = props.marketPack
  marketId.value = props.marketPack?.id
  if (props.updateInstance) {
    void loadInstallations(props.updateInstance.id)
  } else {
    void api.listWslDistros().then(value => { distros.value = value }).catch(() => { distros.value = [] })
  }
  if (source.value) {
    void loadManifest()
  }
}, { immediate: true })

watch(binding, () => {
  if (market.value && !marketChoices.value.some(pack => pack.id === market.value?.id)) {
    source.value = ''
  }
})

watch(() => selectedInstallation.value?.market_id, id => {
  marketRequest += 1
  marketError.value = ''
  if (id && props.visible) {
    void refreshMarket()
  }
})

onBeforeUnmount(resetArchive)

async function loadManifest() {
  const input = source.value.trim()
  if (!input || loading.value || busy.value) {
    return
  }
  resetArchive()
  const current = ++request
  const pack = market.value
  loading.value = true
  try {
    const outcome = await api.inspectPackforgeArchive({
      source: input, market_id: pack?.id, sha256: pack?.sha256 || undefined, size: pack?.size || undefined,
    })
    if (current !== request || !props.visible) {
      discard(outcome)
      return
    }
    archive.value = outcome
    instanceName.value = packforgeText(outcome.manifest.displayName, locale.value) || outcome.manifest.name
    const preferredProfile = outcome.manifest.profileName?.trim() || 'pack'
    profileName.value = outcome.manifest.manifestVersion === 6 && ['web', 'headless'].includes(preferredProfile.toLowerCase())
      ? 'pack' : preferredProfile
    existingInstanceId.value = undefined
    importMode.value = 'new'
    wslDistro.value = undefined
  } catch (error) {
    if (current === request) {
      message.error(String(error))
    }
  } finally {
    if (current === request) {
      loading.value = false
    }
  }
}

function useMarketPack(id: unknown) {
  const pack = marketChoices.value.find(row => row.id === id)
  if (pack) {
    source.value = pack.downloadUrl
    market.value = pack
    marketId.value = pack.id
    void loadManifest()
  } else if (market.value) {
    source.value = ''
  }
}

async function pickFile(previous = false) {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({ multiple: false, filters: [{ name: 'DSH Modpack', extensions: ['dspack', 'tgz'] }] })
  if (typeof file === 'string') {
    if (previous) {
      previousSource.value = file
    } else {
      source.value = file
      await loadManifest()
    }
  }
}

function target(): PackforgeInstallTarget {
  const updating = !!props.updateInstance
  const existing = importMode.value === 'existing'
  return {
    instance_id: props.updateInstance?.id ?? (existing ? existingInstanceId.value : undefined),
    instance_name: !existing && !updating ? instanceName.value.trim() || undefined : undefined,
    profile_name: isDshhome.value ? undefined : updating
      ? selectedInstallation.value?.profile_name || legacyProfile.value.trim() || undefined
      : profileName.value.trim() || undefined,
    wsl_distro: allowWsl.value ? wslDistro.value : undefined,
    update: updating,
    switch_version: switchVersion.value,
    previous_source: previousSource.value.trim() || undefined,
    previous_sha256: previousSource.value.trim() ? previousSha256.value.trim() : undefined,
  }
}

function credentialSet(item: PackforgeInstallResource) {
  return credentials.value[item.key] ?? (credentials.value[item.key] = Object.create(null))
}

function skipped(item: PackforgeInstallResource) {
  const decision = decisions.value[item.key]
  return decision === 'keep' || decision === 'skip' || item.canReplace === false
}

function missing(item: PackforgeInstallResource, reference: string) {
  return !credentialSet(item)[reference] && !(reuse.value[item.key] && item.availableCredentials?.includes(reference))
}

function sanitizedCredentials() {
  const values: Record<string, Record<string, string>> = Object.create(null)
  const confirmed: Record<string, boolean> = Object.create(null)
  for (const item of resources.value?.items ?? []) {
    if (skipped(item)) {
      continue
    }
    const assigned = Object.entries(credentialSet(item)).filter(([, value]) => value !== '')
    if (assigned.length) {
      values[item.key] = Object.fromEntries(assigned)
    }
    if (reuse.value[item.key]) {
      confirmed[item.key] = true
    }
  }
  return { credentials: values, confirmed_credentials: confirmed }
}

async function queued() {
  archive.value = undefined
  credentials.value = Object.create(null)
  emit('update:visible', false)
  await store.refreshTasks()
  message.success(t('download.taskAdded'))
  router.push({ name: 'tasks' })
}

async function confirm() {
  if (!canConfirm.value || !archive.value) {
    return
  }
  const { archive_id } = archive.value
  const current = request
  busy.value = true
  try {
    if (!isV6.value) {
      const selected = target()
      await api.startImportModpackTask({
        archive_id, source: source.value.trim(), force: force.value,
        instance_name: selected.instance_name, profile_name: selected.profile_name,
        existing_instance_id: selected.instance_id, wsl_distro: selected.wsl_distro,
      })
      await queued()
    } else if (step.value === 'source') {
      const selected = target()
      const bound = selectedInstallation.value
      if (bound && (bound.pack_type !== manifest.value?.type || bound.name !== manifest.value.name)) {
        throw new Error(t('modpack.bindingMismatch'))
      }
      const result = await api.listPackforgeResources({ archive_id, target: selected })
      if (current === request) {
        resources.value = result
        for (const item of result.items) {
          decisions.value[item.key] = 'auto'
          credentialSet(item)
        }
        step.value = 'resources'
      }
    } else if (step.value === 'resources') {
      const chosen = Object.fromEntries(Object.entries(decisions.value)
        .filter((entry): entry is [string, PackforgeInstallDecision] => entry[1] !== 'auto'))
      const result = await api.previewPackforgeInstall({
        archive_id, target: target(), decisions: chosen, ...sanitizedCredentials(),
      })
      if (current === request) {
        clearPlan()
        plan.value = result
        step.value = 'plan'
      }
    } else if (plan.value) {
      const answers: Record<string, 'upstream'> = Object.create(null)
      for (const [key, selected] of Object.entries(resolutions.value)) {
        if (selected) {
          answers[key] = 'upstream'
        }
      }
      await api.startPackforgeInstall({ plan_id: plan.value.plan_id, resolutions: answers })
      await queued()
    }
  } catch (error) {
    message.error(String(error))
  } finally {
    busy.value = false
  }
}

function back() {
  if (step.value === 'plan') {
    clearPlan()
    step.value = 'resources'
  } else {
    clearChoices()
    step.value = 'source'
  }
}

function changeLabel(action: string) {
  const key = 'modpack.actions.' + action
  return te(key) ? t(key) : action
}

function close() {
  if (!busy.value) {
    emit('update:visible', false)
  }
}
</script>

<template>
  <dsh-modal
    :visible="visible"
    :title="title"
    :width="660"
    :ok-text="okText"
    :ok-loading="busy"
    :ok-button-props="{ disabled: !canConfirm }"
    :mask-closable="false"
    @ok="confirm"
    @cancel="close"
  >
    <dsh-scrollbar class="packforge-modal-scroll">
      <dsh-form :model="{ source, instanceName, profileName }" layout="vertical">
        <template v-if="step === 'source'">
          <dsh-alert v-if="updateInstance" class="packforge-section" type="info">
            {{ t('modpack.updateTarget', { name: updateInstance.name }) }}
          </dsh-alert>
          <dsh-spin v-if="installationsLoading" />
          <dsh-alert v-if="installationsError" class="packforge-section" type="error">
            {{ installationsError }}
            <dsh-button v-if="updateInstance" size="sm" variant="ghost" @click="loadInstallations(updateInstance.id)">
              {{ t('modpack.marketRetry') }}
            </dsh-button>
          </dsh-alert>
          <dsh-form-item v-if="updateInstance && installations.length" :label="t('modpack.installedBinding')" required>
            <dsh-select v-model="binding" :placeholder="t('modpack.chooseBinding')">
              <dsh-option v-for="(row, index) in installations" :key="index" :value="index">
                {{ row.name }} · {{ row.version }}<template v-if="row.profile_name"> · {{ row.profile_name }}</template>
              </dsh-option>
            </dsh-select>
          </dsh-form-item>
          <dsh-alert v-if="updateInstance && !installationsLoading && !installationsError && !installations.length"
            class="packforge-section" type="warning">
            {{ t('modpack.noRecordedPack') }}
          </dsh-alert>
          <template v-if="selectedInstallation?.market_id">
            <dsh-form-item :label="t('modpack.marketUpdateSource')">
              <div class="market-selection">
                <dsh-select :model-value="marketId" :loading="store.modpackMarketLoading"
                  :disabled="store.modpackMarketLoading || !marketChoices.length" allow-clear @change="useMarketPack">
                  <dsh-option v-for="pack in marketChoices" :key="pack.id" :value="pack.id">
                    {{ packforgeText(pack.displayName, locale) || pack.name }} · {{ pack.version }}
                  </dsh-option>
                </dsh-select>
                <dsh-button variant="secondary" :disabled="store.modpackMarketLoading" @click="refreshMarket">
                  {{ t(marketError ? 'modpack.marketRetry' : 'modpack.marketRefresh') }}
                </dsh-button>
              </div>
            </dsh-form-item>
            <dsh-alert v-if="marketError" class="packforge-section" type="warning">{{ marketError }}</dsh-alert>
          </template>
          <dsh-form-item :label="t('modpack.source')" required>
            <dsh-input v-model="source" :placeholder="t('modpack.sourceHint')" allow-clear @press-enter="loadManifest">
              <template #append>
                <dsh-button variant="secondary" :disabled="loading" @click="pickFile()">{{ t('modpack.pickFile') }}</dsh-button>
              </template>
            </dsh-input>
          </dsh-form-item>
          <dsh-form-item>
            <dsh-button variant="secondary" size="sm" :loading="loading" :disabled="!source.trim()" @click="loadManifest">
              {{ t('modpack.load') }}
            </dsh-button>
          </dsh-form-item>
        </template>

        <template v-if="manifest && step === 'source'">
          <dsh-alert type="info" class="packforge-section">
            {{ manifest.name }} v{{ manifest.version }}
            <template v-if="manifest.author"> · {{ manifest.author }}</template>
            <template v-if="manifest.dshVersions?.length"> · DSH {{ manifest.dshVersions.join(', ') }}</template>
            <template v-else-if="manifest.dshVersion"> · DSH {{ manifest.dshVersion }}</template>
            <template v-if="manifest.files?.length"> · {{ t('modpack.filesCount', { count: manifest.files.length }) }}</template>
          </dsh-alert>
          <dsh-alert v-for="warning in archive?.warnings ?? []" :key="warning" class="packforge-section" type="warning">
            {{ warning }}
          </dsh-alert>
          <PackforgePluginList v-if="isV6" :manifest="manifest" :icons="archive?.icons" />
          <dsh-alert v-if="isDshhome" type="info" class="packforge-section">
            {{ t('modpack.dshhomeHint', { count: Object.keys(manifest.profiles ?? {}).length, default: manifest.defaultProfile }) }}
          </dsh-alert>
          <template v-if="updateInstance">
            <dsh-alert v-if="!isV6" type="warning" class="packforge-section">{{ t('modpack.updateNeedsV6') }}</dsh-alert>
            <dsh-form-item v-if="!isDshhome && !selectedInstallation?.profile_name" :label="t('modpack.profileName')" required>
              <dsh-input v-model="legacyProfile" />
            </dsh-form-item>
          </template>
          <template v-else>
            <dsh-form-item v-if="!isDshhome" :label="t('modpack.target')">
              <dsh-radio-group v-model="importMode" type="button">
                <dsh-radio value="new">{{ t('modpack.targetNew') }}</dsh-radio>
                <dsh-radio value="existing" :disabled="eligibleInstances.length === 0">
                  {{ t('modpack.targetExisting') }}
                </dsh-radio>
              </dsh-radio-group>
            </dsh-form-item>
            <dsh-form-item v-if="importMode === 'existing'" :label="t('modpack.existingInstance')" required>
              <dsh-select v-model="existingInstanceId" :placeholder="t(isV6 ? 'modpack.compatibleInstanceHint' : 'modpack.existingInstanceHint')">
                <dsh-option v-for="inst in eligibleInstances" :key="inst.id" :value="inst.id">
                  {{ inst.name }}（{{ store.versionById(inst.version_id)?.version }}）
                </dsh-option>
              </dsh-select>
            </dsh-form-item>
            <dsh-form-item v-else :label="t('modpack.instanceName')" required>
              <dsh-input v-model="instanceName" />
            </dsh-form-item>
            <dsh-form-item v-if="allowWsl && distros.length" :label="t('modpack.wslTarget')">
              <dsh-select v-model="wslDistro" allow-clear :placeholder="t('modpack.wslTargetHint')">
                <dsh-option v-for="distro in distros" :key="distro" :value="distro">{{ distro }}</dsh-option>
              </dsh-select>
            </dsh-form-item>
            <dsh-alert v-if="allowWsl && wslDistro" type="info" class="packforge-section">
              {{ t('modpack.wslInstallHint', { distro: wslDistro }) }}
            </dsh-alert>
            <dsh-form-item v-if="!isDshhome" :label="t('modpack.profileName')">
              <dsh-input v-model="profileName" placeholder="pack" />
            </dsh-form-item>
            <dsh-form-item v-if="!isDshhome && !isV6">
              <dsh-checkbox v-model="force">{{ t('modpack.force') }}</dsh-checkbox>
            </dsh-form-item>
          </template>
        </template>

        <template v-if="step === 'resources' && resources">
          <dsh-alert class="packforge-section" :type="resources.switch_required ? 'warning' : 'info'">
            {{ t('modpack.chosenDsh', { version: resources.version }) }}
            <template v-if="resources.switch_required">
              <p v-if="resources.shared_home">{{ t('modpack.sharedVersionBlocked') }}</p>
              <dsh-checkbox v-else v-model="switchVersion">
                {{ t('modpack.switchDsh', { current: resources.current_version, version: resources.version }) }}
              </dsh-checkbox>
            </template>
          </dsh-alert>
          <dsh-alert v-for="warning in resources.warnings" :key="warning" type="warning" class="packforge-section">
            {{ warning }}
          </dsh-alert>
          <template v-if="resources.items.length">
            <h3>{{ t('modpack.resources') }}</h3>
            <section v-for="item in resources.items" :key="item.key" class="resource-card">
              <div class="resource-title">
                <strong>{{ packforgeText(item.name, locale) || item.id }}</strong>
                <dsh-tag v-if="item.conflict" color="orange" size="small">{{ t('modpack.conflict') }}</dsh-tag>
                <dsh-select v-model="decisions[item.key]" :aria-label="t('modpack.resourceAction')">
                  <dsh-option value="auto">{{ t(item.canReplace === false ? 'modpack.actions.keep' : 'modpack.autoResource') }}</dsh-option>
                  <dsh-option v-if="item.conflict && item.canReplace !== false" value="keep">{{ t('modpack.actions.keep') }}</dsh-option>
                  <dsh-option v-if="item.conflict && item.canReplace !== false" value="replace">{{ t('modpack.useIncoming') }}</dsh-option>
                  <dsh-option value="skip">{{ t('modpack.skip') }}</dsh-option>
                </dsh-select>
              </div>
              <p>{{ t('modpack.kinds.' + item.kind) }} · {{ item.scope }}<template v-if="item.destination"> · {{ item.destination }}</template></p>
              <p v-if="item.endpoint && item.endpoint !== item.destination">{{ item.endpoint }}</p>
              <p v-if="item.desc">{{ packforgeText(item.desc, locale) }}</p>
              <p v-if="item.notice">{{ item.notice }}</p>
              <template v-if="item.credentialRefs?.length && !skipped(item)">
                <dsh-checkbox v-if="item.availableCredentials?.length" v-model="reuse[item.key]">
                  {{ t('modpack.reuseCredentials') }}
                </dsh-checkbox>
                <dsh-form-item v-for="reference in item.credentialRefs" :key="reference" :label="reference">
                  <dsh-input-password v-model="credentials[item.key][reference]"
                    autocomplete="new-password" :placeholder="t(missing(item, reference) ? 'modpack.missingCredential' : 'modpack.credentialInput')" />
                </dsh-form-item>
                <p v-if="item.credentialRefs.some(reference => missing(item, reference))">{{ t('modpack.missingSkip') }}</p>
              </template>
            </section>
          </template>
          <dsh-alert v-else type="info" class="packforge-section">{{ t('modpack.noResources') }}</dsh-alert>
          <details v-if="updateInstance" :open="!!previousSource">
            <summary>{{ t('modpack.previousPackage') }}</summary>
            <dsh-form-item :label="t('modpack.source')">
              <dsh-input v-model="previousSource" allow-clear>
                <template #append>
                  <dsh-button variant="secondary" @click="pickFile(true)">{{ t('modpack.pickFile') }}</dsh-button>
                </template>
              </dsh-input>
            </dsh-form-item>
            <dsh-form-item v-if="previousSource.trim()" label="SHA-256" :validate-status="missingPreviousHash ? 'error' : undefined">
              <dsh-input v-model="previousSha256" :max-length="64" />
            </dsh-form-item>
          </details>
        </template>

        <template v-if="step === 'plan' && plan">
          <dsh-alert v-if="plan.unknown_base" type="warning" class="packforge-section">{{ t('modpack.unknownBaseline') }}</dsh-alert>
          <h3>{{ t('modpack.changes') }}</h3>
          <p v-if="!plan.changes.length">{{ t('modpack.noChanges') }}</p>
          <ul v-else class="change-list">
            <li v-for="(change, i) in plan.changes" :key="i">
              <dsh-tag size="small" :color="change.conflict ? 'orange' : 'gray'">{{ changeLabel(change.action) }}</dsh-tag>
              {{ change.path }}
            </li>
          </ul>
          <template v-if="plan.conflicts.length">
            <h3>{{ t('modpack.conflicts') }}</h3>
            <p>{{ t('modpack.defaultKeep') }}</p>
            <section v-for="conflict in plan.conflicts" :key="conflict.path" class="resource-card">
              <strong>{{ conflict.path }}</strong>
              <p v-if="conflict.shared">{{ t('modpack.sharedConflict') }}</p>
              <dsh-checkbox v-for="field in conflict.fields?.length ? conflict.fields : [{ key: conflict.path, label: '' }]"
                :key="field.key" v-model="resolutions[field.key]" :disabled="conflict.shared">
                {{ t('modpack.useIncoming') }}<template v-if="field.label"> · {{ field.label }}</template>
              </dsh-checkbox>
            </section>
          </template>
        </template>

        <dsh-button v-if="step !== 'source'" variant="secondary" size="sm" :disabled="busy" @click="back">
          {{ t('exportMulti.prev') }}
        </dsh-button>
      </dsh-form>
    </dsh-scrollbar>
  </dsh-modal>
</template>

<style scoped>
.packforge-modal-scroll { max-height: min(65vh, 720px); overflow-y: auto; padding: 0 6px; }
.packforge-section { margin-bottom: 12px; }
.market-selection { display: flex; align-items: center; gap: 8px; width: 100%; }
.market-selection :deep(.arco-select) { flex: 1 1 auto; min-width: 0; }
.resource-card { padding: 10px 12px; margin: 8px 0 12px; border: 1px solid var(--color-border-1); border-radius: 6px; overflow-wrap: anywhere; }
.resource-card p { margin: 6px 0; }
.resource-card :deep(.arco-checkbox) { display: flex; }
.resource-title { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.resource-title :deep(.arco-select) { width: min(210px, 100%); margin-inline-start: auto; }
.change-list { list-style: none; padding: 0; overflow-wrap: anywhere; }
.change-list li { padding: 5px 0; }
details { margin-bottom: 12px; }
summary { cursor: pointer; }
</style>
