<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import type { MarketPlugin, PluginChannel, PluginVersionInfo } from '@/api/types'

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const plugin = computed<MarketPlugin | null>(() => store.pluginWizard?.plugin ?? null)

/** GitHub repo hint for sources (live/unverified) absent from the static catalog. */
function repoHint(): string | undefined {
  return plugin.value?.repo ?? plugin.value?.urls?.repository ?? undefined
}

const channelMeta: Record<PluginChannel, { letter: string; color: string }> = {
  stable: { letter: 'R', color: 'green' },
  beta: { letter: 'B', color: 'orange' },
  alpha: { letter: 'A', color: 'red' },
}

// Accumulated versions per channel (pages are appended for alpha); the UI
// merges all channels into one list sorted by publish time.
const versionsByChannel = ref<Record<PluginChannel, PluginVersionInfo[]>>({
  stable: [],
  beta: [],
  alpha: [],
})
// Pagination state per channel (only alpha actually pages).
const hasMore = ref<Record<PluginChannel, boolean>>({ stable: false, beta: false, alpha: false })
const loadingMore = ref<Record<PluginChannel, boolean>>({ stable: false, beta: false, alpha: false })
const pagesLoaded = ref<Record<PluginChannel, number>>({ stable: 1, beta: 1, alpha: 1 })
const loadingChannel = ref<PluginChannel | null>(null)
const error = ref<string | null>(null)

/** All channels merged into one list, newest first by publish time. */
const mergedVersions = computed(() => {
  const all = [
    ...versionsByChannel.value.stable,
    ...versionsByChannel.value.beta,
    ...versionsByChannel.value.alpha,
  ]
  // Entries without a timestamp sort last; ties keep channel order.
  return all.sort((a, b) => (b.published_at ?? '').localeCompare(a.published_at ?? ''))
})

async function loadChannel(ch: PluginChannel) {
  if (!plugin.value || loadingChannel.value === ch) return
  loadingChannel.value = ch
  error.value = null
  try {
    const page = await api.fetchPluginVersions(plugin.value.id, ch, pagesLoaded.value[ch], repoHint())
    versionsByChannel.value[ch] = page.versions
    hasMore.value[ch] = page.has_more
  } catch (e) {
    error.value = String(e)
  } finally {
    loadingChannel.value = null
  }
}

/** Reload every channel from its first page. */
async function reloadAll() {
  pagesLoaded.value = { stable: 1, beta: 1, alpha: 1 }
  await Promise.all([loadChannel('stable'), loadChannel('beta')])
  await loadChannel('alpha')
}

/** Load the next page for a channel, appending to the accumulated list. */
async function loadMore(ch: PluginChannel) {
  if (!plugin.value || loadingMore.value[ch] || loadingChannel.value === ch) return
  if (!hasMore.value[ch]) return
  loadingMore.value[ch] = true
  try {
    const nextPage = pagesLoaded.value[ch] + 1
    const page = await api.fetchPluginVersions(plugin.value.id, ch, nextPage, repoHint())
    versionsByChannel.value[ch] = [...versionsByChannel.value[ch], ...page.versions]
    pagesLoaded.value[ch] = nextPage
    hasMore.value[ch] = page.has_more
  } catch (e) {
    error.value = String(e)
  } finally {
    loadingMore.value[ch] = false
  }
}

// --- Infinite scroll: an IntersectionObserver watches the sentinel at the
// bottom of the list and loads the next alpha page when it becomes visible
// (i.e. the user scrolled near the end).

const sentinels = new Map<PluginChannel, HTMLElement>()
let observer: IntersectionObserver | null = null

function ensureObserver() {
  if (observer) return
  observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue
        const ch = (entry.target as HTMLElement).dataset.channel as PluginChannel | undefined
        if (ch) loadMore(ch)
      }
    },
    // Trigger slightly before the element is fully visible.
    { rootMargin: '120px 0px' },
  )
}

function onSentinel(el: unknown, ch: PluginChannel) {
  const node = el as HTMLElement | null
  if (!node) {
    sentinels.delete(ch)
    return
  }
  sentinels.set(ch, node)
  ensureObserver()
  observer?.observe(node)
}

onMounted(async () => {
  if (!plugin.value) {
    router.replace({ name: 'download-plugins' })
    return
  }
  await reloadAll()
})

onBeforeUnmount(() => {
  observer?.disconnect()
  observer = null
  sentinels.clear()
})

function pick(v: PluginVersionInfo) {
  store.pluginWizard = { plugin: plugin.value!, channel: v.channel, version: v }
  router.push({ name: 'plugin-install' })
}

function formatLabel(v: PluginVersionInfo): string {
  return v.label ?? v.version
}

/**
 * 版本号显示：alpha（开发版）是 Git commit 哈希，只显示前 7 位；存储时
 * 依然使用完整的哈希（v.version 原样写入 store.pluginWizard）。
 */
function displayVersion(v: PluginVersionInfo): string {
  if (v.channel === 'alpha' && /^[0-9a-f]{40}$/i.test(v.version)) {
    return v.version.slice(0, 7)
  }
  return v.version
}
</script>

<template>
  <div class="version-pick">
    <a-page-header
      class="pick-header"
      :title="plugin?.name ?? t('plugins.chooseVersion')"
      :sub-title="plugin?.id"
      @back="router.push({ name: 'download-plugins' })"
    />

    <div v-if="error" class="pick-error">
      <a-alert :title="error" type="error" />
    </div>

    <div v-if="!plugin" class="pick-empty">
      <a-empty :description="t('plugins.noMatch')" />
    </div>

    <div v-else class="dl-card channel-card">
      <div class="dl-card-title">
        <h3>
          {{ t('plugins.chooseVersion') }}
          <a-tag v-if="hasMore.alpha" size="small" color="arcoblue" class="lazy-tag">
            {{ t('plugins.lazyLoad') }}
          </a-tag>
        </h3>
        <a-button
          size="small"
          type="text"
          :loading="loadingChannel !== null"
          @click="reloadAll"
        >
          ⟳
        </a-button>
      </div>

      <template v-if="mergedVersions.length">
        <div
          v-for="v in mergedVersions"
          :key="`${v.channel}:${v.version}`"
          class="version-row"
          @click="pick(v)"
        >
          <span
            class="version-icon"
            :style="{ background: channelMeta[v.channel].color }"
          >{{ channelMeta[v.channel].letter }}</span>
          <div class="version-meta">
            <div class="version-name">
              {{ displayVersion(v) }}
              <a-tag size="small" :color="channelMeta[v.channel].color">
                {{ t(`plugins.channel.${v.channel}`) }}
              </a-tag>
              <a-tag v-if="v.is_default" size="small" color="green">
                {{ t('plugins.defaultTag') }}
              </a-tag>
            </div>
            <div class="version-sub">{{ formatLabel(v) }}</div>
          </div>
          <span class="version-arrow">›</span>
        </div>
        <!-- Sentinel for infinite scroll: loads the next alpha page when scrolled into view -->
        <div
          v-if="hasMore.alpha"
          :ref="(el: unknown) => onSentinel(el, 'alpha')"
          data-channel="alpha"
          class="load-more-sentinel"
        >
          <a-spin v-if="loadingMore.alpha" :size="14" />
          <span v-else class="load-more-hint">{{ t('plugins.scrollMore') }}</span>
        </div>
      </template>
      <div v-else class="card-empty">
        <template v-if="loadingChannel">{{ t('common.loading') }}</template>
        <template v-else>{{ t('plugins.noVersions') }}</template>
      </div>
    </div>
  </div>
</template>

<style lang="scss" scoped>
.version-pick {
  max-width: 860px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.pick-header {
  padding: 0;
  background: transparent;
  border-radius: 8px;
}

.pick-error {
  margin-bottom: 4px;
}

.pick-empty {
  padding: 40px 0;
}

.lazy-tag {
  margin-left: 8px;
  vertical-align: 2px;
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
  color: #fff;
  font-size: 16px;
  font-weight: 700;
  flex-shrink: 0;
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

.card-empty {
  padding: 18px 12px;
  color: var(--color-text-3);
  font-size: 13px;
}

.load-more-sentinel {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 12px 0 4px;
  min-height: 36px;
}

.load-more-hint {
  font-size: 12px;
  color: var(--color-text-4);
}
</style>
