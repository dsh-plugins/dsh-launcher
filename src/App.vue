<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import { api } from '@/api'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { IconPlayArrow, IconDownload, IconSettings } from '@dsh-plugin/dsh-ui-components-vue/icons'
import ModpackImportDialog from '@/components/ModpackImportDialog.vue'
import PluginFileImportDialog from '@/components/PluginFileImportDialog.vue'
import InstanceLaunchErrorDialog from '@/components/InstanceLaunchErrorDialog.vue'
import PageScrollbar from '@/components/PageScrollbar.vue'

const { message: Message } = useFeedback()

const route = useRoute()
const router = useRouter()
const { t, locale } = useI18n()
const store = useLauncherStore()
const isTauri = api.isTauri

onMounted(async () => {
  await store.init()
  locale.value = store.settings.locale || 'zh-CN'
  // If Node.js is missing, guide the user to install it before anything else.
  // A managed runtime that exists but cannot work (issue #101: pre-0.2.7
  // extraction dropped exec bits and npm symlinks) routes there too — the
  // system `node` may still run, so `installed` alone does not catch it.
  if (
    (!store.runtime?.node?.installed || store.runtime?.managed_node_broken) &&
    route.name !== 'setup'
  ) {
    router.push({ name: 'setup' })
  }
  await setupModpackEntryPoints()
})

// --- Modpack entry points: .dspack/.tgz drag-drop + dsh-launcher://pack?url= ---

const modpackImportVisible = ref(false)
const modpackImportSource = ref('')
const pluginFileVisible = ref(false)
const pluginFilePath = ref('')
let unlistenDrag: (() => void) | undefined
let unlistenDeepLink: (() => void) | undefined

function openModpackImport(source: string) {
  modpackImportSource.value = source
  modpackImportVisible.value = true
}

function openPluginFileImport(path: string) {
  pluginFilePath.value = path
  pluginFileVisible.value = true
}

/** A dropped .dspack is always a modpack; a .tgz may be a legacy modpack or
 * a plugin tarball — probe the manifest and route accordingly. */
async function handleDroppedPack(path: string) {
  if (/\.dspack$/i.test(path)) {
    openModpackImport(path)
    return
  }
  try {
    await api.readModpackManifest(path)
    openModpackImport(path)
  } catch {
    openPluginFileImport(path)
  }
}

/** Instance id when the drop happened on an instance editor page. */
const dropInstanceId = computed(() =>
  route.name === 'instance-edit' ? String(route.params.id ?? '') : undefined,
)

async function setupModpackEntryPoints() {
  if (!isTauri) return
  // Cold start via protocol: the deep link arrived in argv before the
  // webview could listen; pull it now.
  const pending = await api.pendingDeepLink()
  if (pending) handleDeepLink(pending)
  const { getCurrentWebview } = await import('@tauri-apps/api/webview')
  unlistenDrag = await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type !== 'drop') return
    const path = event.payload.paths.find((p) => /\.(dspack|tgz)$/i.test(p))
    if (path) void handleDroppedPack(path)
  })
  const { listen } = await import('@tauri-apps/api/event')
  unlistenDeepLink = await listen<string>('deep-link', (event) => handleDeepLink(event.payload))
}

/** dsh-launcher://pack?url=<tgz> → modpack import; dsh-launcher://launch → start instance. */
function handleDeepLink(raw: string) {
  try {
    const u = new URL(raw)
    if (u.protocol !== 'dsh-launcher:') return
    if (u.host === 'pack') {
      const packUrl = u.searchParams.get('url')
      if (packUrl) openModpackImport(packUrl)
    } else if (u.host === 'launch') {
      void launchFromDeepLink(u)
    }
  } catch {
    // Not a URL we understand; ignore.
  }
}

/** dsh-launcher://launch?instance=<name|id>&profile=<name> (issue #9). */
async function launchFromDeepLink(u: URL) {
  const ref = u.searchParams.get('instance')
  if (!ref) return
  const inst = store.instances.find((i) => i.id === ref || i.name === ref)
  if (!inst) {
    Message.error(t('modpackLaunch.instanceNotFound', { name: ref }))
    return
  }
  // Early-loading window: covers the whole shortcut launch until the DSH
  // window is up (the launcher main window stays hidden for launch links).
  earlyLoadingLaunches.add(inst.id)
  await api.openEarlyLoading(inst.id).catch(() => {})
  try {
    const state = store.statusOf(inst.id).state
    if (state !== 'running' && state !== 'starting') {
      const profile = u.searchParams.get('profile') || inst.default_profile || 'web'
      await api.reportLaunchStage(inst.id, 'spawning')
      await api.startInstance(inst.id, profile)
    }
    await api.reportLaunchStage(inst.id, 'waiting-ready')
    // start_instance returns right after spawn; the web URL (and the window
    // command's readiness check) only exist once the instance is running.
    await store.openWindowWhenReady(inst.id)
    await api.reportLaunchStage(inst.id, 'done')
  } catch (e) {
    // A cancelled launch is refused by the backend; the early-loading window
    // already shows the cancelled state, so stay quiet here.
    if (String(e).includes('启动已取消')) return
    void api.reportLaunchStage(inst.id, 'failed', undefined, String(e))
    Message.error(String(e))
  } finally {
    earlyLoadingLaunches.delete(inst.id)
    // The window closes itself on done/cancelled; close defensively here so
    // an error or an already-running instance never strands it.
    void api.closeEarlyLoading(inst.id)
  }
}

/** Ids with an in-flight deep-link launch (drives the running watcher). */
const earlyLoadingLaunches = new Set<string>()

watch(
  () => store.statusById,
  (statuses) => {
    for (const id of earlyLoadingLaunches) {
      const st = statuses[id]
      if (st?.state === 'running' && st.url) {
        void api.reportLaunchStage(id, 'opening-window')
      }
    }
  },
  { deep: true },
)

onUnmounted(() => {
  unlistenDrag?.()
  unlistenDeepLink?.()
})

watch(
  () => store.settings.locale,
  (v) => {
    if (v) locale.value = v
  },
)


const selectedKeys = computed(() => {
  const name = route.name as string
  if (name === 'download' || name?.startsWith('download-')) return ['download']
  if (name === 'settings') return ['settings']
  if (name === 'home') return ['home']
  return []
})

const onTasksPage = computed(() => route.name === 'tasks')

const onInstancePage = computed(() => route.name === 'instances' || route.name === 'instance-edit')

const instancePageTitle = computed(() => {
  if (route.name === 'instances') return t('instances.title')
  if (route.name === 'instance-edit') {
    return route.params.id ? t('instanceEdit.titleEdit') : t('instanceEdit.titleNew')
  }
  return ''
})

function onHeaderBack() {
  router.push({ name: 'home' })
}

function onFabClick() {
  if (onTasksPage.value) {
    router.back()
  } else {
    router.push({ name: 'tasks' })
  }
}

// --- Fly-to-tasks animation ----------------------------------------------------
// Pages that queue a background task without navigating away bump
// store.taskFlyTick; a small square flies from the viewport center to the
// task FAB as visual confirmation.

const taskFabRef = ref<HTMLElement | null>(null)

watch(
  () => store.taskFlyTick,
  (tick) => {
    if (tick <= 0) return
    const fab = taskFabRef.value
    if (!fab) return
    const target = fab.getBoundingClientRect()
    const startX = window.innerWidth / 2
    const startY = window.innerHeight / 2
    const endX = target.left + target.width / 2
    const endY = target.top + target.height / 2

    const square = document.createElement('div')
    square.className = 'task-fly-square'
    square.style.left = `${startX}px`
    square.style.top = `${startY}px`
    document.body.appendChild(square)
    const anim = square.animate(
      [
        { transform: 'translate(-50%, -50%) scale(1)', opacity: 1 },
        {
          transform: `translate(calc(-50% + ${endX - startX}px), calc(-50% + ${endY - startY}px)) scale(0.3)`,
          opacity: 0.6,
        },
      ],
      { duration: 550, easing: 'cubic-bezier(0.3, 0.7, 0.4, 1)', fill: 'forwards' },
    )
    anim.onfinish = () => square.remove()
  },
)

function onMenuSelect(key: string) {
  router.push({ name: key })
}

const appWindow = (() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (window as any)?.__TAURI_INTERNALS__ ? loadWindowApi() : null
})()

const windowMaximized = ref(false)
let unlistenWindowResize: (() => void) | undefined
onMounted(async () => {
  const w = await appWindow
  if (!w) return
  const syncMaximized = async () => { windowMaximized.value = await w.isMaximized() }
  unlistenWindowResize = await w.onResized(syncMaximized)
  await syncMaximized()
})
onUnmounted(() => unlistenWindowResize?.())

async function loadWindowApi() {
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  return getCurrentWindow()
}

async function minimize() {
  const w = await appWindow
  w?.minimize()
}

async function close() {
  const w = await appWindow
  // Delegate to the native close request: the Rust CloseRequested handler
  // honors minimize_to_tray (hide) or lets the window close for real.
  await w?.close()
}

// Manual drag: native data-tauri-drag-region only works on elements carrying
// the attribute, which leaves the menu area in the middle undraggable.
async function onHeaderMouseDown(e: MouseEvent) {
  if (!isTauri || e.button !== 0) return
  const el = e.target as HTMLElement | null
  if (el?.closest('.window-controls, .arco-menu-item, a, button, input, [data-no-drag]')) return
  const w = await appWindow
  w?.startDragging()
}
</script>

<template>
  <!-- Standalone frameless windows render bare, without the app shell. -->
  <router-view v-if="['tui-terminal', 'update-notice', 'early-loading'].includes(route.name as string)" />
  <dsh-layout v-else class="app-shell" :class="{ 'is-maximized': windowMaximized }">
    <dsh-layout-header class="app-header" @mousedown="onHeaderMouseDown">
      <!-- Brand; dragging is handled manually via onHeaderMouseDown. -->
      <div v-if="!onInstancePage" class="app-brand">
        <img src="@/assets/launcher-icon-v3.png" class="app-logo" alt="" />
        <span class="app-title">{{ t('app.title') }}</span>
        <dsh-tag v-if="!isTauri" size="small" color="orange" class="app-preview-badge" nowrap :title="t('app.mockBadge')">{{ t('app.mockBadge') }}</dsh-tag>
      </div>
      <template v-if="onInstancePage">
        <div class="header-back">
          <button class="header-back-btn" @click="onHeaderBack">←</button>
          <span class="header-back-title">{{ instancePageTitle }}</span>
        </div>
      </template>
      <dsh-menu
        v-else
        mode="horizontal"
        :selected-keys="selectedKeys"
        class="app-menu"
        @menu-item-click="onMenuSelect"
      >
        <dsh-menu-item key="home">
          <template #icon><IconPlayArrow aria-hidden="true" /></template>
          {{ t('nav.home') }}
        </dsh-menu-item>
        <dsh-menu-item key="download">
          <template #icon><IconDownload aria-hidden="true" /></template>
          {{ t('nav.download') }}
        </dsh-menu-item>
        <dsh-menu-item key="settings">
          <template #icon><IconSettings aria-hidden="true" /></template>
          {{ t('nav.settings') }}
        </dsh-menu-item>
      </dsh-menu>
      <div class="window-controls">
        <button class="wc-btn" title="最小化" @click="minimize">
          <svg viewBox="0 0 12 12" width="12" height="12"><line x1="1" y1="6" x2="11" y2="6" stroke="currentColor" stroke-width="1.4"/></svg>
        </button>
        <button class="wc-btn wc-close" title="关闭" @click="close">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M1 1 L11 11 M11 1 L1 11" stroke="currentColor" stroke-width="1.4"/></svg>
        </button>
      </div>
    </dsh-layout-header>
    <dsh-layout-content class="app-content">
      <PageScrollbar>
        <router-view />
      </PageScrollbar>
    </dsh-layout-content>

    <!-- Floating task manager entry (bottom-right); becomes a back button on the tasks page. -->
    <div ref="taskFabRef" class="task-fab" @click="onFabClick">
      <dsh-badge v-if="!onTasksPage" :count="store.runningTaskCount" :max-count="99">
        <span class="task-fab-icon">⏱</span>
      </dsh-badge>
      <span v-else class="task-fab-icon">←</span>
      <span class="task-fab-text">{{ onTasksPage ? t('download.back') : t('tasks.fab') }}</span>
    </div>

    <ModpackImportDialog v-model:visible="modpackImportVisible" :initial-source="modpackImportSource" />
    <PluginFileImportDialog
      v-model:visible="pluginFileVisible"
      :file-path="pluginFilePath"
      :initial-instance-id="dropInstanceId"
    />
    <InstanceLaunchErrorDialog />
  </dsh-layout>
</template>
<style lang="scss" scoped>
.app-shell {
  height: 100%;
}

.header-back {
  display: flex;
  align-items: center;
  gap: 12px;
  // Instance pages have no brand/menu: span the first two grid columns.
  grid-column: 1 / 3;
  justify-self: start;
  min-width: 0;
  height: 100%;
}

.header-back-btn {
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 16px;
  color: var(--color-text-2);
  background: transparent;
  border: none;
  border-radius: 6px;
  cursor: pointer;
  transition:
    background 0.15s,
    color 0.15s;

  &:hover {
    background: var(--color-fill-2);
    color: rgb(var(--primary-6));
  }
}

.header-back-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.app-content {
  flex: 1;
  min-height: 0;
  overflow: hidden;
}

.app-header {
  // Three-column grid: brand left, nav menu exactly centered, window controls
  // right. Deterministic regardless of Arco's menu-internal styles (issue
  // #13 — flex/absolute variants all lost specificity fights or collapsed).
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
  align-items: center;
  flex-shrink: 0;
  height: var(--dl-header-height);
  padding: 0 20px;
  background: var(--dl-glass-background);
  border-bottom: 1px solid var(--dl-glass-border);
  -webkit-backdrop-filter: var(--dl-glass-blur);
  backdrop-filter: var(--dl-glass-blur);
  position: relative;
}

.app-brand {
  display: flex;
  align-items: center;
  gap: 8px;
  grid-column: 1;
  justify-self: start;
  min-width: 0;
  max-width: 100%;
  white-space: nowrap;
  height: 100%;
  cursor: default;

  .app-logo {
    flex-shrink: 0;
    width: 24px;
    height: 24px;
    border-radius: 5px;
    object-fit: cover;
  }

  .app-title {
    flex-shrink: 0;
    font-size: 16px;
    font-weight: 600;
  }
}

.app-preview-badge {
  min-width: 0;
}

.window-controls {
  display: flex;
  align-items: center;
  gap: 6px;
  grid-column: 3;
  justify-self: end;
  margin-right: -8px;
}

.wc-btn {
  width: 32px;
  height: 32px;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: var(--color-text-2);
  cursor: pointer;

  &:hover {
    background: var(--color-fill-2);
    color: var(--color-text-1);
  }
}

.wc-close:hover {
  background: #e81123;
  color: #fff;
}

.app-menu {
  // Middle grid column, centered. Arco's horizontal menu collapses items
  // into a "···" overflow dropdown when its measured width is too small —
  // shrink-wrap widths (auto / max-content) lock it into the collapsed
  // state, so give it a definite width with slack for all items (issue #13).
  grid-column: 2;
  justify-self: center;
  width: 480px;
  max-width: 100%;
  // Exactly header height: Arco's 14px vertical inner padding makes the menu
  // 58px tall, pushing the selected-item indicator 6px past the header's
  // bottom edge.
  height: 100%;
  // Keep transparent so the header's border-bottom shows through below the
  // menu instead of being covered by a menu background.
  background: transparent;
  border-bottom: none;

  :deep(.arco-menu-inner) {
    height: 100%;
    padding-top: 0;
    padding-bottom: 0;
    background: transparent;
    border-bottom: none;
    justify-content: center;
  }

  :deep(.arco-menu-overflow-wrap) {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
  }

  :deep(.arco-menu-item),
  :deep(.arco-menu-pop) {
    width: 112px;
    margin-left: 0;
    text-align: center;
  }

  :deep(.arco-menu-item) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding-inline: 10px;
  }

  :deep(.arco-menu-icon) {
    display: inline-flex;
    align-items: center;
    margin-right: 0;
    font-size: 16px;
  }

  :deep(.arco-menu-icon .arco-icon) {
    margin-right: 0;
  }
}

.task-fab {
  position: fixed;
  right: calc(24px + var(--dl-window-inset));
  bottom: calc(24px + var(--dl-window-inset));
  z-index: 100;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 18px;
  background: var(--color-bg-2);
  border: 1px solid var(--color-border-2);
  border-radius: 24px;
  box-shadow: 0 4px 16px rgb(0 0 0 / 12%);
  cursor: pointer;
  user-select: none;
  transition: transform 0.15s, box-shadow 0.15s;

  &:hover {
    transform: translateY(-2px);
    box-shadow: 0 8px 24px rgb(22 93 255 / 20%);
  }
}

.task-fab-icon {
  font-size: 18px;
}

.task-fab-text {
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-1);
}
</style>

<!-- Global: the fly-to-tasks square is appended to document.body. -->
<style>
.task-fly-square {
  position: fixed;
  width: 16px;
  height: 16px;
  border-radius: 4px;
  background: linear-gradient(135deg, #0fc6c2, #165dff);
  box-shadow: 0 2px 8px rgba(22, 93, 255, 0.4);
  z-index: 3000;
  pointer-events: none;
}
</style>
