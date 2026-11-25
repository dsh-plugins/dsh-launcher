<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { HintIcon, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import PackforgeExportOptions from '@/components/PackforgeExportOptions.vue'

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
  presets: false,
  home_extra_files: false,
})

const exportOptions = ref<InstanceType<typeof PackforgeExportOptions>>()
const inspectInput = computed(() => ctx ? {
  home_id: ctx.homeId, instance_id: ctx.instanceId, profiles: [ctx.profile], whole: false,
} : undefined)
const busy = ref(false)

onMounted(() => {
  if (!ctx) {
    router.replace({ name: 'instances' })
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
  if (!ctx || !exportOptions.value || busy.value) {
    return
  }
  busy.value = true
  try {
    const { dshVersion, dshVersions, plugins, selection } = exportOptions.value.getOptions()
    const { save } = await import('@tauri-apps/plugin-dialog')
    const outFile = await save({
      defaultPath: defaultFileName(),
      filters: [{ name: 'DSH Modpack', extensions: ['dspack'] }],
    })
    if (!outFile) {
      return
    }
    const path = await api.exportModpack({
      home_id: ctx.homeId,
      instance_id: ctx.instanceId,
      profile: ctx.profile,
      out_file: outFile,
      name: form.value.name.trim() || undefined,
      version: form.value.version.trim() || undefined,
      displayName: form.value.displayName.trim() || undefined,
      description: form.value.description.trim() || undefined,
      author: form.value.author.trim() || undefined,
      contents: { ...contents.value },
      dshVersion, dshVersions, plugins, selection,
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
        <div class="content-row">
          <dsh-checkbox v-model="contents.presets">{{ t('modpack.homePresets') }}</dsh-checkbox>
        </div>
        <div class="content-row">
          <dsh-checkbox v-model="contents.home_extra_files">{{ t('modpack.homeData') }}</dsh-checkbox>
        </div>
      </div>

      <PackforgeExportOptions v-if="inspectInput" ref="exportOptions" :input="inspectInput" />

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

.export-actions {
  display: flex;
  justify-content: center;
  margin-top: 24px;
}
</style>
