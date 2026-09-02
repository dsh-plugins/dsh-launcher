<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useLauncherStore } from '@/stores/launcher'
import type { ThemeMode } from '@/api/types'
import { api } from '@/api'
import { Message } from '@arco-design/web-vue'
import ModpackImportDialog from '@/components/ModpackImportDialog.vue'
import PluginFileImportDialog from '@/components/PluginFileImportDialog.vue'

const route = useRoute()
const router = useRouter()
const { t, locale } = useI18n()
const store = useLauncherStore()
const isTauri = api.isTauri

// --- Theme: light / dark / follow system -------------------------------------

const themeMedia = window.matchMedia('(prefers-color-scheme: dark)')

/** Applies the effective Arco theme to <body> (body[arco-theme='dark']). */
function applyTheme(mode: ThemeMode) {
  const dark = mode === 'dark' || (mode === 'system' && themeMedia.matches)
  if (dark) {
    document.body.setAttribute('arco-theme', 'dark')
  } else {
    document.body.removeAttribute('arco-theme')
  }
}

function onSystemThemeChange() {
  if (store.settings.theme === 'system') applyTheme('system')
}

onMounted(async () => {
  await store.init()
  locale.value = store.settings.locale || 'zh-CN'
  // Apply the persisted theme early (before init resolves the settings may
  // still be defaults; the watch below re-applies on any change).
  applyTheme(store.settings.theme || 'system')
  themeMedia.addEventListener('change', onSystemThemeChange)
  // If Node.js is missing, guide the user to install it before anything else.
  if (!store.runtime?.node?.installed && route.name !== 'setup') {
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
  try {
    const state = store.statusOf(inst.id).state
    if (state !== 'running' && state !== 'starting') {
      const profile = u.searchParams.get('profile') || inst.default_profile || 'web'
      await api.startInstance(inst.id, profile)
    }
    // start_instance returns right after spawn; the web URL (and the window
    // command's readiness check) only exist once the instance is running.
    await openWindowWhenReady(inst.id)
  } catch (e) {
    Message.error(String(e))
  }
}

/** Waits for the instance to report `running` with a URL, then opens its window. */
async function openWindowWhenReady(id: string) {
  const deadline = Date.now() + 120_000
  for (;;) {
    const st = store.statusOf(id)
    if (st.state === 'running' && st.url) {
      await api.openInstanceWindow(id)
      return
    }
    if (st.state === 'exited' || Date.now() > deadline) {
      // Last attempt: surface the backend's own error if it is not ready.
      await api.openInstanceWindow(id)
      return
    }
    await new Promise((r) => setTimeout(r, 500))
  }
}

onUnmounted(() => {
  themeMedia.removeEventListener('change', onSystemThemeChange)
  unlistenDrag?.()
  unlistenDeepLink?.()
})

watch(
  () => store.settings.locale,
  (v) => {
    if (v) locale.value = v
  },
)

watch(
  () => store.settings.theme,
  (v) => {
    if (v) applyTheme(v)
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

function onMenuSelect(key: string) {
  router.push({ name: key })
}

const appWindow = (() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (window as any)?.__TAURI_INTERNALS__ ? loadWindowApi() : null
})()

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
  <a-layout class="app-shell">
    <a-layout-header class="app-header" @mousedown="onHeaderMouseDown">
      <!-- Brand; dragging is handled manually via onHeaderMouseDown. -->
      <div v-if="!onInstancePage" class="app-brand">
        <img src="@/assets/launcher-icon.png" class="app-logo" alt="" />
        <span class="app-title">{{ t('app.title') }}</span>
        <a-tag v-if="!isTauri" size="small" color="orange">{{ t('app.mockBadge') }}</a-tag>
      </div>
      <template v-if="onInstancePage">
        <div class="header-back">
          <button class="header-back-btn" @click="onHeaderBack">←</button>
          <span class="header-back-title">{{ instancePageTitle }}</span>
        </div>
      </template>
      <a-menu
        v-else
        mode="horizontal"
        :selected-keys="selectedKeys"
        class="app-menu"
        @menu-item-click="onMenuSelect"
      >
        <a-menu-item key="home">{{ t('nav.home') }}</a-menu-item>
        <a-menu-item key="download">{{ t('nav.download') }}</a-menu-item>
        <a-menu-item key="settings">{{ t('nav.settings') }}</a-menu-item>
      </a-menu>
      <div class="window-controls">
        <button class="wc-btn" title="最小化" @click="minimize">
          <svg viewBox="0 0 12 12" width="12" height="12"><line x1="1" y1="6" x2="11" y2="6" stroke="currentColor" stroke-width="1.4"/></svg>
        </button>
        <button class="wc-btn wc-close" title="关闭" @click="close">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M1 1 L11 11 M11 1 L1 11" stroke="currentColor" stroke-width="1.4"/></svg>
        </button>
      </div>
    </a-layout-header>
    <a-layout-content class="app-content">
      <a-scrollbar
        type="track"
        outer-style="height: 100%"
        style="height: 100%; overflow-y: auto"
      >
        <router-view />
      </a-scrollbar>
    </a-layout-content>

    <!-- Floating task manager entry (bottom-right); becomes a back button on the tasks page. -->
    <div class="task-fab" @click="onFabClick">
      <a-badge v-if="!onTasksPage" :count="store.runningTaskCount" :dot="store.runningTaskCount > 0">
        <span class="task-fab-icon">⏱</span>
      </a-badge>
      <span v-else class="task-fab-icon">←</span>
      <span class="task-fab-text">{{ onTasksPage ? t('download.back') : t('tasks.fab') }}</span>
    </div>

    <ModpackImportDialog v-model:visible="modpackImportVisible" :initial-source="modpackImportSource" />
    <PluginFileImportDialog
      v-model:visible="pluginFileVisible"
      :file-path="pluginFilePath"
      :initial-instance-id="dropInstanceId"
    />
  </a-layout>
</template>

<style lang="scss" scoped>
.app-shell {
  height: 100%;
}

.header-back {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
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
  display: flex;
  align-items: center;
  flex-shrink: 0;
  height: var(--dl-header-height);
  padding: 0 20px;
  background: var(--color-bg-2);
  border-bottom: 1px solid var(--color-border-2);
}

.app-brand {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-right: 32px;
  white-space: nowrap;
  height: 100%;
  cursor: default;

  .app-logo {
    width: 24px;
    height: 24px;
    border-radius: 5px;
    object-fit: cover;
  }

  .app-title {
    font-size: 16px;
    font-weight: 600;
  }
}

.window-controls {
  display: flex;
  align-items: center;
  gap: 2px;
  margin-right: -12px;
}

.wc-btn {
  width: 42px;
  height: var(--dl-header-height);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
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
  flex: 1;
  // Keep transparent so the header's border-bottom shows through below the
  // menu instead of being covered by a menu background.
  background: transparent;
  border-bottom: none;

  :deep(.arco-menu-inner) {
    background: transparent;
    border-bottom: none;
  }
}

.task-fab {
  position: fixed;
  right: 24px;
  bottom: 24px;
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
