<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import type { RemoteVersion } from '@/api/types'

// keep-alive name: the download page caches this view (list state).
defineOptions({ name: 'CreatePickPage' })

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const remote = computed(() => store.remoteVersions)
const loading = computed(() => store.remoteLoading)

onMounted(() => {
  store.refreshRemoteVersions()
})

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

function usedByCount(versionId: string) {
  return store.instances.filter((i) => i.version_id === versionId).length
}

async function onRemove(id: string, version: string) {
  try {
    await api.removeVersion(id)
    await store.refreshVersions()
    Message.success(t('download.versionDeleted', { version }))
  } catch (e) {
    Message.error(String(e))
  }
}
</script>

<template>
  <div class="create-pick" v-loading="loading">
    <!-- Latest versions (always rendered; empty state when no data) -->
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('download.latest') }}</h3>
        <a-button size="small" type="text" :loading="loading" @click="store.refreshRemoteVersions()">
          {{ t('common.refresh') }}
        </a-button>
      </div>
      <template v-if="latest.length">
        <div
          v-for="row in latest"
          :key="row.v.version"
          class="version-row"
          @click="pick(row.v.version)"
        >
          <span class="version-icon" :class="{ pre: isPrerelease(row.v.version) }">◆</span>
          <div class="version-meta">
            <div class="version-name">
              {{ row.v.version }}
              <a-tag v-if="row.v.source === 'github'" size="small" color="orange">
                {{ t('download.sourceBuildTag') }}
              </a-tag>
              <a-tag v-if="installedSet.has(row.v.version)" size="small" color="green">
                {{ t('download.installedTag') }}
              </a-tag>
            </div>
            <div class="version-sub">
              {{ row.label }}<template v-if="row.v.released_at">，{{ t('download.releasedAt', { date: formatDate(row.v.released_at) }) }}</template>
            </div>
          </div>
          <span class="version-arrow">›</span>
        </div>
      </template>
      <div v-else class="card-empty">{{ loading ? t('common.loading') : t('download.noData') }}</div>
    </div>

    <!-- Grouped version lists -->
    <a-collapse :default-active-key="['stable', 'prerelease']" class="version-groups">
      <a-collapse-item key="stable" :header="t('download.stable')">
        <template v-if="stable.length">
          <div v-for="v in stable" :key="v.version" class="version-row" @click="pick(v.version)">
            <span class="version-icon">◆</span>
            <div class="version-meta">
              <div class="version-name">
                {{ v.version }}
                <a-tag v-if="v.source === 'github'" size="small" color="orange">
                  {{ t('download.sourceBuildTag') }}
                </a-tag>
                <a-tag v-if="installedSet.has(v.version)" size="small" color="green">
                  {{ t('download.installedTag') }}
                </a-tag>
              </div>
              <div class="version-sub">{{ formatDate(v.released_at) }}</div>
            </div>
            <span class="version-arrow">›</span>
          </div>
        </template>
        <div v-else class="card-empty">{{ t('download.noData') }}</div>
      </a-collapse-item>
      <a-collapse-item key="prerelease" :header="t('download.prerelease')">
        <template v-if="prerelease.length">
          <div v-for="v in prerelease" :key="v.version" class="version-row" @click="pick(v.version)">
            <span class="version-icon pre">◆</span>
            <div class="version-meta">
              <div class="version-name">
                {{ v.version }}
                <a-tag v-if="v.source === 'github'" size="small" color="orange">
                  {{ t('download.sourceBuildTag') }}
                </a-tag>
                <a-tag v-if="installedSet.has(v.version)" size="small" color="green">
                  {{ t('download.installedTag') }}
                </a-tag>
              </div>
              <div class="version-sub">{{ formatDate(v.released_at) }}</div>
            </div>
            <span class="version-arrow">›</span>
          </div>
        </template>
        <div v-else class="card-empty">{{ t('download.noData') }}</div>
      </a-collapse-item>
    </a-collapse>

    <!-- Installed versions -->
    <div class="dl-card installed-card">
      <div class="dl-card-title">
        <h3>{{ t('download.installedTitle') }}</h3>
      </div>
      <div v-for="v in store.versions" :key="v.id" class="installed-row">
        <span class="version-icon">◆</span>
        <div class="version-meta">
          <div class="version-name">{{ v.version }}</div>
          <div class="version-sub">{{ v.dir }}</div>
        </div>
        <span class="used-by">{{ t('download.usedBy', { count: usedByCount(v.id) }) }}</span>
        <a-popconfirm
          :content="t('download.confirmDeleteVersion', { version: v.version })"
          @ok="onRemove(v.id, v.version)"
        >
          <a-button size="small" status="danger">{{ t('download.deleteVersion') }}</a-button>
        </a-popconfirm>
      </div>
      <a-empty v-if="store.versions.length === 0" :description="t('download.emptyInstalled')" />
    </div>
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
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #0fc6c2, #165dff);
  color: #fff;
  font-size: 16px;
  flex-shrink: 0;

  &.pre {
    background: linear-gradient(135deg, #f7ba1e, #f53f3f);
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

.version-groups {
  background: var(--color-bg-2);
  border-radius: 8px;
  border: none;
  box-shadow: 0 1px 3px rgb(0 0 0 / 6%);
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
