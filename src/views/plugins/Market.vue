<script setup lang="ts">
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, toRef, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import { injectDownloadScroll } from '@/composables/download-scroll'
import { api } from '@/api'
import { localizedText, nameValues, npmPluginIconUrl, pluginName, rangeParts } from '@/utils/market'
import { usePaginatedList } from '@/composables/paginated-list'
import MarketIcon from '@/components/MarketIcon.vue'
import type { Confidence, MarketPlugin, PluginSource } from '@/api/types'

// keep-alive name: the download page caches this view (search/scroll state).
defineOptions({ name: 'MarketPage' })

const { message: Message } = useFeedback()

const router = useRouter()
const { t, te, locale } = useI18n()
const store = useLauncherStore()
const scrollAccessor = injectDownloadScroll()

const search = ref(store.pluginMarketSearch)
const error = computed(() => store.marketError)
/** '' means "all sources". */
const sourceFilter = ref<'' | PluginSource>(store.pluginMarketSource)

// Persist the search box + source filter in the store so they survive a trip
// into the install wizard and back (issue #29).
watch([search, sourceFilter], ([q, src]) => {
  store.pluginMarketSearch = q
  store.pluginMarketSource = src
})

/**
 * A persisted filter id may point at a source that was deleted or disabled in
 * Settings; drop it so the market does not silently show an empty list. Only
 * validate once the source list has actually loaded.
 */
function validateSourceFilter() {
  if (!sourceFilter.value || !store.pluginSourcesLoadedAt) return
  const ok = store.pluginSources.some((s) => s.id === sourceFilter.value && s.enabled)
  if (!ok) sourceFilter.value = ''
}

watch(() => store.pluginSources, validateSourceFilter)

function pickDescription(p: MarketPlugin): string {
  const d = p.description
  if (!d) return ''
  if (typeof d === 'string') return d
  const language = locale.value.toLowerCase()
  const localized = d.find((x) => x.language.toLowerCase() === language)
    ?? d.find((x) => x.language.toLowerCase() === language.split('-')[0])
  return (localized ?? d[0])?.content ?? ''
}

/** Source of an entry; absent on old payloads means the primary catalog. */
function sourceOf(p: MarketPlugin): PluginSource {
  return p.source ?? 'dsh-plugins'
}

/** Display name for a source id (known ids are translated, custom ids as-is). */
function sourceLabel(id: string): string {
  const key = `plugins.source.${id}`
  return te(key) ? t(key) : id
}

const CONFIDENCE_COLORS: Record<Confidence, string> = {
  official: 'green',
  curated: 'purple',
  aggregated: 'blue',
  unverified: 'orangered',
}

/** A missing confidence is unknown and must be shown as unverified. */
function confidenceOf(p: MarketPlugin): Confidence {
  return p.confidence ?? 'unverified'
}

/** Only enabled sources are selectable; a disabled one would empty the list. */
const enabledSources = computed(() => store.pluginSources.filter((s) => s.enabled))

const filtered = computed(() => {
  let list = store.marketPlugins
  if (sourceFilter.value) {
    list = list.filter((p) => sourceOf(p) === sourceFilter.value)
  }
  const q = search.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((p) => {
    const desc = pickDescription(p).toLowerCase()
    return p.id.toLowerCase().includes(q)
      || nameValues(p.name).some((name) => name.toLowerCase().includes(q))
      || localizedText(p.catalog?.title, locale.value).toLowerCase().includes(q) || desc.includes(q)
  })
})

const page = toRef(store, 'pluginMarketPage')
const { visible: visiblePlugins, pageSize } = usePaginatedList(filtered, page)
const failedIcons = reactive(new Set<string>())
const pluginRows = computed(() => visiblePlugins.value.map((plugin) => ({
  plugin,
  icon: [plugin.catalog?.icon, npmPluginIconUrl(plugin)]
    .find((url) => url && !failedIcons.has(url)),
})))

watch([page, filtered], () => {
  // Cached markets share the download scrollbar; only move the active one.
  if (router.currentRoute.value.name === 'download-plugins') scrollAccessor?.(0)
}, { flush: 'post' })

function relationships(p: MarketPlugin) {
  return p.relationship ?? []
}

function choose(p: MarketPlugin) {
  store.pluginWizard = { plugin: p, channel: 'stable', version: null }
  router.push({ name: 'plugin-version' })
}

async function load() {
  failedIcons.clear()
  await store.refreshMarketPlugins(true)
}

onMounted(() => {
  store.refreshPluginSources().then(validateSourceFilter).catch((error) => Message.error(String(error)))
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
        <dsh-space wrap>
          <div class="source-select">
            <dsh-select
              v-model="sourceFilter"
              size="small"
              :trigger-props="{
                autoFitPopupWidth: false,
                autoFitPopupMinWidth: true,
                contentClass: 'plugin-source-popup',
                popupStyle: { width: 'max-content', maxWidth: 'min(26rem, calc(100vw - 24px))' },
              }"
            >
              <dsh-option value="">{{ t('plugins.sourceAll') }}</dsh-option>
              <dsh-option v-for="s in enabledSources" :key="s.id" :value="s.id">
                {{ sourceLabel(s.id) }}
              </dsh-option>
            </dsh-select>
          </div>
          <dsh-input
            v-model="search"
            :placeholder="t('plugins.searchPlaceholder')"
            allow-clear
            size="small"
            class="search-input"
          />
          <dsh-button variant="ghost" size="sm" :loading="store.marketLoading" @click="load">
            {{ t('plugins.refresh') }}
          </dsh-button>
        </dsh-space>
      </div>

      <div v-if="error" class="market-error">
        <dsh-alert :title="t('plugins.marketError')" type="error">
          {{ error }}
          <template #action>
            <dsh-button variant="secondary" size="sm" @click="load">{{ t('plugins.retry') }}</dsh-button>
          </template>
        </dsh-alert>
      </div>

      <dsh-alert
        v-for="warning in store.marketWarnings"
        :key="warning.source_id"
        class="market-warning"
        type="warning"
        :title="t('plugins.sourceFetchFailed', { source: sourceLabel(warning.source_id) })"
      >
        <p>{{ warning.error }}</p>
        <p>{{ t(warning.cached ? 'plugins.sourceUsingCache' : 'plugins.sourceUnavailable') }}</p>
      </dsh-alert>

      <div v-if="store.marketLoading" class="market-loading">
        <dsh-spin />
      </div>

      <template v-else>
        <div v-if="filtered.length === 0 && !error" class="market-empty">
          <dsh-empty :description="search ? t('plugins.noMatch') : t('plugins.empty')" />
        </div>
        <div
          v-for="{ plugin: p, icon } in pluginRows"
          :key="p.id"
          class="plugin-row"
          @click="choose(p)"
        >
          <MarketIcon :src="icon" class="plugin-icon" @error="failedIcons.add($event)">🧩</MarketIcon>
          <div class="plugin-meta">
            <div class="plugin-name">
              {{ localizedText(p.catalog?.title, locale) || pluginName(p.name, locale) }}
              <span class="plugin-id">{{ p.id }}</span>
              <dsh-tag v-if="sourceOf(p) !== 'dsh-plugins'" size="small">
                {{ sourceLabel(sourceOf(p)) }}
              </dsh-tag>
              <dsh-tag
                size="small"
                :color="CONFIDENCE_COLORS[confidenceOf(p)]"
              >
                {{ t(`plugins.confidence.${confidenceOf(p)}`) }}
              </dsh-tag>
            </div>
            <div class="plugin-desc">{{ pickDescription(p) }}</div>
            <div v-if="relationships(p).length" class="plugin-rel">
              <dsh-tag
                v-for="r in relationships(p)"
                :key="r.kind + r.id"
                size="small"
                :color="r.kind === 'dependency' ? 'blue' : 'red'"
              >
                {{ r.kind === 'dependency' ? t('plugins.dependency') : t('plugins.incompatibility') }}: {{ r.id }} {{ rangeParts(r.versions).join('; ') }}
              </dsh-tag>
            </div>
          </div>
          <div class="plugin-side">
            <dsh-tag v-for="part in rangeParts(p.support_versions)" :key="part" size="small">{{ part }}</dsh-tag>
            <span class="version-arrow">›</span>
          </div>
        </div>
        <dsh-pagination
          v-model:current="page"
          class="market-pagination"
          :total="filtered.length"
          :page-size="pageSize"
          hide-on-single-page
        />
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

  :deep(.arco-select-view-single) {
    width: 100%;
  }

  :deep(.arco-select-view-single > .arco-select-view-suffix) {
    top: 50%;
    transform: translateY(-50%);
  }
}

:global(.plugin-source-popup .arco-select-option-content) {
  white-space: normal;
  overflow-wrap: anywhere;
}

.market-error,
.market-warning {
  margin: 12px 0;
}

.market-warning p {
  margin: 0;
  overflow-wrap: anywhere;
}

.market-loading {
  display: flex;
  justify-content: center;
  padding: 40px 0;
}

.market-empty {
  padding: 20px 0;
}

.market-pagination {
  display: flex;
  justify-content: center;
  padding: 12px 0;
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
  font-size: 18px;
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
  /* Row uses align-items: flex-start for the meta block; the side (support
     badge + › arrow) must still sit at the row's vertical center (issue #13). */
  align-self: center;
  gap: 10px;
  flex-shrink: 0;
}

.version-arrow {
  color: var(--color-text-3);
  font-size: 20px;
}
</style>
