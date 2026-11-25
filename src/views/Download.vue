<script setup lang="ts">
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { IconApps, IconLayers, IconPlusCircle, IconRefresh } from '@dsh-plugin/dsh-ui-components-vue/icons'
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import { provideDownloadScroll } from '@/composables/download-scroll'
import PageScrollbar from '@/components/PageScrollbar.vue'

const { message: Message } = useFeedback()

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const scrollbar = ref<InstanceType<typeof PageScrollbar> | null>(null)

/**
 * Read (top === null) or write the shared scrollbar's scrollTop. The scrollable
 * element is .arco-scrollbar-container: the template style (`overflow-y: auto`)
 * is forwarded there via $attrs rather than onto the root .arco-scrollbar.
 */
function scrollAccessor(top: number | null): number | void {
  if (top === null) {
    const el = scrollbar.value?.$el?.querySelector('.arco-scrollbar-container')
    return el ? (el as HTMLElement).scrollTop : 0
  }
  scrollbar.value?.scrollTop(top)
}

provideDownloadScroll(scrollAccessor)

const PLUGIN_ROUTES = new Set(['download-plugins', 'plugin-version', 'plugin-install'])
const MODPACK_ROUTES = new Set(['download-modpacks'])

const selectedKeys = computed(() => {
  const name = route.name as string
  if (PLUGIN_ROUTES.has(name)) return ['plugins']
  if (MODPACK_ROUTES.has(name)) return ['modpacks']
  return ['create']
})

const onCreatePage = computed(() => selectedKeys.value[0] === 'create')

function onMenuSelect(key: string) {
  const target =
    key === 'plugins'
      ? 'download-plugins'
      : key === 'modpacks'
        ? 'download-modpacks'
        : 'download-create'
  router.push({ name: target })
}

function onRefreshVersions() {
  store.refreshRemoteVersions().catch((error) => Message.error(String(error)))
}

// --- Scroll memory -------------------------------------------------------------
// The scrollbar wraps <router-view> (shared by all download children), so
// keep-alive alone cannot restore each child's scroll position. Track the
// scroller's scrollTop continuously per route and restore it on return
// (the market page additionally persists its own offset via the composable).

const scrollMem = new Map<string, number>()

function scroller(): HTMLElement | null {
  return (scrollbar.value?.$el?.querySelector('.arco-scrollbar-container') as HTMLElement) ?? null
}

function onScroll() {
  if (typeof route.name === 'string') {
    scrollMem.set(route.name, scroller()?.scrollTop ?? 0)
  }
}

watch(
  () => route.name,
  async (name) => {
    if (typeof name !== 'string') return
    await nextTick()
    const el = scroller()
    const top = scrollMem.get(name)
    if (el && top != null) el.scrollTop = top
  },
  { flush: 'post', immediate: true },
)

onMounted(() => scroller()?.addEventListener('scroll', onScroll, { passive: true }))
onUnmounted(() => scroller()?.removeEventListener('scroll', onScroll))
</script>

<template>
  <div class="download-page dl-split-page">
    <aside class="download-sidebar dl-sidebar">
      <dsh-menu :selected-keys="selectedKeys" @menu-item-click="onMenuSelect">
        <dsh-menu-item key="create" style="padding-inline-end: 6px">
          <template #icon><IconPlusCircle aria-hidden="true" /></template>
          {{ t('download.createInstance') }}
          <template #action>
            <dsh-button variant="ghost"
              size="sm"
              icon-only
              class="refresh-btn"
              :class="{ 'refresh-hidden': !onCreatePage }"
              :aria-label="t('common.refresh')"
              :loading="store.remoteLoading"
              @click="onRefreshVersions"
            >
              <template #icon><IconRefresh aria-hidden="true" /></template>
            </dsh-button>
          </template>
        </dsh-menu-item>
        <dsh-menu-item key="plugins">
          <template #icon><IconApps aria-hidden="true" /></template>
          {{ t('download.plugins') }}
        </dsh-menu-item>
        <dsh-menu-item key="modpacks">
          <template #icon><IconLayers aria-hidden="true" /></template>
          {{ t('download.modpacks') }}
        </dsh-menu-item>
      </dsh-menu>
    </aside>
    <section class="download-content dl-page-content">
      <PageScrollbar ref="scrollbar">
        <div class="download-inner">
          <router-view v-slot="{ Component }">
            <keep-alive :include="['MarketPage', 'CreatePickPage', 'ModpackMarketPage']">
              <component :is="Component" />
            </keep-alive>
          </router-view>
        </div>
      </PageScrollbar>
    </section>
  </div>
</template>

<style lang="scss" scoped>
.download-page {
  display: flex;
  height: var(--dl-content-height);
}

.download-sidebar {
  width: 200px;
  flex-shrink: 0;
}

.refresh-hidden {
  visibility: hidden;
}

.download-sidebar .refresh-btn.dsh-button {
  --button-fg: var(--accent-foreground);
  width: 24px;
  height: 24px;
  flex-shrink: 0;
  padding: 0;
  gap: 0;
  font-size: 14px;
  color: var(--accent-foreground);

  :deep(.arco-btn-icon) {
    display: flex;
    margin: 0;
  }
}

.download-content {
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.download-inner {
  padding: 20px 20px 60px 0;
}
</style>
