<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import { provideDownloadScroll } from '@/composables/download-scroll'
import type { ScrollbarInstance } from '@arco-design/web-vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const scrollbar = ref<ScrollbarInstance | null>(null)

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

const selectedKeys = computed(() => {
  const name = route.name as string
  if (name === 'download-plugins') return ['plugins']
  return ['create']
})

const onCreatePage = computed(() => selectedKeys.value[0] === 'create')

function onMenuSelect(key: string) {
  router.push({ name: key === 'plugins' ? 'download-plugins' : 'download-create' })
}

function onRefreshVersions() {
  store.refreshRemoteVersions()
}
</script>

<template>
  <div class="download-page">
    <aside class="download-sidebar">
      <a-menu :selected-keys="selectedKeys" @menu-item-click="onMenuSelect">
        <a-menu-item key="create">
          <span class="menu-line">
            {{ t('download.createInstance') }}
            <a-button
              v-if="onCreatePage"
              type="text"
              size="mini"
              class="refresh-btn"
              :loading="store.remoteLoading"
              @click.stop="onRefreshVersions"
            >
              ⟳
            </a-button>
          </span>
        </a-menu-item>
        <a-menu-item key="plugins">{{ t('download.plugins') }}</a-menu-item>
      </a-menu>
    </aside>
    <section class="download-content">
      <a-scrollbar
        ref="scrollbar"
        type="track"
        outer-style="height: 100%"
        style="height: 100%; overflow-y: auto"
      >
        <div class="download-inner">
          <router-view />
        </div>
      </a-scrollbar>
    </section>
  </div>
</template>

<style lang="scss" scoped>
.download-page {
  display: flex;
  height: calc(100vh - var(--dl-header-height));
}

.download-sidebar {
  width: 200px;
  flex-shrink: 0;
  background: var(--color-bg-2);
  border-right: 1px solid var(--color-border-2);

  :deep(.arco-menu) {
    height: 100%;
  }
}

.menu-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  width: 100%;
}

.refresh-btn {
  margin-left: auto;
  padding: 0 4px;
  font-size: 14px;
  color: var(--color-text-3);

  &:hover {
    color: rgb(var(--primary-6));
  }
}

.download-content {
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.download-inner {
  padding: 20px 24px 80px;
}
</style>
