<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'

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
})

const busy = ref(false)

onMounted(() => {
  if (!ctx) router.replace({ name: 'instances' })
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
      contents: { ...contents.value },
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
      :title="t('exportPack.title')"
      :sub-title="ctx ? `Profile「${ctx.profile}」` : ''"
      @back="goBack"
    />

    <template v-if="ctx">
      <!-- 基本信息 -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('exportPack.basic') }}</h3></div>
        <a-form :model="form" layout="vertical">
          <div class="form-row">
            <a-form-item :label="t('exportPack.name')" class="form-col">
              <a-input v-model="form.name" />
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
        </a-form>
      </div>

      <!-- 导出内容列表 -->
      <div class="dl-card">
        <div class="dl-card-title"><h3>{{ t('exportPack.contents') }}</h3></div>
        <div class="content-row">
          <a-checkbox :model-value="true" disabled>{{ t('exportPack.contentManifest') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentManifestHint') }}</div>
        </div>
        <div class="content-row">
          <a-checkbox v-model="contents.patch">{{ t('exportPack.contentPatch') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentPatchHint') }}</div>
        </div>
        <div class="content-row">
          <a-checkbox v-model="contents.lockfile">{{ t('exportPack.contentLockfile') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentLockfileHint') }}</div>
        </div>
        <div class="content-row">
          <a-checkbox v-model="contents.workspace">{{ t('exportPack.contentWorkspace') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentWorkspaceHint') }}</div>
        </div>
        <div class="content-row">
          <a-checkbox v-model="contents.icon">{{ t('exportPack.contentIcon') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentIconHint') }}</div>
        </div>
        <div class="content-row">
          <a-checkbox v-model="contents.extra_files">{{ t('exportPack.contentExtra') }}</a-checkbox>
          <div class="content-hint">{{ t('exportPack.contentExtraHint') }}</div>
        </div>
      </div>

      <div class="export-actions">
        <a-button
          type="primary"
          size="large"
          :loading="busy"
          @click="startExport"
        >
          {{ t('exportPack.start') }}
        </a-button>
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

.content-hint {
  margin: 2px 0 0 26px;
  font-size: 12px;
  color: var(--color-text-3);
}

.export-actions {
  display: flex;
  justify-content: center;
  margin-top: 24px;
}
</style>
