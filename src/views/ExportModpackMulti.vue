<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { ExportContents, ExportProfileSpec } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const ctx = store.modpackExportMulti

// --- Step 1: profile selection ------------------------------------------------

const step = ref(1)
const profiles = ref<string[]>([])
const selected = ref<string[]>([])
const expanded = ref<string | null>(null)
const busy = ref(false)

/** Per-profile content selection, created on first expand. */
const contentsMap = reactive<Record<string, ExportContents>>({})

/** Templates never ship in a dshhome pack (manifest v5 §4). */
const EXCLUDED = new Set(['__temp__', 'node_modules', 'web', 'headless'])

function defaultContents(): ExportContents {
  return { patch: true, lockfile: true, workspace: true, icon: false, extra_files: false }
}

function contentsFor(p: string): ExportContents {
  if (!contentsMap[p]) contentsMap[p] = defaultContents()
  return contentsMap[p]
}

function toggleExpand(p: string) {
  expanded.value = expanded.value === p ? null : p
  contentsFor(p)
}

function setSelected(p: string, v: boolean | (string | number | boolean)[]) {
  const on = v === true
  selected.value = on ? [...selected.value, p] : selected.value.filter((x) => x !== p)
}

const canNext = computed(() => selected.value.length > 0)

onMounted(async () => {
  if (!ctx) {
    router.replace({ name: 'instances' })
    return
  }
  const list = await api.listProfiles(ctx.homeId)
  profiles.value = list.filter((p) => !EXCLUDED.has(p))
  if (ctx.defaultProfile && profiles.value.includes(ctx.defaultProfile)) {
    selected.value = [ctx.defaultProfile]
  }
})

function goBack() {
  if (ctx) router.push({ name: 'instance-edit', params: { id: ctx.instanceId } })
  else router.push({ name: 'instances' })
}

// --- Step 2: pack metadata ----------------------------------------------------

const form = ref({
  name: '',
  version: '1.0.0',
  displayName: ctx?.displayName ?? '',
  description: '',
  author: '',
})

const defaultProfile = ref('')
const includeIcon = ref(true)

function goNext() {
  if (!canNext.value) return
  defaultProfile.value =
    ctx?.defaultProfile && selected.value.includes(ctx.defaultProfile)
      ? ctx.defaultProfile
      : selected.value[0]
  step.value = 2
}

/** Default save-file name for the dialog: `<name>-<version>.dspack`. */
function defaultFileName() {
  const name = form.value.name.trim() || ctx?.displayName || 'dsh-home-pack'
  const version = form.value.version.trim() || '1.0.0'
  return `${name}-${version}.dspack`
}

async function startExport() {
  if (!ctx) return
  // The save location is picked at export time, not in the form above.
  const { save } = await import('@tauri-apps/plugin-dialog')
  const outFile = await save({
    defaultPath: defaultFileName(),
    filters: [{ name: 'DSH Modpack', extensions: ['dspack'] }],
  })
  if (!outFile) return
  const specs: ExportProfileSpec[] = selected.value.map((p) => ({
    profile: p,
    contents: { ...contentsFor(p) },
  }))
  busy.value = true
  try {
    const path = await api.exportDshhomeModpack({
      home_id: ctx.homeId,
      instance_id: ctx.instanceId,
      profiles: specs,
      out_file: outFile,
      name: form.value.name.trim() || undefined,
      version: form.value.version.trim() || undefined,
      displayName: form.value.displayName.trim() || undefined,
      description: form.value.description.trim() || undefined,
      author: form.value.author.trim() || undefined,
      defaultProfile: defaultProfile.value || undefined,
      icon: includeIcon.value,
    })
    Message.success(t('exportPack.exported', { path }))
    goBack()
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="export-page">
    <a-page-header
      class="export-header"
      :title="t('exportMulti.title')"
      :sub-title="ctx ? ctx.displayName : ''"
      @back="goBack"
    />

    <template v-if="ctx">
      <a-steps :current="step" class="export-steps">
        <a-step>{{ t('exportMulti.stepSelect') }}</a-step>
        <a-step>{{ t('exportMulti.stepInfo') }}</a-step>
      </a-steps>

      <!-- Step 1: pick profiles + per-profile contents -->
      <div v-if="step === 1" class="dl-card">
        <div class="dl-card-title"><h3>{{ t('exportMulti.pickProfiles') }}</h3></div>
        <p class="step-desc">{{ t('exportMulti.pickProfilesHint') }}</p>
        <a-empty v-if="profiles.length === 0" :description="t('exportMulti.noProfiles')" />
        <div v-for="p in profiles" :key="p" class="profile-block">
          <div class="profile-row" @click="toggleExpand(p)">
            <a-checkbox
              :model-value="selected.includes(p)"
              @change="(v: boolean | (string | number | boolean)[]) => setSelected(p, v)"
              @click.stop
            />
            <span class="profile-name">
              {{ p }}
              <a-tag v-if="ctx.defaultProfile === p" color="arcoblue" size="small">
                {{ t('instanceEdit.profileDefaultTag') }}
              </a-tag>
            </span>
            <span class="profile-expand">{{ expanded === p ? '▲' : '▼' }}</span>
          </div>
          <div v-if="expanded === p" class="profile-contents">
            <div class="content-row">
              <a-checkbox :model-value="true" disabled>{{ t('exportPack.contentManifest') }}</a-checkbox>
              <div class="content-hint">{{ t('exportPack.contentManifestHint') }}</div>
            </div>
            <div class="content-row">
              <a-checkbox v-model="contentsFor(p).patch">{{ t('exportPack.contentPatch') }}</a-checkbox>
              <div class="content-hint">{{ t('exportPack.contentPatchHint') }}</div>
            </div>
            <div class="content-row">
              <a-checkbox v-model="contentsFor(p).lockfile">{{ t('exportPack.contentLockfile') }}</a-checkbox>
              <div class="content-hint">{{ t('exportPack.contentLockfileHint') }}</div>
            </div>
            <div class="content-row">
              <a-checkbox v-model="contentsFor(p).workspace">{{ t('exportPack.contentWorkspace') }}</a-checkbox>
              <div class="content-hint">{{ t('exportPack.contentWorkspaceHint') }}</div>
            </div>
            <div class="content-row">
              <a-checkbox v-model="contentsFor(p).extra_files">{{ t('exportPack.contentExtra') }}</a-checkbox>
              <div class="content-hint">{{ t('exportPack.contentExtraHint') }}</div>
            </div>
          </div>
        </div>
        <div class="export-actions">
          <a-button type="primary" size="large" :disabled="!canNext" @click="goNext">
            {{ t('exportMulti.next') }}
          </a-button>
        </div>
      </div>

      <!-- Step 2: pack metadata -->
      <template v-else>
        <div class="dl-card">
          <div class="dl-card-title"><h3>{{ t('exportPack.basic') }}</h3></div>
          <a-form :model="form" layout="vertical">
            <div class="form-row">
              <a-form-item :label="t('exportPack.name')" class="form-col">
                <a-input v-model="form.name" :placeholder="ctx.displayName" />
              </a-form-item>
              <a-form-item :label="t('exportPack.version')" class="form-col">
                <a-input v-model="form.version" placeholder="1.0.0" />
              </a-form-item>
            </div>
            <a-form-item :label="t('exportPack.displayName')">
              <a-input v-model="form.displayName" />
            </a-form-item>
            <a-form-item :label="t('exportPack.description')">
              <a-textarea v-model="form.description" :auto-size="{ minRows: 2, maxRows: 4 }" />
            </a-form-item>
            <a-form-item :label="t('exportPack.author')">
              <a-input v-model="form.author" />
            </a-form-item>
            <a-form-item :label="t('exportMulti.defaultProfile')">
              <a-select v-model="defaultProfile">
                <a-option v-for="p in selected" :key="p" :value="p">{{ p }}</a-option>
              </a-select>
            </a-form-item>
            <a-form-item>
              <a-checkbox v-model="includeIcon">{{ t('exportPack.contentIcon') }}</a-checkbox>
            </a-form-item>
          </a-form>
        </div>

        <div class="export-actions">
          <a-space size="large">
            <a-button size="large" @click="step = 1">{{ t('exportMulti.prev') }}</a-button>
            <a-button type="primary" size="large" :loading="busy" @click="startExport">
              {{ t('exportPack.start') }}
            </a-button>
          </a-space>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.export-page {
  max-width: 720px;
  margin: 0 auto;
  padding: 0 16px 32px;
}

.export-header {
  padding-left: 0;
  padding-right: 0;
}

.export-steps {
  margin-bottom: 20px;
}

.step-desc {
  margin: 0 0 12px;
  color: var(--color-text-3);
  font-size: 13px;
}

.profile-block {
  border-bottom: 1px solid var(--color-border-1);
}

.profile-block:last-of-type {
  border-bottom: none;
}

.profile-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 4px;
  cursor: pointer;
  user-select: none;
}

.profile-row:hover {
  background: var(--color-fill-1);
}

.profile-name {
  flex: 1;
  font-weight: 600;
}

.profile-expand {
  color: var(--color-text-3);
  font-size: 12px;
}

.profile-contents {
  padding: 4px 4px 10px 30px;
}

.content-row {
  padding: 6px 4px;
}

.content-hint {
  margin: 2px 0 0 26px;
  font-size: 12px;
  color: var(--color-text-3);
}

.form-row {
  display: flex;
  gap: 16px;
}

.form-col {
  flex: 1;
  min-width: 0;
}

.export-actions {
  display: flex;
  justify-content: center;
  margin-top: 24px;
}
</style>
