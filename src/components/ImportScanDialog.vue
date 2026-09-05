<script setup lang="ts">
// Import-wizard dialog (issue #31, problem 1): scans the machine for DSH
// environments the launcher did not install itself — `~/.dsh*` homes
// (including `DSH_HOME` and WSL distros), plus user-picked local version
// directories — and imports the user's selection as homes / versions /
// instances. Idempotent: already-known entries are skipped and reported.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { ImportReport, ScanReport, ScannedVersion } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'

const props = defineProps<{
  visible: boolean
}>()

const emit = defineEmits<{
  'update:visible': [boolean]
  imported: []
}>()

const { t } = useI18n()
const store = useLauncherStore()

const scanning = ref(false)
const importing = ref(false)
const report = ref<ScanReport | null>(null)
/** Selected profile key per home: `${homePath}::${profileName}`. */
const checkedProfiles = ref<string[]>([])
const versions = ref<ScannedVersion[]>([])
const newVersionDir = ref('')
const importResult = ref<ImportReport | null>(null)

// --- scan --------------------------------------------------------------------

async function runScan() {
  scanning.value = true
  importResult.value = null
  try {
    report.value = await api.scanLocalDsh()
    // Default-select web/tui profiles of unknown homes (skip known ones).
    const defaults: string[] = []
    for (const home of report.value.homes) {
      if (home.alreadyKnown) continue
      for (const p of home.profiles) {
        if (p.kind !== 'other') defaults.push(`${home.path}::${p.name}`)
      }
    }
    checkedProfiles.value = defaults
  } catch (e) {
    Message.error(String(e))
  } finally {
    scanning.value = false
  }
}

watch(
  () => props.visible,
  (v) => {
    if (v) void runScan()
  },
)

// --- versions ----------------------------------------------------------------

async function addVersion() {
  const dir = newVersionDir.value.trim()
  if (!dir) return
  try {
    const v = await api.validateLocalVersion(dir)
    if (versions.value.some((x) => x.dir.toLowerCase() === v.dir.toLowerCase())) {
      Message.warning(t('importScan.versionAlreadyAdded'))
      return
    }
    versions.value.push(v)
    newVersionDir.value = ''
  } catch (e) {
    Message.error(String(e))
  }
}

// --- import ------------------------------------------------------------------

const hasSelection = computed(
  () => checkedProfiles.value.length > 0 || versions.value.length > 0,
)

function profileKey(homePath: string, profile: string): string {
  return `${homePath}::${profile}`
}

async function doImport() {
  if (!report.value || !hasSelection.value) return
  importing.value = true
  try {
    const homes = report.value.homes
      .map((home) => ({
        path: home.path,
        wsl: home.wsl,
        profiles: home.profiles
          .filter((p) => checkedProfiles.value.includes(profileKey(home.path, p.name)))
          .map((p) => p.name),
      }))
      .filter((h) => h.profiles.length > 0)
    const result = await api.importScanned({
      homes,
      versions: versions.value.map((v) => ({ dir: v.dir })),
    })
    importResult.value = result
    Message.success(t('importScan.done', { instances: result.instancesAdded }))
    emit('imported')
    // Reload the store so new instances appear immediately.
    await store.init()
  } catch (e) {
    Message.error(String(e))
  } finally {
    importing.value = false
  }
}

function close() {
  emit('update:visible', false)
}
</script>

<template>
  <a-modal
    :visible="props.visible"
    :title="t('importScan.title')"
    :width="640"
    :footer="false"
    unmount-on-close
    @cancel="close"
  >
    <a-spin :loading="scanning" style="width: 100%">
      <div v-if="report" class="scan-body">
        <p class="scan-hint">{{ t('importScan.hint') }}</p>

        <div v-if="report.homes.length === 0" class="scan-empty">
          {{ t('importScan.nothingFound') }}
        </div>

        <div v-for="home in report.homes" :key="home.path" class="home-card">
          <div class="home-head">
            <span class="home-path">{{ home.path }}</span>
            <a-tag v-if="home.wsl" size="small" color="orange">WSL: {{ home.wsl }}</a-tag>
            <a-tag v-if="home.alreadyKnown" size="small" color="gray">
              {{ t('importScan.known') }}
            </a-tag>
          </div>
          <a-checkbox-group v-model="checkedProfiles" direction="vertical">
            <a-checkbox
              v-for="p in home.profiles"
              :key="p.name"
              :value="profileKey(home.path, p.name)"
              :disabled="home.alreadyKnown"
            >
              {{ p.name }}
              <a-tag v-if="p.kind === 'tui'" size="small" color="purple">TUI</a-tag>
              <a-tag v-else-if="p.kind === 'web'" size="small" color="green">Web</a-tag>
            </a-checkbox>
          </a-checkbox-group>
        </div>

        <div class="versions-block">
          <div class="block-title">{{ t('importScan.versions') }}</div>
          <a-input-search
            v-model="newVersionDir"
            :placeholder="t('importScan.versionDirPlaceholder')"
            search-button
            :button-text="t('importScan.addVersion')"
            @search="addVersion"
          />
          <div v-for="v in versions" :key="v.dir" class="version-row">
            <span class="version-dir">{{ v.dir }}</span>
            <a-tag size="small" color="blue">v{{ v.version }}</a-tag>
            <a-tag size="small" color="gray">{{ v.layout }}</a-tag>
            <a-tag v-if="!v.ready" size="small" color="red">{{ t('importScan.needsBuild') }}</a-tag>
          </div>
        </div>

        <div v-if="importResult" class="import-result">
          {{ t('importScan.summary', importResult) }}
        </div>

        <div class="dialog-actions">
          <a-button @click="runScan" :loading="scanning">{{ t('importScan.rescan') }}</a-button>
          <a-button
            type="primary"
            :disabled="!hasSelection"
            :loading="importing"
            @click="doImport"
          >
            {{ t('importScan.import') }}
          </a-button>
        </div>
      </div>
    </a-spin>
  </a-modal>
</template>

<style lang="scss" scoped>
.scan-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.scan-hint {
  margin: 0;
  color: var(--color-text-3);
  font-size: 12px;
}

.scan-empty {
  padding: 24px 0;
  text-align: center;
  color: var(--color-text-3);
}

.home-card {
  padding: 10px 12px;
  border: 1px solid var(--color-border-2);
  border-radius: 8px;
}

.home-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.home-path {
  font-weight: 600;
  font-size: 13px;
  word-break: break-all;
}

.versions-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.block-title {
  font-size: 13px;
  font-weight: 600;
}

.version-row {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
}

.version-dir {
  word-break: break-all;
}

.import-result {
  padding: 8px 12px;
  border-radius: 6px;
  background: var(--color-fill-2);
  font-size: 13px;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
