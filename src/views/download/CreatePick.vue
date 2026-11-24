<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback, useDialog } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import type { RemoteVersion } from '@/api/types'
import MarketIcon from '@/components/MarketIcon.vue'

// keep-alive name: the download page caches this view (list state).
defineOptions({ name: 'CreatePickPage' })

const { message: Message } = useFeedback()
const { modal: Modal } = useDialog()

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const remote = computed(() => store.remoteVersions)
const loading = computed(() => store.remoteLoading)

const emptyVersionsText = computed(() => t(loading.value && !remote.value.length ? 'common.loading' : 'download.noData'))

function refreshVersions() {
  store.refreshRemoteVersions().catch((error) => Message.error(String(error)))
}

onMounted(refreshVersions)

const collator = new Intl.Collator('en', { numeric: true })

const sortedDesc = computed(() =>
  [...remote.value].sort((a, b) => collator.compare(b.version, a.version)),
)

const isPrerelease = (v: string) => v.includes('-')

const stable = computed(() => sortedDesc.value.filter((v) => !isPrerelease(v.version)))
const prerelease = computed(() => sortedDesc.value.filter((v) => isPrerelease(v.version)))

const latest = computed(() => {
  const rows: { v: RemoteVersion; label: string }[] = []
  if (stable.value[0]) rows.push({ v: stable.value[0], label: t('download.latestStable') })
  if (prerelease.value[0]) rows.push({ v: prerelease.value[0], label: t('download.latestPrerelease') })
  return rows
})

const installedSet = computed(() => new Set(store.versions.map((v) => v.version)))
const sourceBuilds = computed(() => new Set(remote.value.filter((v) => v.source === 'github').map((v) => v.version)))

function versionIcon(version: string) {
  return sourceBuilds.value.has(version) || /-alpha(?:[.\d-]|$)/i.test(version)
    ? store.versionIcons.chat
    : store.versionIcons.platform
}

function formatDate(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString()
}

function pick(version: string) {
  router.push({ name: 'download-name', params: { version } })
}

// --- Installed versions management -----------------------------------------

/** Version id whose directory is being deleted; empty when idle. */
const removingId = ref<string | null>(null)
const submittingRepairs = ref(new Set<string>())

function repairBusy(id: string) {
  return submittingRepairs.value.has(id) || store.repairVersionBusy(id)
}

function versionInUse(id: string) {
  return store.instances.some((instance) => instance.version_id === id && (
    ['running', 'starting'].includes(store.statusOf(instance.id).state) || Boolean(store.externalOf(instance.id))
  ))
}

async function onRepair(id: string) {
  if (repairBusy(id) || removingId.value === id || versionInUse(id)) return
  submittingRepairs.value.add(id)
  try {
    await api.startRepairVersionTask(id)
    await store.refreshTasks()
    store.notifyTaskQueued()
    Message.success(t('download.taskAdded'))
  } catch (error) {
    Message.error(String(error))
  } finally {
    submittingRepairs.value.delete(id)
  }
}

function usedByCount(versionId: string) {
  return store.instances.filter((i) => i.version_id === versionId).length
}

async function onRemove(id: string, version: string) {
  // 目录删除在后台线程执行，期间禁用其余删除按钮，避免重复提交。
  if (removingId.value) return false
  removingId.value = id
  try {
    await api.removeVersion(id)
    await store.refreshVersions()
    Message.success(t('download.versionDeleted', { version }))
    return true
  } catch (e) {
    Message.error(String(e))
    return false
  } finally {
    removingId.value = null
  }
}

function confirmRemove(id: string, version: string) {
  Modal.confirm({
    title: t('download.deleteVersion'),
    content: t('download.confirmDeleteVersion', { version }),
    okText: t('download.deleteVersion'),
    cancelText: t('common.cancel'),
    okButtonProps: { status: 'danger' },
    onBeforeOk: () => onRemove(id, version),
  })
}
</script>

<template>
  <div class="create-pick">
    <!-- Latest versions (always rendered; empty state when no data) -->
    <section class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('download.latest') }}</h3>
        <dsh-button variant="ghost" size="sm" :loading="loading" @click="refreshVersions">
          {{ t('common.refresh') }}
        </dsh-button>
      </div>
      <template v-if="latest.length">
        <div
          v-for="row in latest"
          :key="row.v.version"
          class="version-row"
          @click="pick(row.v.version)"
        >
          <MarketIcon :src="versionIcon(row.v.version)" class="version-icon" :class="{ pre: isPrerelease(row.v.version) }">◆</MarketIcon>
          <div class="version-meta">
            <div class="version-name">
              {{ row.v.version }}
              <dsh-tag v-if="row.v.source === 'github'" size="small" color="orange">
                {{ t('download.sourceBuildTag') }}
              </dsh-tag>
              <dsh-tag v-if="installedSet.has(row.v.version)" size="small" color="green">
                {{ t('download.installedTag') }}
              </dsh-tag>
            </div>
            <div class="version-sub">
              {{ row.label }}<template v-if="row.v.released_at">，{{ t('download.releasedAt', { date: formatDate(row.v.released_at) }) }}</template>
            </div>
          </div>
          <span class="version-arrow">›</span>
        </div>
      </template>
      <div v-else class="card-empty">{{ emptyVersionsText }}</div>
    </section>

    <!-- Grouped version lists -->
    <dsh-collapse :default-active-key="['stable', 'prerelease']" class="dl-glass">
      <dsh-collapse-item key="stable" :header="t('download.stable')">
        <template v-if="stable.length">
          <div v-for="v in stable" :key="v.version" class="version-row" @click="pick(v.version)">
            <MarketIcon :src="versionIcon(v.version)" class="version-icon">◆</MarketIcon>
            <div class="version-meta">
              <div class="version-name">
                {{ v.version }}
                <dsh-tag v-if="v.source === 'github'" size="small" color="orange">
                  {{ t('download.sourceBuildTag') }}
                </dsh-tag>
                <dsh-tag v-if="installedSet.has(v.version)" size="small" color="green">
                  {{ t('download.installedTag') }}
                </dsh-tag>
              </div>
              <div class="version-sub">{{ formatDate(v.released_at) }}</div>
            </div>
            <span class="version-arrow">›</span>
          </div>
        </template>
        <div v-else class="card-empty">{{ emptyVersionsText }}</div>
      </dsh-collapse-item>
      <dsh-collapse-item key="prerelease" :header="t('download.prerelease')">
        <template v-if="prerelease.length">
          <div v-for="v in prerelease" :key="v.version" class="version-row" @click="pick(v.version)">
            <MarketIcon :src="versionIcon(v.version)" class="version-icon pre">◆</MarketIcon>
            <div class="version-meta">
              <div class="version-name">
                {{ v.version }}
                <dsh-tag v-if="v.source === 'github'" size="small" color="orange">
                  {{ t('download.sourceBuildTag') }}
                </dsh-tag>
                <dsh-tag v-if="installedSet.has(v.version)" size="small" color="green">
                  {{ t('download.installedTag') }}
                </dsh-tag>
              </div>
              <div class="version-sub">{{ formatDate(v.released_at) }}</div>
            </div>
            <span class="version-arrow">›</span>
          </div>
        </template>
        <div v-else class="card-empty">{{ emptyVersionsText }}</div>
      </dsh-collapse-item>
    </dsh-collapse>

    <!-- Installed versions -->
    <section class="dl-card installed-card">
      <div class="dl-card-title">
        <h3>{{ t('download.installedTitle') }}</h3>
      </div>
      <div v-for="v in store.versions" :key="v.id" class="installed-row">
        <MarketIcon :src="versionIcon(v.version)" class="version-icon">◆</MarketIcon>
        <div class="version-meta">
          <div class="version-name">{{ v.version }}</div>
          <div class="version-sub">{{ v.dir }}</div>
        </div>
        <span class="used-by">{{ t('download.usedBy', { count: usedByCount(v.id) }) }}</span>
        <dsh-button
          variant="secondary"
          size="sm"
          :title="t('download.repairVersionHint')"
          :loading="repairBusy(v.id)"
          :disabled="removingId === v.id || repairBusy(v.id) || versionInUse(v.id)"
          @click="onRepair(v.id)"
        >
          {{ t('download.repairVersion') }}
        </dsh-button>
        <dsh-button variant="danger"
          size="sm"
          :loading="removingId === v.id"
          :disabled="removingId !== null || repairBusy(v.id)"
          @click="confirmRemove(v.id, v.version)"
        >
          {{ t('download.deleteVersion') }}
        </dsh-button>
      </div>
      <dsh-empty v-if="store.versions.length === 0" :description="t('download.emptyInstalled')" />
    </section>
  </div>
</template>

<style lang="scss" scoped>
.create-pick {
  max-width: 860px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.version-row {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.15s;

  &:hover {
    background: var(--color-fill-2);
  }
}

.version-icon {
  color: #fff;
  font-size: 16px;

  &.pre {
    --market-icon-fallback: linear-gradient(135deg, #f7ba1e, #f53f3f);
  }
}

.version-meta {
  flex: 1;
  min-width: 0;
}

.version-name {
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}

.version-sub {
  font-size: 12px;
  color: var(--color-text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.version-arrow {
  color: var(--color-text-3);
  font-size: 20px;
}

.installed-card {
  margin-top: 0;
}

.installed-row {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 12px;
  border-radius: 8px;

  &:hover {
    background: var(--color-fill-2);
  }
}

.used-by {
  color: var(--color-text-3);
  font-size: 12px;
  margin-right: 8px;
  white-space: nowrap;
}

.card-empty {
  padding: 18px 12px;
  color: var(--color-text-3);
  font-size: 13px;
}
</style>
