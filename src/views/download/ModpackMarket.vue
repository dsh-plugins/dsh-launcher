<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import type { MarketModpack } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import ModpackImportDialog from '@/components/ModpackImportDialog.vue'

// keep-alive name: the download page caches this view (search state).
defineOptions({ name: 'ModpackMarketPage' })

const { t, locale } = useI18n()
const store = useLauncherStore()

/** "如何发布整合包？" documentation link (issue #17). */
const PUBLISH_URL =
  'https://github.com/DSH-PackForge/dsh-pack-market#%E5%A6%82%E4%BD%95%E5%8F%91%E5%B8%83%E8%AE%A9%E6%95%B4%E5%90%88%E5%8C%85%E8%A2%AB%E6%94%B6%E5%BD%95'

const search = ref(store.modpackMarketSearch)
const error = ref<string | null>(null)
/** '' means "all pack types". */
const typeFilter = ref<'' | 'profile' | 'dshhome'>('')

// Import dialog state: installing a pack just prefills the existing import
// dialog with the pack's download URL (auto-loads the manifest).
const importVisible = ref(false)
const importSource = ref('')

// Persist the search box in the store so it survives navigation (same
// pattern as the plugin market, issue #29).
watch(search, (q) => {
  store.modpackMarketSearch = q
})

/** Localized text: string passthrough, or locale map with fallback. */
function localized(value: string | Record<string, string> | null | undefined): string {
  if (!value) return ''
  if (typeof value === 'string') return value
  return value[locale.value] ?? value['en-US'] ?? Object.values(value)[0] ?? ''
}

function fmtSize(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return ''
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

const filtered = computed(() => {
  let list = store.marketModpacks
  if (typeFilter.value) {
    list = list.filter((p) => (p.type ?? 'profile') === typeFilter.value)
  }
  const q = search.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((p) => {
    const hay = [p.id, p.name, p.author ?? '', localized(p.displayName), localized(p.description)]
      .join(' ')
      .toLowerCase()
    return hay.includes(q)
  })
})

function install(pack: MarketModpack) {
  importSource.value = pack.downloadUrl
  importVisible.value = true
}

async function load() {
  error.value = null
  try {
    await store.refreshModpackMarket(true)
  } catch (e) {
    error.value = String(e)
  }
}

onMounted(() => {
  if (store.marketModpacks.length === 0) load()
})
</script>

<template>
  <div class="modpack-market">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('modpack.marketTitle') }}</h3>
        <a-space>
          <a-link @click="api.openExternal(PUBLISH_URL)">
            {{ t('modpack.marketPublish') }}
          </a-link>
          <a-input
            v-model="search"
            :placeholder="t('modpack.marketSearch')"
            allow-clear
            class="search-input"
          />
          <a-button size="small" type="text" :loading="store.modpackMarketLoading" @click="load">
            {{ t('modpack.marketRefresh') }}
          </a-button>
        </a-space>
      </div>

      <div class="type-filter">
        <a-radio-group v-model="typeFilter" type="button" size="small">
          <a-radio value="">{{ t('modpack.marketTypeAll') }}</a-radio>
          <a-radio value="profile">{{ t('modpack.marketTypeProfile') }}</a-radio>
          <a-radio value="dshhome">{{ t('modpack.marketTypeDshhome') }}</a-radio>
        </a-radio-group>
      </div>

      <div v-if="error" class="market-error">
        <a-alert :title="t('modpack.marketError')" type="error">
          <template #action>
            <a-button size="mini" @click="load">{{ t('modpack.marketRetry') }}</a-button>
          </template>
        </a-alert>
      </div>

      <div v-if="!error && store.modpackMarketLoading" class="market-loading">
        <a-spin />
      </div>

      <template v-else-if="!error">
        <div v-if="filtered.length === 0" class="market-empty">
          <a-empty
            :description="search || typeFilter ? t('modpack.marketNoMatch') : t('modpack.marketEmpty')"
          />
        </div>
        <div v-for="p in filtered" :key="p.id" class="modpack-row">
          <span class="modpack-icon">📦</span>
          <div class="modpack-meta">
            <div class="modpack-name">
              {{ localized(p.displayName) || p.name }}
              <span class="modpack-id">{{ p.id }}</span>
              <a-tag size="small" :color="p.type === 'dshhome' ? 'orangered' : 'arcoblue'">
                {{ p.type === 'dshhome' ? t('modpack.marketTypeDshhome') : t('modpack.marketTypeProfile') }}
              </a-tag>
              <a-tag size="small">{{ p.version }}</a-tag>
            </div>
            <div class="modpack-desc">{{ localized(p.description) }}</div>
            <div class="modpack-tags">
              <a-tag v-if="p.author" size="small" color="gray">{{ p.author }}</a-tag>
              <a-tag v-if="p.dshVersion" size="small" color="gray">
                {{ t('modpack.marketDshVersion', { version: p.dshVersion }) }}
              </a-tag>
              <a-tag v-if="fmtSize(p.size)" size="small" color="gray">{{ fmtSize(p.size) }}</a-tag>
              <a-tag v-if="p.profileCount" size="small" color="gray">
                {{ t('modpack.marketProfiles', { count: p.profileCount }) }}
              </a-tag>
              <a-tag v-if="p.bundleCount" size="small" color="gray">
                {{ t('modpack.marketBundles', { count: p.bundleCount }) }}
              </a-tag>
              <a-tag v-if="p.depCount" size="small" color="gray">
                {{ t('modpack.marketDeps', { count: p.depCount }) }}
              </a-tag>
              <a-tag v-if="p.updatedAt" size="small" color="gray">
                {{ t('modpack.marketUpdated', { date: p.updatedAt }) }}
              </a-tag>
            </div>
          </div>
          <div class="modpack-side">
            <a-button size="small" type="primary" @click="install(p)">
              {{ t('modpack.marketInstall') }}
            </a-button>
          </div>
        </div>
      </template>
    </div>

    <ModpackImportDialog v-model:visible="importVisible" :initial-source="importSource" />
  </div>
</template>

<style lang="scss" scoped>
.modpack-market {
  max-width: 860px;
  margin: 0 auto;
}

.search-input {
  width: 260px;
}

.type-filter {
  margin-bottom: 12px;
}

.market-error {
  margin: 12px 0;
}

.market-loading {
  display: flex;
  justify-content: center;
  padding: 40px 0;
}

.market-empty {
  padding: 20px 0;
}

.modpack-row {
  display: flex;
  align-items: flex-start;
  gap: 14px;
  padding: 12px;
  border-radius: 8px;
}

.modpack-icon {
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #722ed1, #165dff);
  font-size: 18px;
  flex-shrink: 0;
}

.modpack-meta {
  flex: 1;
  min-width: 0;
}

.modpack-name {
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.modpack-id {
  font-size: 12px;
  font-weight: 400;
  color: var(--color-text-3);
}

.modpack-desc {
  font-size: 13px;
  color: var(--color-text-2);
  margin-top: 2px;
  overflow: hidden;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}

.modpack-tags {
  margin-top: 6px;
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.modpack-side {
  display: flex;
  align-items: center;
  flex-shrink: 0;
}
</style>
