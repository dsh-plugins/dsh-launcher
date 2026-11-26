<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import semver from 'semver'
import { api } from '@/api'
import type {
  InspectPackforgeExportInput, PackforgeExportCandidate, PackforgeExportChoices,
  PackforgePluginInfo, PackforgePluginOverride, PackforgeResourceGroup, PackforgeResourceSelection,
  PackforgeSelection, PackforgeSkillMode, PackforgeText,
} from '@/api/types'
import { HintIcon } from '@dsh-plugin/dsh-ui-components-vue'
import { packforgeText } from '@/utils/packforge'

const props = defineProps<{ input: InspectPackforgeExportInput }>()
const { locale, t } = useI18n()
const localeExample = JSON.stringify({ 'zh-CN': '示例', en: 'Example' })
const inspection = ref<PackforgeExportChoices>()
const loading = ref(false)
const failure = ref('')
let request = 0

interface SkillChoice {
  id: string
  scope: string
  mode: PackforgeSkillMode
  repository: string
  subdir: string
  ref: string
  name: string
  desc: string
}

interface PluginChoice {
  profile: string
  id: string
  version?: string
  original?: PackforgePluginInfo
  name: string
  desc: string
  icon: string
  registry: string
  registryDefault: string
}

const groups: { key: PackforgeResourceGroup; label: string }[] = [
  { key: 'paths', label: 'path' }, { key: 'providers', label: 'provider' },
  { key: 'mcp', label: 'mcp' }, { key: 'skills', label: 'skill' },
]
const checked = ref<Record<string, boolean>>(Object.create(null))
const skills = ref<Record<string, SkillChoice>>(Object.create(null))
const newSkills = ref<{ key: number; choice: SkillChoice }[]>([])
let nextSkillKey = 0
const plugins = ref<PluginChoice[]>([])
const selectedVersions = ref<string[]>([])
const preferredVersion = ref<string>()
const availableVersions = computed(() => [...new Set([
  ...(inspection.value?.dsh_versions ?? []), ...selectedVersions.value,
])].filter(exactVersion).sort(semver.rcompare))
const skillModes: PackforgeSkillMode[] = ['embedded', 'fixed', 'track']
const remoteSkillModes: PackforgeSkillMode[] = ['fixed', 'track']
const scopeChoices = computed(() => ['home', ...(props.input.whole ? props.input.profiles.map(name => 'profile:' + name) : [])])

function asText(value?: PackforgeText) {
  return typeof value === 'object' ? JSON.stringify(value) : value || ''
}

function exactVersion(value: string) {
  return /^\d/.test(value) && semver.valid(value) !== null
}

function newSkill(candidate?: PackforgeExportCandidate): SkillChoice {
  return {
    id: candidate?.id || '', scope: candidate?.scope || 'home', mode: candidate ? 'embedded' : 'fixed',
    repository: candidate?.source?.repository || candidate?.source?.repo || '',
    subdir: candidate?.source?.subdir || '', ref: candidate?.source?.commit || candidate?.source?.ref || '',
    name: asText(candidate?.name), desc: asText(candidate?.desc),
  }
}

async function inspect() {
  const current = ++request
  failure.value = ''
  loading.value = true
  try {
    const outcome = await api.inspectPackforgeExport(props.input)
    if (current !== request) {
      return
    }
    const byProfile = Object.entries(outcome.units)
    plugins.value = byProfile.flatMap(([profile, unit]) =>
      [...new Set([...unit.bundles, ...Object.keys(unit.dependencies)])].map(id => {
        const original = unit.plugins?.[id]
        const version = unit.dependencies[id]
        const prior = plugins.value.find(row => row.profile === profile && row.id === id && row.version === version)
        const registryDefault = original?.source?.registry ?? ''
        if (prior) {
          return prior.registry.trim() !== prior.registryDefault
            ? prior : { ...prior, registry: registryDefault, registryDefault }
        }
        return {
          profile, id, version, original,
          name: asText(original?.name), desc: asText(original?.desc), icon: original?.icon ?? '',
          registry: registryDefault, registryDefault,
        }
      }),
    )
    const previous = skills.value
    skills.value = Object.fromEntries((outcome.candidates.skills ?? []).map(item => [item.key, previous[item.key] ?? newSkill(item)]))
    const version = outcome.dsh_version
    if (!inspection.value && version) {
      preferredVersion.value = version
      selectedVersions.value = [version]
    }
    inspection.value = outcome
  } catch (error) {
    if (current === request) {
      failure.value = String(error)
    }
  } finally {
    if (current === request) {
      loading.value = false
    }
  }
}

watch(() => JSON.stringify(props.input), inspect, { immediate: true })

watch(selectedVersions, versions => {
  if (!preferredVersion.value || !versions.includes(preferredVersion.value)) {
    preferredVersion.value = versions[0]
  }
})

function parseLabel(raw: string): PackforgeText | undefined {
  const text = raw.trim()
  if (!text) {
    return undefined
  }
  if (!text.startsWith('{')) {
    return text
  }
  let map: unknown
  try {
    map = JSON.parse(text)
  } catch {
    throw new Error(t('modpack.localeMapError'))
  }
  if (!map || typeof map !== 'object' || Array.isArray(map)
    || !Object.keys(map).length || Object.values(map).some(value => typeof value !== 'string' || !value.trim())) {
    throw new Error(t('modpack.localeMapError'))
  }
  return map as Record<string, string>
}

function setSkillMode(skill: SkillChoice, mode: unknown, candidate?: PackforgeExportCandidate) {
  if (mode === 'fixed' || mode === 'track' || mode === 'embedded') {
    skill.mode = mode
    if (candidate?.source) {
      skill.ref = mode === 'fixed' ? candidate.source.commit || '' : candidate.source.ref || ''
    }
  }
}

function skillSelection(choice: SkillChoice, key: string, remote = false): PackforgeResourceSelection {
  const id = choice.id.trim()
  const repository = choice.repository.trim()
  const reference = choice.ref.trim()
  if (!id || id === '.' || id === '..' || /[\\/:*?"<>|]/.test(id) || !scopeChoices.value.includes(choice.scope)) {
    throw new Error(t('modpack.skillIdError'))
  }
  if (choice.mode !== 'embedded' && (!repository || !reference
    || (choice.mode === 'fixed' && !/^(?:[a-f\d]{40}|[a-f\d]{64})$/i.test(reference)))) {
    throw new Error(t('modpack.skillSourceError', { name: id }))
  }
  return {
    key, id, scope: remote ? choice.scope : undefined, mode: choice.mode,
    repository: choice.mode !== 'embedded' ? repository : undefined,
    subdir: choice.mode !== 'embedded' ? choice.subdir.trim() || undefined : undefined,
    ref: choice.mode !== 'embedded' ? reference : undefined,
    name: parseLabel(choice.name), desc: parseLabel(choice.desc),
  }
}

function getOptions() {
  if (!inspection.value || loading.value || failure.value) {
    throw new Error(t('modpack.exportScanRequired'))
  }
  const versions = [...new Set(selectedVersions.value.map(version => version.trim()).filter(Boolean))]
  if (versions.some(version => !exactVersion(version))) {
    throw new Error(t('modpack.versionValidation'))
  }
  const selection: PackforgeSelection = { paths: [], providers: [], mcp: [], skills: [] }
  for (const { key } of groups) {
    for (const candidate of inspection.value.candidates[key] ?? []) {
      if (!candidate.unsupported && checked.value[candidate.key]) {
        selection[key].push(key === 'skills' ? skillSelection(skills.value[candidate.key], candidate.key) : candidate.key)
      }
    }
  }
  const skillKeys = new Set(selection.skills.map(value => typeof value === 'string' ? value : value.key))
  for (const { choice: skill } of newSkills.value) {
    const key = 'skill:' + skill.scope + ':' + skill.id.trim()
    if (skillKeys.has(key)) {
      throw new Error(t('modpack.duplicateSkill', { name: skill.id }))
    }
    skillKeys.add(key)
    selection.skills.push(skillSelection(skill, key, true))
  }

  const profilePlugins: Record<string, Record<string, PackforgePluginOverride>> = Object.create(null)
  for (const row of plugins.value) {
    const entry: PackforgePluginOverride = {}
    const original = row.original
    if (row.name.trim() !== asText(original?.name)) {
      entry.name = parseLabel(row.name)
    }
    if (row.desc.trim() !== asText(original?.desc)) {
      entry.desc = parseLabel(row.desc)
    }
    if (row.icon.trim() && row.icon.trim() !== (original?.icon || '')) {
      const icon = new URL(row.icon.trim())
      if (icon.protocol !== 'https:' || icon.username || icon.password) {
        throw new Error(t('modpack.iconValidation'))
      }
      entry.icon = icon.href
    }
    const registry = row.registry.trim()
    if (registry !== row.registryDefault) {
      if (!row.version || !exactVersion(row.version)) {
        throw new Error(t('modpack.registryVersionRequired', { name: row.id }))
      }
      if (registry) {
        const url = new URL(registry)
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
          throw new Error(t('modpack.registryValidation'))
        }
        entry.source = { type: 'npm', registry: url.href }
      } else {
        entry.source = null
      }
      entry.version = row.version
    }
    if (Object.values(entry).some(value => value !== undefined)) {
      (profilePlugins[row.profile] ??= Object.create(null))[row.id] = entry
    }
  }

  return {
    dshVersions: versions.length ? versions : undefined,
    dshVersion: preferredVersion.value && versions.includes(preferredVersion.value) ? preferredVersion.value : versions[0],
    selection,
    profilePlugins,
    plugins: profilePlugins[props.input.profiles[0]] ?? {},
  }
}

defineExpose({ getOptions })
</script>

<template>
  <div class="dl-card">
    <div class="dl-card-title"><h3>{{ t('modpack.exportResources') }}</h3></div>
    <dsh-spin v-if="loading" />
    <dsh-alert v-else-if="failure" type="error">
      {{ failure }}
      <dsh-button variant="ghost" size="sm" @click="inspect">{{ t('modpack.marketRetry') }}</dsh-button>
    </dsh-alert>
    <template v-else-if="inspection">
      <dsh-form :model="{}" layout="vertical">
        <dsh-form-item>
          <template #label>
            {{ t('modpack.compatibleVersions') }}
            <HintIcon :content="t('modpack.compatibleVersionsHint')" />
          </template>
          <dsh-select v-model="selectedVersions" multiple allow-create allow-search>
            <dsh-option v-for="version in availableVersions" :key="version" :value="version">{{ version }}</dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item v-if="selectedVersions.length" :label="t('modpack.preferredVersion')">
          <dsh-select v-model="preferredVersion">
            <dsh-option v-for="version in selectedVersions" :key="version" :value="version">{{ version }}</dsh-option>
          </dsh-select>
        </dsh-form-item>
        <template v-for="group in groups" :key="group.key">
          <details v-if="inspection.candidates[group.key]?.length" class="packforge-group">
            <summary>{{ t('modpack.kinds.' + group.label) }}</summary>
            <section v-for="item in inspection.candidates[group.key]" :key="item.key" class="packforge-entry">
              <dsh-checkbox v-model="checked[item.key]" :disabled="item.unsupported">
                {{ packforgeText(item.name, locale) || item.id }} · {{ item.scope }}
              </dsh-checkbox>
              <HintIcon v-if="item.unsupported" :content="t('modpack.unsupportedResource')" />
              <p v-if="item.desc">{{ packforgeText(item.desc, locale) }}</p>
              <p v-if="item.notice">{{ item.notice }}</p>
              <template v-if="group.key === 'skills' && checked[item.key]">
                <dsh-form-item :label="t('modpack.skillMode')">
                  <dsh-select :model-value="skills[item.key].mode" @change="value => setSkillMode(skills[item.key], value, item)">
                    <dsh-option v-for="mode in skillModes" :key="mode" :value="mode">{{ t('modpack.skillModes.' + mode) }}</dsh-option>
                  </dsh-select>
                </dsh-form-item>
                <template v-if="skills[item.key].mode !== 'embedded'">
                  <dsh-form-item :label="t('modpack.skillRepo')"><dsh-input v-model="skills[item.key].repository" /></dsh-form-item>
                  <dsh-form-item :label="t('modpack.skillSubdir')"><dsh-input v-model="skills[item.key].subdir" /></dsh-form-item>
                  <dsh-form-item :label="t('modpack.skillRef')"><dsh-input v-model="skills[item.key].ref" /></dsh-form-item>
                </template>
              </template>
            </section>
          </details>
        </template>
        <section v-for="({ key, choice: skill }, index) in newSkills" :key="key" class="packforge-entry">
          <dsh-form-item :label="t('modpack.skillId')" required><dsh-input v-model="skill.id" /></dsh-form-item>
          <dsh-form-item v-if="input.whole" :label="t('modpack.resourceScope')">
            <dsh-select v-model="skill.scope">
              <dsh-option v-for="scope in scopeChoices" :key="scope" :value="scope">{{ scope }}</dsh-option>
            </dsh-select>
          </dsh-form-item>
          <dsh-form-item :label="t('modpack.skillMode')">
            <dsh-select :model-value="skill.mode" @change="value => setSkillMode(skill, value)">
              <dsh-option v-for="mode in remoteSkillModes" :key="mode" :value="mode">{{ t('modpack.skillModes.' + mode) }}</dsh-option>
            </dsh-select>
          </dsh-form-item>
          <dsh-form-item :label="t('modpack.skillRepo')" required><dsh-input v-model="skill.repository" /></dsh-form-item>
          <dsh-form-item :label="t('modpack.skillSubdir')"><dsh-input v-model="skill.subdir" /></dsh-form-item>
          <dsh-form-item :label="t('modpack.skillRef')" required><dsh-input v-model="skill.ref" /></dsh-form-item>
          <dsh-form-item :label="t('modpack.displayLabel')"><dsh-input v-model="skill.name" /></dsh-form-item>
          <dsh-button variant="ghost" @click="newSkills.splice(index, 1)">{{ t('instances.table.delete') }}</dsh-button>
        </section>
        <dsh-button variant="secondary" size="sm" @click="newSkills.push({ key: nextSkillKey++, choice: newSkill() })">{{ t('modpack.addRemoteSkill') }}</dsh-button>

        <details v-if="plugins.length" class="packforge-group">
          <summary>{{ t('modpack.plugins') }}</summary>
          <details v-for="row in plugins" :key="row.profile + '/' + row.id" class="packforge-entry">
            <summary>{{ row.profile }} · {{ row.id }}<template v-if="row.version"> · {{ row.version }}</template></summary>
            <dsh-form-item>
              <template #label>
                {{ t('modpack.displayLabel') }}
                <HintIcon :content="t('modpack.localeMapHint', { example: localeExample })" />
              </template>
              <dsh-input v-model="row.name" />
            </dsh-form-item>
            <dsh-form-item>
              <template #label>
                {{ t('modpack.descriptionLabel') }}
                <HintIcon :content="t('modpack.localeMapHint', { example: localeExample })" />
              </template>
              <dsh-input v-model="row.desc" />
            </dsh-form-item>
            <dsh-form-item :label="t('modpack.pluginIcon')"><dsh-input v-model="row.icon" /></dsh-form-item>
            <dsh-form-item v-if="row.version && exactVersion(row.version)" :label="t('modpack.registry')">
              <dsh-input v-model="row.registry" :placeholder="t('modpack.defaultRegistry')" />
            </dsh-form-item>
          </details>
        </details>
      </dsh-form>
    </template>
  </div>
</template>

<style scoped>
.packforge-group { margin: 12px 0; }
.packforge-entry { padding: 8px 10px; margin: 6px 0; border: 1px solid var(--color-border-1); border-radius: 6px; overflow-wrap: anywhere; }
.packforge-entry p { margin: 6px 0; }
summary { cursor: pointer; }
</style>
