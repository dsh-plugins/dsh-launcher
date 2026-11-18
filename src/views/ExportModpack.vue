<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { SkillInfo } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import HintIcon from '@/components/HintIcon.vue'

const { message: Message } = useFeedback()

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const ctx = store.modpackExport

const form = ref({
  name: ctx?.profile ?? '',
  version: '1.0.0',
  displayName: ctx?.displayName ?? '',
  description: '',
  author: '',
})

/** Content selection; the manifest itself (bundles + pinned dependencies)
 * is the pack's core and always included. */
const contents = ref({
  patch: true,
  lockfile: true,
  workspace: true,
  icon: true,
  extra_files: false,
  agents_md: false,
})

/** Skills of the export HOME + the selected on-disk entries (issue #58). */
const skillList = ref<SkillInfo[]>([])
const skillSelected = ref<string[]>([])

/** Select-all checkbox state for the SKILL list. */
const skillAllChecked = computed(
  () => skillList.value.length > 0 && skillSelected.value.length === skillList.value.length,
)
const skillIndeterminate = computed(
  () => skillSelected.value.length > 0 && skillSelected.value.length < skillList.value.length,
)

function onToggleAllSkills(value: boolean | (string | number | boolean)[]) {
  skillSelected.value = value === true ? skillList.value.map((s) => s.entry) : []
}

const busy = ref(false)

onMounted(async () => {
  if (!ctx) {
    router.replace({ name: 'instances' })
    return
  }
  try {
    skillList.value = await api.listInstanceSkills(ctx.homeId)
  } catch (e) {
    Message.error(String(e))
  }
})

function goBack() {
  if (ctx) router.push({ name: 'instance-edit', params: { id: ctx.instanceId } })
  else router.push({ name: 'instances' })
}

/** Default save-file name for the dialog: `<name>-<version>.dspack`. */
function defaultFileName() {
  const name = form.value.name.trim() || ctx?.profile || 'modpack'
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
  busy.value = true
  try {
    const path = await api.exportModpack({
      home_id: ctx.homeId,
      profile: ctx.profile,
      out_file: outFile,
      name: form.value.name.trim() || undefined,
      version: form.value.version.trim() || undefined,
      displayName: form.value.displayName.trim() || undefined,
      description: form.value.description.trim() || undefined,
      author: form.value.author.trim() || undefined,
      contents: { ...contents.value, skills: [...skillSelected.value] },
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
    <dsh-page-header
      class="export-header"
      :title="t('exportPack.title')"
      :sub-title="ctx ? `Profile「${ctx.profile}」` : ''"
      @back="goBack"
    />

    <template v-if="ctx">
      <!-- 基本信息 -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('exportPack.basic') }}</h3></div>
        <dsh-form :model="form" layout="vertical">
          <div class="form-row">
            <dsh-form-item :label="t('exportPack.name')" class="form-col">
              <dsh-input v-model="form.name" />
            </dsh-form-item>
            <dsh-form-item :label="t('exportPack.version')" class="form-col">
              <dsh-input v-model="form.version" placeholder="1.0.0" />
            </dsh-form-item>
          </div>
          <dsh-form-item :label="t('exportPack.displayName')">
            <dsh-input v-model="form.displayName" />
          </dsh-form-item>
          <dsh-form-item :label="t('exportPack.description')">
            <dsh-textarea v-model="form.description" :auto-size="{ minRows: 2, maxRows: 4 }" />
          </dsh-form-item>
          <dsh-form-item :label="t('exportPack.author')">
            <dsh-input v-model="form.author" />
          </dsh-form-item>
        </dsh-form>
      </div>

      <!-- 导出内容列表 -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('exportPack.contents') }}</h3></div>
        <div class="content-row">
          <dsh-checkbox :model-value="true" disabled>
            {{ t('exportPack.contentManifest') }}
            <HintIcon :content="t('exportPack.contentManifestHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.patch">
            {{ t('exportPack.contentPatch') }}
            <HintIcon :content="t('exportPack.contentPatchHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.lockfile">
            {{ t('exportPack.contentLockfile') }}
            <HintIcon :content="t('exportPack.contentLockfileHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.workspace">
            {{ t('exportPack.contentWorkspace') }}
            <HintIcon :content="t('exportPack.contentWorkspaceHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.icon">
            {{ t('exportPack.contentIcon') }}
            <HintIcon :content="t('exportPack.contentIconHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.extra_files">
            {{ t('exportPack.contentExtra') }}
            <HintIcon :content="t('exportPack.contentExtraHint')" />
          </dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.agents_md">
            {{ t('exportPack.contentAgents') }}
            <HintIcon :content="t('exportPack.contentAgentsHint')" />
          </dsh-checkbox>
        </div>
        <div v-if="skillList.length > 0" class="content-row content-skills">
          <div class="content-skills-title">
            <dsh-checkbox
              :model-value="skillAllChecked"
              :indeterminate="skillIndeterminate"
              @change="onToggleAllSkills"
            >
              {{ t('exportPack.contentSkills') }}
            </dsh-checkbox>
            <HintIcon :content="t('exportPack.contentSkillsHint')" />
          </div>
          <dsh-checkbox-group v-model="skillSelected" class="content-skills-list">
            <dsh-checkbox v-for="s in skillList" :key="s.entry" :value="s.entry">
              {{ s.name }}
              <span v-if="s.name !== s.entry" class="content-skill-entry">({{ s.entry }})</span>
            </dsh-checkbox>
          </dsh-checkbox-group>
        </div>
      </div>

      <div class="export-actions">
        <dsh-button variant="primary"
          size="lg"
          :loading="busy"
          @click="startExport"
        >
          {{ t('exportPack.start') }}
        </dsh-button>
      </div>
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

.form-row {
  display: flex;
  gap: 16px;
}

.form-col {
  flex: 1;
  min-width: 0;
}

.content-row {
  padding: 10px 4px;
  border-bottom: 1px solid var(--color-border-1);
}

.content-row:last-child {
  border-bottom: none;
}

.content-skills-title {
  color: var(--color-text-1);
  margin-bottom: 6px;
}

.content-skills-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-left: 22px;
}

.content-skill-entry {
  color: var(--color-text-3);
  font-size: 12px;
}

.export-actions {
  display: flex;
  justify-content: center;
  margin-top: 24px;
}
</style>
