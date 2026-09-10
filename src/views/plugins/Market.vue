<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import { injectDownloadScroll } from '@/composables/download-scroll'
import { api } from '@/api'
import type { MarketPlugin, PluginSource } from '@/api/types'

// keep-alive name: the download page caches this view (search/scroll state).
defineOptions({ name: 'MarketPage' })

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()
const scrollAccessor = injectDownloadScroll()

const search = ref(store.pluginMarketSearch)
const error = ref<string | null>(null)
/** '' means "all sources". */
const sourceFilter = ref<'' | PluginSource>(store.pluginMarketSource)

// Persist the search box + source filter in the store so they survive a trip
// into the install wizard and back (issue #29).
watch([search, sourceFilter], ([q, src]) => {
  store.pluginMarketSearch = q
  store.pluginMarketSource = src
})

function pickDescription(p: MarketPlugin): string {
  const d = p.description
  if (!d) return ''
  if (typeof d === 'string') return d
  const zh = d.find((x) => x.language.startsWith('zh'))
  return (zh ?? d[0])?.content ?? ''
}

/** Source of an entry; absent on old payloads means the primary catalog. */
function sourceOf(p: MarketPlugin): PluginSource {
  return p.source ?? 'dsh-plugins'
}

const filtered = computed(() => {
  let list = store.marketPlugins
  if (sourceFilter.value) {
    list = list.filter((p) => sourceOf(p) === sourceFilter.value)
  }
  const q = search.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((p) => {
    const desc = pickDescription(p).toLowerCase()
    return p.id.toLowerCase().includes(q) || p.name.toLowerCase().includes(q) || desc.includes(q)
  })
})

const hasSupportBadge = (p: MarketPlugin) => {
  const sv = p.support_versions
  return sv !== undefined && sv !== null && sv !== ''
}

function supportText(p: MarketPlugin): string {
  const sv = p.support_versions
  if (typeof sv === 'string') return sv
  if (typeof sv === 'number') return String(sv)
  return ''
}

function relationships(p: MarketPlugin) {
  return p.relationship ?? []
}

function choose(p: MarketPlugin) {
  store.pluginWizard = { plugin: p, channel: 'stable', version: null }
  router.push({ name: 'plugin-version' })
}

async function load() {
  error.value = null
  try {
    await store.refreshMarketPlugins(true)
  } catch (e) {
    error.value = String(e)
  }
}

onMounted(() => {
  if (store.marketPlugins.length === 0) load()
  // Restore the saved scroll offset once the list has re-rendered.
  nextTick(() => {
    if (store.pluginMarketScrollTop > 0) {
      scrollAccessor?.(store.pluginMarketScrollTop)
    }
  })
})

onBeforeUnmount(() => {
  // Remember where the user was scrolled for the next visit.
  const top = (scrollAccessor?.(null) as number) ?? 0
  store.pluginMarketScrollTop = top
})
</script>

<template>
  <div class="plugin-market">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('plugins.title') }}</h3>
        <a-space>
          <a-select v-model="sourceFilter" class="source-select" size="small">
            <a-option value="">{{ t('plugins.sourceAll') }}</a-option>
            <a-option value="dsh-plugins">dsh-plugins</a-option>
            <a-option value="awesome-dsh-plugin">awesome-dsh-plugin</a-option>
          </a-select>
          <a-input
            v-model="search"
            :placeholder="t('plugins.searchPlaceholder')"
            allow-clear
            class="search-input"
          />
          <a-button size="small" type="text" :loading="store.marketLoading" @click="load">
            {{ t('plugins.refresh') }}
          </a-button>
        </a-space>
      </div>

      <div v-if="error" class="market-error">
        <a-alert :title="t('plugins.marketError')" type="error">
          <template #action>
            <a-button size="mini" @click="load">{{ t('plugins.retry') }}</a-button>
          </template>
        </a-alert>
      </div>

      <div v-if="!error && store.marketLoading" class="market-loading">
        <a-spin />
      </div>

      <template v-else-if="!error">
        <div v-if="filtered.length === 0" class="market-empty">
          <a-empty :description="search ? t('plugins.noMatch') : t('plugins.empty')" />
        </div>
        <div
          v-for="p in filtered"
          :key="p.id"
          class="plugin-row"
          @click="choose(p)"
        >
          <span class="plugin-icon">🧩</span>
          <div class="plugin-meta">
            <div class="plugin-name">
              {{ p.name }}
              <span class="plugin-id">{{ p.id }}</span>
              <a-tag v-if="sourceOf(p) === 'awesome-dsh-plugin'" size="small" color="purple">
                awesome
              </a-tag>
            </div>
            <div class="plugin-desc">{{ pickDescription(p) }}</div>
            <div v-if="relationships(p).length" class="plugin-rel">
              <a-tag
                v-for="r in relationships(p)"
                :key="r.kind + r.id"
                size="small"
                :color="r.kind === 'dependency' ? 'blue' : 'red'"
              >
                {{ r.kind === 'dependency' ? t('plugins.dependency') : t('plugins.incompatibility') }}: {{ r.id }} {{ r.versions }}
              </a-tag>
            </div>
          </div>
          <div class="plugin-side">
            <a-tag v-if="hasSupportBadge(p)" size="small">{{ supportText(p) }}</a-tag>
            <span class="version-arrow">›</span>
          </div>
        </div>
      </template>
    </div>
  </div>
</template>

<style lang="scss" scoped>
.plugin-market {
  max-width: 860px;
  margin: 0 auto;
}

.search-input {
  width: 260px;
}

.source-select {
  width: 190px;
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

.plugin-row {
  display: flex;
  align-items: flex-start;
  gap: 14px;
  padding: 12px;
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.15s;

  &:hover {
    background: var(--color-fill-2);
  }
}

.plugin-icon {
  width: 40px;
  height: 40px;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #0fc6c2, #165dff);
  font-size: 18px;
  flex-shrink: 0;
}

.plugin-meta {
  flex: 1;
  min-width: 0;
}

.plugin-name {
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.plugin-id {
  font-size: 12px;
  font-weight: 400;
  color: var(--color-text-3);
}

.plugin-desc {
  font-size: 13px;
  color: var(--color-text-2);
  margin-top: 2px;
  overflow: hidden;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}

.plugin-rel {
  margin-top: 6px;
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.plugin-side {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}

.version-arrow {
  color: var(--color-text-3);
  font-size: 20px;
}
</style>
