<script setup lang="ts">
import { computed, h, nextTick, onBeforeUnmount, onMounted, ref, render, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { HintIcon, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { Marked } from 'marked'
import markedAlert from 'marked-alert'
import markedFootnote from 'marked-footnote'
import markedKatex from 'marked-katex-extension'
import DOMPurify from 'dompurify'
import { api } from '@/api'
import InstanceAccessLinks from '@/components/InstanceAccessLinks.vue'
import type { CompatibilityReport as CompatibilityReportType } from '@/api/types'
import CompatibilityReport from '@/components/CompatibilityReport.vue'
import { useLauncherStore } from '@/stores/launcher'
import { markedSpoiler } from '@/utils/marked-spoiler'
import launcherDefaultIcon from '@/assets/launcher-icon-v3.png'
import PageScrollbar from '@/components/PageScrollbar.vue'
import 'katex/dist/katex.min.css'

// --- Instance icons (issue #8): the launch panel avatar follows the instance --

const { message: Message } = useFeedback()

const iconMap = ref<Record<string, string | null>>({})

async function loadIcons() {
  const next: Record<string, string | null> = {}
  for (const inst of store.instances) {
    if (!inst.icon) {
      next[inst.id] = null
      continue
    }
    try {
      next[inst.id] = await api.readInstanceIcon(inst.id)
    } catch {
      next[inst.id] = null
    }
  }
  iconMap.value = next
}

const marked = new Marked({ gfm: true, breaks: true })
  .use(markedAlert())
  .use(markedFootnote())
  .use(markedKatex({ throwOnError: false }))
  .use(markedSpoiler())

// marked-alert only matches UPPERCASE alert types, while GitHub treats
// `[!note]`/`[!Note]` the same as `[!NOTE]`. Normalize the type to uppercase
// before parsing so all casings render as alerts.
const ALERT_RE = /^(\s*>+\s*)\[!([a-z]+)]/gim
function normalizeAlerts(md: string): string {
  return md.replace(ALERT_RE, (_m, prefix: string, type: string) => `${prefix}[!${type.toUpperCase()}]`)
}

// News links always open in a new window (the system browser on desktop)
// instead of navigating the launcher itself. Internal anchors (#footnote-…)
// must stay in-page so footnote jumps work.
DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A' && !(node.getAttribute('href') ?? '').startsWith('#')) {
    node.setAttribute('target', '_blank')
    node.setAttribute('rel', 'noopener noreferrer')
  }
})

/** Renders md (GFM + inline HTML) or raw HTML, sanitized against XSS. */
function renderNews(content: string, source: string): string {
  const isHtml = /\.html?([?#].*)?$/i.test(source)
  const raw = isHtml ? content : (marked.parse(normalizeAlerts(content)) as string)
  const sanitized = DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    ADD_TAGS: ['input', 'section'],
    ADD_ATTR: ['type', 'checked', 'disabled', 'id', 'data-footnote-ref', 'data-footnote-backref', 'aria-describedby', 'aria-label'],
    FORBID_TAGS: ['style', 'iframe', 'object', 'embed', 'form', 'textarea', 'select', 'button'],
    FORBID_ATTR: ['srcset'],
  })
  const document = new DOMParser().parseFromString(sanitized, 'text/html')
  for (const table of document.querySelectorAll('table')) {
    const wrapper = document.createElement('div')
    wrapper.className = 'news-table'
    table.before(wrapper)
    wrapper.append(table)
  }
  return document.body.innerHTML
}

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

// --- Linked dual dropdowns: instance -> profiles of its DSH_HOME ----------

const selectedInstanceId = ref<string | undefined>(store.settings.last_instance_id ?? undefined)
const profiles = ref<string[]>([])
/** Kind per profile name ("web" | "tui" | "other") for dropdown badges. */
const profileKinds = ref<Record<string, string>>({})
const selectedProfile = ref<string | undefined>(undefined)
const compatibility = ref<CompatibilityReportType | null>(null)
const compatibilityBusy = ref(false)
const handoffCompleted = ref(false)
/** Ids whose launch is currently driven by the early-loading window; status
 * watch reports `opening-window` only for these (a merely-running instance
 * must not revive a closed or never-opened window). */
const earlyLoadingActive = ref(new Set<string>())
// The compatibility result shows in a popup, not inline (issue: launch-UX).
// Once the DSH window opens the popup hides at once — the details stay in
// the instance log file only (<data>/logs/<id>.log).
const dshWindowOpened = ref(false)
const compatDismissed = ref(false)
watch([selectedInstanceId, selectedProfile], () => {
  compatibility.value = null
  handoffCompleted.value = false
  dshWindowOpened.value = false
  compatDismissed.value = false
})

/** The DSH window of `id` just opened: hide the compatibility popup and
 * close the early-loading window — the launch is fully up. */
function onDshWindowOpened(id: string) {
  earlyLoadingActive.value.delete(id)
  void api.closeEarlyLoading(id)
  if (id !== selectedInstanceId.value) return
  dshWindowOpened.value = true
  compatibility.value = null
}

const compatModalVisible = computed(
  () =>
    !dshWindowOpened.value &&
    !compatDismissed.value &&
    // A launch driven by the early-loading window renders the report there,
    // so the launcher modal stays hidden for that instance.
    !(selectedInstanceId.value && earlyLoadingActive.value.has(selectedInstanceId.value)) &&
    (compatibilityBusy.value || !!compatibility.value),
)

function closeCompatModal() {
  compatibility.value = null
  compatDismissed.value = true
}
let unlistenCompatibility: (() => void) | null = null
onMounted(async () => {
  if (!('__TAURI_INTERNALS__' in window)) return
  const { listen } = await import('@tauri-apps/api/event')
  unlistenCompatibility = await listen<CompatibilityReportType>('instance://compatibility', (event) => {
    const report = event.payload
    if (dshWindowOpened.value) return
    // A launch driven by the early-loading window renders the report there,
    // not as a launcher modal.
    if (earlyLoadingActive.value.has(report.instance_id)) {
      void api.reportLaunchCompat(report.instance_id, report)
      return
    }
    if (selectedInstanceId.value === report.instance_id && selectedProfile.value === report.profile) {
      compatibility.value = report
      if (!report.handoff_pending) handoffCompleted.value = true
    }
  })
})
onBeforeUnmount(() => unlistenCompatibility?.())
const profilesLoading = ref(false)

const selectedInstance = computed(() =>
  selectedInstanceId.value ? store.instanceById(selectedInstanceId.value) : undefined,
)

const selectedStatus = computed(() =>
  selectedInstanceId.value ? store.statusOf(selectedInstanceId.value) : undefined,
)

const selectedIcon = computed(() =>
  selectedInstanceId.value ? (iconMap.value[selectedInstanceId.value] ?? null) : null,
)

watch(
  () => store.instances.map((i) => `${i.id}:${i.icon ?? ''}`).join(','),
  loadIcons,
  { immediate: true },
)

const selectedVersion = computed(() =>
  selectedInstance.value ? store.versionById(selectedInstance.value.version_id) : undefined,
)

const sharedHome = computed(() => {
  if (!selectedInstance.value) return false
  return store.instances.filter((i) => i.home_id === selectedInstance.value!.home_id).length > 1
})

async function loadProfiles() {
  profiles.value = []
  profileKinds.value = {}
  selectedProfile.value = undefined
  const inst = selectedInstance.value
  if (!inst) return
  profilesLoading.value = true
  try {
    // Issue #31: annotated list so TUI profiles can be labeled in the
    // dropdown (launching them opens the terminal window, not a webview).
    const infos = await api.listProfileInfos(inst.home_id)
    profiles.value = infos.map((p) => p.name)
    for (const p of infos) profileKinds.value[p.name] = p.kind
    selectedProfile.value =
      (store.homeProfile && profiles.value.includes(store.homeProfile) && store.homeProfile) ||
      (inst.last_profile && profiles.value.includes(inst.last_profile) && inst.last_profile) ||
      (inst.default_profile && profiles.value.includes(inst.default_profile) && inst.default_profile) ||
      profiles.value[0] ||
      undefined
    if (profiles.value.length === 0) {
      Message.warning(t('home.noProfile'))
    }
  } catch (e) {
    Message.error(t('common.operationFailed', { msg: String(e) }))
  } finally {
    profilesLoading.value = false
  }
}

// The plugin install wizard preselects the profile chosen here.
watch(selectedProfile, (p) => {
  store.homeProfile = p ?? null
})

watch(selectedInstanceId, () => {
  loadProfiles()
  if (selectedInstanceId.value) {
    api.updateSettings({ last_instance_id: selectedInstanceId.value }).then((s) => {
      store.settings = s
    })
  }
})

// On mount the instance id may already be restored from settings without a
// watch change (e.g. navigating back to this page) — load profiles eagerly.
onMounted(() => {
  if (selectedInstanceId.value) loadProfiles()
  loadNews()
})

// --- News area ---------------------------------------------------------------

const newsSource = computed(() => (store.settings.news_source ?? '').trim())
const newsHtml = ref('')
const newsLoading = ref(false)
const newsError = ref('')

async function loadNews() {
  const src = newsSource.value
  newsHtml.value = ''
  newsError.value = ''
  if (!src) return
  newsLoading.value = true
  try {
    const content = await api.fetchNews(src)
    newsHtml.value = renderNews(content, src)
  } catch (e) {
    newsError.value = String(e)
  } finally {
    newsLoading.value = false
  }
}

watch(newsSource, () => loadNews())

// --- Mermaid diagrams --------------------------------------------------------
// Renders ```mermaid blocks inside the news DOM after it is inserted. The
// heavy mermaid module is loaded lazily on first use.

let mermaidApi: typeof import('mermaid').default | null = null
const newsBody = ref<HTMLElement | null>(null)
const newsTables = new Set<HTMLElement>()

function clearNewsTables() {
  for (const host of newsTables) render(null, host)
  newsTables.clear()
}

function renderNewsTables(root: HTMLElement) {
  for (const host of root.querySelectorAll<HTMLElement>('.news-table')) {
    if (newsTables.has(host)) continue
    const table = host.querySelector('table')
    if (!table) continue
    const html = table.outerHTML
    host.replaceChildren()
    render(h(PageScrollbar, { horizontal: true }, () => h('div', { innerHTML: html })), host)
    newsTables.add(host)
  }
}

onBeforeUnmount(clearNewsTables)

async function renderMermaidBlocks(root: HTMLElement) {
  const blocks = root.querySelectorAll<HTMLElement>('pre > code.language-mermaid')
  if (!blocks.length) return
  if (!mermaidApi) {
    mermaidApi = (await import('mermaid')).default
    mermaidApi.initialize({
      startOnLoad: false,
      theme: document.body.getAttribute('arco-theme') === 'dark' ? 'dark' : 'default',
      securityLevel: 'strict',
    })
  }
  let i = 0
  for (const code of blocks) {
    const pre = code.parentElement as HTMLElement | null
    if (!pre) continue
    const text = code.textContent ?? ''
    i += 1
    try {
      const id = `mermaid-${Date.now()}-${i}`
      const { svg } = await mermaidApi.render(id, text)
      pre.outerHTML = svg
    } catch {
      // Keep the raw code block on failure so the source stays visible.
      code.classList.remove('language-mermaid')
      code.classList.add('language-text')
    }
  }
}

watch(newsHtml, async () => {
  clearNewsTables()
  await nextTick()
  const root = newsBody.value
  if (root) {
    renderNewsTables(root)
    await renderMermaidBlocks(root)
  }
})

// Footnote / in-page anchor jumps: the content scrolls inside an a-scrollbar,
// so native `href="#id"` navigation does nothing. Intercept and scroll the
// scroll container manually.
function onNewsClick(e: MouseEvent) {
  // Spoiler bars ( >!…!< ): click pins the revealed state (hover already
  // reveals via CSS). A revealed spoiler falls through so links inside it
  // keep working.
  const spoiler = (e.target as HTMLElement | null)?.closest('.md-spoiler:not(.md-spoiler-revealed)')
  if (spoiler) {
    spoiler.classList.add('md-spoiler-revealed')
    e.preventDefault()
    return
  }
  const target = (e.target as HTMLElement | null)?.closest('a[href^="#"]') as HTMLAnchorElement | null
  if (!target) return
  e.preventDefault()
  const id = target.getAttribute('href')!.slice(1)
  const el = document.getElementById(id)
  if (!el) return
  const container = document.querySelector<HTMLElement>('.news-area .arco-scrollbar-container')
  if (!container) return
  const containerTop = container.getBoundingClientRect().top
  const top = container.scrollTop + el.getBoundingClientRect().top - containerTop
  container.scrollTo({ top, behavior: 'smooth' })
}

watch(
  () => store.instances,
  () => {
    if (selectedInstanceId.value && !store.instanceById(selectedInstanceId.value)) {
      selectedInstanceId.value = store.instances[0]?.id ?? undefined
    }
    if (!selectedInstanceId.value && store.instances.length > 0) {
      selectedInstanceId.value =
        store.settings.last_instance_id ?? store.instances[0]?.id ?? undefined
    }
  },
  { deep: true, immediate: true },
)

// --- Start / stop / open ---------------------------------------------------

const starting = computed(() => selectedStatus.value?.state === 'starting')
const running = computed(() => selectedStatus.value?.state === 'running')

const canStart = computed(
  () =>
    !!selectedInstance.value &&
    !!selectedProfile.value &&
    !starting.value &&
    !running.value &&
    !!store.versionById(selectedInstance.value.version_id),
)

/** Kind of the currently selected profile ("web" | "tui" | "other"). */
const selectedProfileKind = computed(
  () => (selectedProfile.value && profileKinds.value[selectedProfile.value]) || 'other',
)

const launchSubtitle = computed(() => {
  if (!selectedInstance.value) return ''
  const v = selectedVersion.value?.version ?? '?'
  const p = selectedProfile.value ?? '—'
  return `${v} · ${p}`
})

async function onStart() {
  if (!selectedInstanceId.value || !selectedProfile.value) return
  const id = selectedInstanceId.value
  compatibility.value = null
  dshWindowOpened.value = false
  compatDismissed.value = false
  // Early-loading window: shows the launch stages (including the
  // compatibility preflight) until the DSH window is up.
  earlyLoadingActive.value.add(id)
  await api.openEarlyLoading(id).catch(() => {})
  try {
    await api.reportLaunchStage(id, 'spawning')
    await api.startInstance(id, selectedProfile.value)
    await api.reportLaunchStage(id, 'waiting-ready')
    // TUI profiles: start_instance opens the terminal window; the PTY session
    // is started by that window (issue #31).
    if (selectedProfileKind.value === 'tui') {
      Message.success(t('home.startedTui'))
      // The TUI window IS the DSH window: it opened with the start.
      onDshWindowOpened(id)
    } else {
      Message.success(t('home.started'))
      // Launch behavior: optionally open the instance window once ready.
      if (store.settings.auto_open_on_launch) {
        store
          .openWindowWhenReady(id)
          .then(() => onDshWindowOpened(id))
          .catch((e) => {
            // A cancelled launch is refused by the backend; the early-loading
            // window already shows the cancelled state, so stay quiet here.
            if (String(e).includes('启动已取消')) return
            Message.error(String(e))
          })
      }
    }
    void reportHealth(id, selectedProfile.value)
  } catch (e) {
    // Sync failure (preflight / spawn): surface the full detail in the
    // launch-failure dialog instead of a transient toast (issue #30).
    // (onStart early-returns without a selection, so an id always exists here.)
    earlyLoadingActive.value.delete(id)
    void api.reportLaunchStage(id, 'failed', undefined, String(e))
    void api.closeEarlyLoading(id)
    store.reportLaunchError({
      instanceId: id,
      message: String(e),
      exitCode: null,
    })
  }
}

async function onCompatibleStart() {
  const id = selectedInstanceId.value
  const profile = selectedProfile.value
  if (!id || !profile) return
  compatibility.value = null
  handoffCompleted.value = false
  dshWindowOpened.value = false
  compatDismissed.value = false
  compatibilityBusy.value = true
  earlyLoadingActive.value.add(id)
  await api.openEarlyLoading(id).catch(() => {})
  void api.reportLaunchStage(id, 'preflight')
  try {
    const report = await api.startCompatibleInstance(id, profile)
    // A launch driven by the early-loading window renders the report there,
    // not as a launcher modal.
    if (earlyLoadingActive.value.has(id)) {
      void api.reportLaunchCompat(id, report)
    } else if (selectedInstanceId.value === id && selectedProfile.value === profile &&
        !(report.handoff_pending && handoffCompleted.value)) {
      compatibility.value = report
    }
    void api.reportLaunchStage(
      id,
      report.started || report.handoff_pending ? 'waiting-ready' : 'failed',
      undefined,
      report.started || report.handoff_pending
        ? undefined
        : t('earlyLoading.compatBlocked', { count: report.unresolved.length }),
    )
    if (report.started && selectedProfileKind.value !== 'tui' && store.settings.auto_open_on_launch) {
      store
        .openWindowWhenReady(id)
        .then(() => onDshWindowOpened(id))
        .catch((e) => {
          // A cancelled launch is refused by the backend; the early-loading
          // window already shows the cancelled state, so stay quiet here.
          if (String(e).includes('启动已取消')) return
          Message.error(String(e))
        })
    }
    // TUI handoff: the backend already opened the terminal window.
    if (report.handoff_pending && selectedProfileKind.value === 'tui') {
      onDshWindowOpened(id)
    }
  } catch (e) {
    earlyLoadingActive.value.delete(id)
    void api.reportLaunchStage(id, 'failed', undefined, String(e))
    void api.closeEarlyLoading(id)
    if (selectedInstanceId.value === id && selectedProfile.value === profile) Message.error(String(e))
  } finally {
    compatibilityBusy.value = false
  }
}

async function reportHealth(instanceId: string, profile: string) {
  try {
    const report = await api.checkPluginCompatibility(instanceId, profile)
    // The DSH window already opened: keep the details in the log file only.
    if (dshWindowOpened.value) return
    // A launch driven by the early-loading window renders the report there,
    // not as a launcher modal.
    if (earlyLoadingActive.value.has(instanceId)) {
      void api.reportLaunchCompat(instanceId, report)
      return
    }
    if (selectedInstanceId.value === instanceId && selectedProfile.value === profile) {
      report.started = selectedProfileKind.value !== 'tui'
      report.handoff_pending = selectedProfileKind.value === 'tui'
      compatibility.value = report
    }
  } catch (e) {
    if (selectedInstanceId.value === instanceId && selectedProfile.value === profile) Message.warning(String(e))
  }
}

async function onStop() {
  if (!selectedInstanceId.value) return
  try {
    await api.stopInstance(selectedInstanceId.value)
    Message.success(t('home.stopped'))
  } catch (e) {
    Message.error(String(e))
  }
}

async function onOpenWindow() {
  if (!selectedInstanceId.value) return
  const id = selectedInstanceId.value
  try {
    await api.openInstanceWindow(id)
    onDshWindowOpened(id)
  } catch (e) {
    Message.error(String(e))
  }
}

// While the early-loading window is up, reflect the instance reaching
// `running` as the final stage so the window shows real progress even when
// auto_open_on_launch is off (the window closes from onDshWindowOpened or
// from the process waiter when the launch ends). Guarded by
// earlyLoadingActive so a merely-running selected instance never reports.
watch(selectedStatus, (st) => {
  const id = selectedInstanceId.value
  if (!id || !st || st.state !== 'running') return
  if (!earlyLoadingActive.value.has(id)) return
  void api.reportLaunchStage(id, 'opening-window')
}, { flush: 'post' })

function goEditSelected() {
  if (selectedInstanceId.value) {
    router.push({ name: 'instance-edit', params: { id: selectedInstanceId.value } })
  }
}
</script>

<template>
  <div class="home-page dl-split-page">
    <!-- Left launch panel -->
    <aside class="launch-panel dl-sidebar">
      <div class="identity-block">
        <div class="instance-avatar"><img :src="selectedIcon ?? launcherDefaultIcon" alt="" /></div>
        <div class="instance-name">{{ selectedInstance?.name ?? '—' }}</div>
        <dsh-tag
          v-if="selectedStatus"
          :color="selectedStatus.state === 'running' ? 'green' : selectedStatus.state === 'starting' ? 'orange' : 'gray'"
          size="small"
        >
          {{ t(`home.status.${selectedStatus.state}`) }}
        </dsh-tag>
        <InstanceAccessLinks
          v-if="running && selectedStatus?.url"
          :status="selectedStatus"
          centered
          @opened="onDshWindowOpened"
        />
        <dsh-tag v-if="sharedHome" color="orangered" size="small">
          {{ t('home.sharedHome') }}
          <HintIcon :content="t('home.sharedHomeWarning')" />
        </dsh-tag>
      </div>

      <div class="selector-block">
        <div class="field">
          <span class="field-label">{{ t('home.instance') }}</span>
          <dsh-select
            v-model="selectedInstanceId"
            :placeholder="t('home.selectInstance')"
            allow-clear
          >
            <dsh-option v-for="inst in store.instances" :key="inst.id" :value="inst.id">
              <span class="option-line">
                {{ inst.name }}
                <dsh-tag
                  v-if="store.statusOf(inst.id).state === 'running'"
                  size="small"
                  color="green"
                >
                  {{ t('home.status.running') }}
                </dsh-tag>
              </span>
            </dsh-option>
          </dsh-select>
        </div>
        <div class="field">
          <span class="field-label">{{ t('home.profile') }}</span>
          <dsh-select
            v-model="selectedProfile"
            :placeholder="t('home.selectProfile')"
            :loading="profilesLoading"
            :disabled="!selectedInstance"
            allow-clear
          >
            <template #label>
              <span class="option-line">
                {{ selectedProfile }}
                <dsh-tag v-if="selectedProfileKind === 'tui'" size="small" color="purple">
                  {{ t('home.profileTui') }}
                </dsh-tag>
                <dsh-tag v-else-if="selectedProfileKind === 'web'" size="small" color="green">Web</dsh-tag>
              </span>
            </template>
            <dsh-option v-for="p in profiles" :key="p" :value="p">
              <span class="option-line">
                {{ p }}
                <dsh-tag v-if="profileKinds[p] === 'tui'" size="small" color="purple">
                  {{ t('home.profileTui') }}
                </dsh-tag>
                <dsh-tag v-else-if="profileKinds[p] === 'web'" size="small" color="green">Web</dsh-tag>
              </span>
            </dsh-option>
          </dsh-select>
        </div>
      </div>

      <div class="action-block">
        <template v-if="!running">
          <dsh-button variant="primary"
            size="lg"
            radius="full"
            long
            :disabled="!canStart"
            :loading="starting"
            class="launch-button"
            @click="onStart"
          >
            <span class="launch-text">{{ starting ? t('home.starting') : t('home.start') }}</span>
            <span v-if="launchSubtitle && !starting" class="launch-sub">{{ launchSubtitle }}</span>
          </dsh-button>
          <dsh-button variant="secondary" long :disabled="!canStart || compatibilityBusy" :loading="compatibilityBusy" @click="onCompatibleStart">
            {{ t('compat.launch') }}
          </dsh-button>
        </template>
        <template v-else>
          <dsh-button variant="primary" size="lg" radius="full" long class="launch-button" @click="onOpenWindow">
            <span class="launch-text">{{ t('home.openWindow') }}</span>
            <span class="launch-sub">{{ launchSubtitle }}</span>
          </dsh-button>
          <dsh-button variant="danger" long class="stop-button" @click="onStop">
            {{ t('home.stop') }}
          </dsh-button>
        </template>
        <div class="mini-actions">
          <dsh-button variant="secondary" class="mini-button" @click="router.push({ name: 'instances' })">
            {{ t('home.instanceList') }}
          </dsh-button>
          <dsh-button variant="secondary" class="mini-button" :disabled="!selectedInstance" @click="goEditSelected">
            {{ t('home.editSelected') }}
          </dsh-button>
        </div>
      </div>
    </aside>

    <!-- Compatibility result pops up instead of stacking under the buttons;
         it hides the moment the DSH window opens (details stay in the log). -->
    <dsh-modal
      :visible="compatModalVisible"
      :title="t('compat.title')"
      :closable="!compatibilityBusy"
      :mask-closable="!compatibilityBusy"
      :hide-cancel="true"
      :ok-text="t('common.confirm')"
      width="560px"
      @ok="closeCompatModal"
      @cancel="closeCompatModal"
    >
      <dsh-scrollbar type="track" outer-style="max-height: 60vh" style="max-height: 60vh; overflow-y: auto">
        <div class="compat-modal-body">
          <div v-if="compatibilityBusy && !compatibility" class="compat-checking">
            <dsh-spin :size="22" />
            <span>{{ t('compat.checking') }}</span>
          </div>
          <CompatibilityReport v-else-if="compatibility" :report="compatibility" />
        </div>
      </dsh-scrollbar>
    </dsh-modal>

    <!-- Right news area: renders the configured md/html source (XSS-sanitized) -->
    <section class="news-area dl-page-content">
      <div v-if="!newsSource" class="news-placeholder">{{ t('home.newsPlaceholder') }}</div>
      <div v-else-if="newsLoading" class="news-placeholder">
        <dsh-spin :size="20" />
      </div>
      <div v-else-if="newsError" class="news-placeholder news-error">
        <span>{{ newsError }}</span>
        <dsh-button variant="secondary" size="sm" @click="loadNews">{{ t('common.refresh') }}</dsh-button>
      </div>
      <PageScrollbar v-else>
        <article ref="newsBody" class="news-body" v-html="newsHtml" @click="onNewsClick"></article>
      </PageScrollbar>
    </section>
  </div>
</template>

<style lang="scss" scoped>
.home-page {
  display: flex;
  height: var(--dl-content-height);
}

.launch-panel {
  width: 320px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  padding: 20px 16px;
}

.selector-block {
  display: flex;
  flex-direction: column;
  gap: 16px;
  margin-bottom: 14px;

  :deep(.arco-select-view-single) {
    // Include the 1px border when aligning text with the 18px capsule end.
    padding-inline-start: 17px;
  }
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.field-label {
  padding-inline-start: 18px;
  font-size: 12px;
  color: var(--color-text-3);
}

.identity-block {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  min-height: 180px;
}

.instance-avatar {
  width: 88px;
  height: 88px;
  border-radius: 16px;
  background: linear-gradient(135deg, #165dff, #722ed1);
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  box-shadow: 0 6px 16px rgb(22 93 255 / 25%);
  user-select: none;

  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
}

.instance-name {
  font-size: 18px;
  font-weight: 600;
}

.action-block {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

// The popup caps its own height and scrolls internally (a-scrollbar track),
// so a long findings list never stretches the modal past the viewport and no
// opaque native scrollbar track shows up.
.compat-modal-body {
  padding-right: 6px;
}

.compat-checking { display: flex; align-items: center; gap: 12px; padding: 24px 4px; font-size: 13px; color: var(--color-text-2); }

.launch-panel .launch-button {
  height: 64px;
  padding-block: 10px;
  display: flex;
  flex-direction: column;
  gap: 4px;

  .launch-text {
    font-size: 17px;
    line-height: 24px;
    font-weight: 600;
  }

  .launch-sub {
    font-size: 12px;
    line-height: 16px;
    opacity: 0.8;
  }
}

.stop-button {
  height: 40px;
}

.mini-actions {
  display: flex;
  gap: 10px;
  justify-content: center;
}

.mini-button {
  flex: 1;
}

.news-area {
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.news-placeholder {
  height: 100%;
  margin-inline-start: var(--dl-sidebar-gap);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: var(--color-text-4);
  font-size: 14px;
  letter-spacing: 4px;
  user-select: none;
}

.news-error {
  color: rgb(var(--red-6));
  font-size: 13px;
  letter-spacing: 0;
  padding: 0 24px;
  text-align: center;
  word-break: break-all;
}

.news-body {
  padding: 24px 28px;
  font-size: 14px;
  line-height: 1.7;
  color: var(--color-text-1);
  word-wrap: break-word;

  :deep(h1),
  :deep(h2),
  :deep(h3) {
    margin: 18px 0 10px;
    line-height: 1.35;
  }

  :deep(h1) {
    font-size: 22px;
  }

  :deep(h2) {
    font-size: 18px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--color-border-2);
  }

  :deep(h3) {
    font-size: 16px;
  }

  :deep(p) {
    margin: 8px 0;
  }

  // Read-only task lists keep rounded-square checkboxes in both states.
  :deep(li:has(> input[type='checkbox'])),
  :deep(li:has(> p > input[type='checkbox'])) {
    list-style: none;

    input[type='checkbox'] {
      appearance: none;
      -webkit-appearance: none;
      width: 20px;
      height: 20px;
      margin: 0 10px 0 0;
      border: 0;
      border-radius: 7px;
      vertical-align: -4px;
      background: var(--surface);
      opacity: 1;

      &:checked {
        background-color: var(--accent);
        background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Cpath d='m4 8 2.5 2.5L12 5' fill='none' stroke='white' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
        background-position: center;
        background-size: 14px 14px;
        background-repeat: no-repeat;
      }
    }
  }

  :deep(mark) {
    padding: 1px 6px;
    border-radius: 6px;
    color: inherit;
    background: rgb(255 214 102 / 45%);
    -webkit-backdrop-filter: var(--dl-glass-blur);
    backdrop-filter: var(--dl-glass-blur);
    -webkit-box-decoration-break: clone;
    box-decoration-break: clone;
  }

  :deep(kbd) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 3px;
    min-height: 24px;
    padding: 1px 7px;
    margin-inline: 4px;
    border: 0;
    border-radius: 7px;
    background: var(--surface-secondary);
    color: var(--muted);
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    line-height: 20px;
    vertical-align: middle;
    white-space: nowrap;

    .kbd-symbol {
      color: rgb(var(--primary-6));
      font-size: 15px;
      line-height: 1;
    }
  }

  :deep(a) {
    color: rgb(var(--primary-6));
    text-decoration: none;

    &:hover {
      text-decoration: underline;
    }
  }

  :deep(code) {
    font-family: Consolas, 'Courier New', monospace;
    font-size: 0.9em;
    background: var(--color-fill-2);
    border-radius: 4px;
    padding: 1px 5px;
  }

  // Inline spoilers ( >!…!< ): the bar flattens the (translucent in dark
  // mode) text color over the page background so it is fully opaque, while
  // the glyphs are transparent — nothing can show through. Hover or click
  // reveals normal text on a transparent background.
  :deep(.md-spoiler) {
    border-radius: 4px;
    padding: 0 4px;
    transition:
      background-color 0.15s,
      color 0.15s;
  }

  :deep(.md-spoiler:not(.md-spoiler-revealed):not(:hover)) {
    background:
      linear-gradient(var(--color-text-1), var(--color-text-1)),
      var(--color-bg-1);
    color: transparent;
    cursor: pointer;
    user-select: none;

    // Children only: the span itself must keep its opaque bar background.
    * {
      color: transparent !important;
      background-color: transparent !important;
      text-shadow: none !important;
      text-decoration-color: transparent !important;
    }

    a {
      pointer-events: none;
    }
  }

  :deep(pre) {
    background: #1d2129;
    color: #a9b7c6;
    border-radius: 20px;
    padding: 12px 16px;
    overflow-x: auto;
    margin: 12px 0;
    scrollbar-width: thin;
    scrollbar-color: transparent transparent;

    &:hover,
    &:focus-within {
      scrollbar-color: #69717e transparent;
    }

    &::-webkit-scrollbar {
      height: 6px;
      background: transparent;
    }

    &::-webkit-scrollbar-track,
    &::-webkit-scrollbar-button {
      display: none;
      background: transparent;
    }

    &::-webkit-scrollbar-button {
      width: 0;
      height: 0;
    }

    &::-webkit-scrollbar-thumb {
      border-radius: 999px;
      background: transparent;
    }

    &:hover::-webkit-scrollbar-thumb,
    &:focus-within::-webkit-scrollbar-thumb {
      background: #69717e;
    }

    // Standard scrollbar colors override WebKit's hidden-button rules in Chromium.
    @supports selector(::-webkit-scrollbar) {
      scrollbar-width: auto;
      scrollbar-color: auto;

      &:hover,
      &:focus-within {
        scrollbar-color: auto;
      }
    }

    code {
      background: none;
      padding: 0;
    }
  }

  :deep(blockquote) {
    margin: 12px 0;
    padding: 4px 14px;
    border-left: 3px solid var(--dl-quote-border);
    background: transparent;
    color: var(--color-text-2);
    border-radius: 0;
  }

  // GitHub-style alerts: > [!NOTE] / [!TIP] / [!IMPORTANT] / [!WARNING] / [!CAUTION]
  :deep(.markdown-alert) {
    margin: 12px 0;
    padding: 10px 14px;
    border: 1px solid rgba(var(--md-alert-color), 0.22);
    border-radius: 20px;
    font-size: 13.5px;
    background: color-mix(in srgb, rgb(var(--md-alert-color)) 18%, rgb(255 255 255 / 8%));
    -webkit-backdrop-filter: var(--dl-glass-blur);
    backdrop-filter: var(--dl-glass-blur);

    .markdown-alert-title {
      display: flex;
      align-items: center;
      gap: 6px;
      margin: 0 0 6px;
      font-weight: 600;
      color: rgb(var(--md-alert-color));

      svg {
        width: 16px;
        height: 16px;
        flex-shrink: 0;
      }
    }

    p {
      margin: 4px 0;
    }
  }

  :deep(.markdown-alert-note) {
    --md-alert-color: var(--primary-6);
  }

  :deep(.markdown-alert-tip) {
    --md-alert-color: var(--green-6);
  }

  :deep(.markdown-alert-important) {
    --md-alert-color: var(--purple-6);
  }

  :deep(.markdown-alert-warning) {
    --md-alert-color: var(--orange-6);
  }

  :deep(.markdown-alert-caution) {
    --md-alert-color: var(--red-6);
  }

  :deep(.news-table) {
    margin: 12px 0;
    max-width: 100%;
    padding: 0 4px 4px;
    border-radius: 20px;
    background: var(--surface-secondary);
    overflow: hidden;

    table {
      width: max-content;
      min-width: 100%;
      border-collapse: separate;
      border-spacing: 0;
    }

    th,
    td {
      border: 0;
    }

    th {
      position: relative;
      padding: 10px 16px;
      background: var(--surface-secondary);
      color: var(--muted);
      font-size: 12px;
      font-weight: 500;
      line-height: 16px;
    }

    th:not(:last-child)::after {
      content: '';
      position: absolute;
      right: 0;
      top: 50%;
      width: 1px;
      height: 16px;
      transform: translateY(-50%);
      background: var(--separator);
    }

    td {
      padding: 12px 16px;
      background: var(--surface);
      color: var(--foreground);
      font-size: 14px;
      line-height: 20px;
      border-bottom: 1px solid color-mix(in oklab, var(--separator-tertiary) 50%, transparent);
    }

    tbody tr:first-child td:first-child { border-top-left-radius: 16px; }
    tbody tr:first-child td:last-child { border-top-right-radius: 16px; }
    tbody tr:last-child td:first-child { border-bottom-left-radius: 16px; }
    tbody tr:last-child td:last-child { border-bottom-right-radius: 16px; }
    tbody tr:hover td { background: var(--surface-hover); }
  }

  :deep(ul),
  :deep(ol) {
    margin: 8px 0;
    padding-left: 24px;
  }

  :deep(li) {
    margin: 4px 0;
  }

  :deep(img) {
    max-width: 100%;
    border-radius: 6px;
  }

  :deep(hr) {
    border: none;
    border-top: 1px solid var(--color-border-2);
    margin: 16px 0;
  }
}

.option-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
</style>
