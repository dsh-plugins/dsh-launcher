<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message, Notification } from '@arco-design/web-vue'
import { Marked } from 'marked'
import markedAlert from 'marked-alert'
import markedFootnote from 'marked-footnote'
import markedKatex from 'marked-katex-extension'
import DOMPurify from 'dompurify'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import { markedSpoiler } from '@/utils/marked-spoiler'
import launcherDefaultIcon from '@/assets/launcher-icon.png'
import 'katex/dist/katex.min.css'

// --- Instance icons (issue #8): the launch panel avatar follows the instance --

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
  return DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    ADD_TAGS: ['input', 'section'],
    ADD_ATTR: ['type', 'checked', 'disabled', 'id', 'data-footnote-ref', 'data-footnote-backref', 'aria-describedby', 'aria-label'],
    FORBID_TAGS: ['style', 'iframe', 'object', 'embed', 'form', 'textarea', 'select', 'button'],
    FORBID_ATTR: ['srcset'],
  })
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
  await nextTick()
  const root = document.querySelector<HTMLElement>('.news-body')
  if (root) await renderMermaidBlocks(root)
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
  history.replaceState(null, '', `#${id}`)
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
  try {
    await api.startInstance(selectedInstanceId.value, selectedProfile.value)
    // TUI profiles: start_instance opens the terminal window; the PTY session
    // is started by that window (issue #31).
    if (selectedProfileKind.value === 'tui') {
      Message.success(t('home.startedTui'))
    } else {
      Message.success(t('home.started'))
    }
    // Dependency-tree preflight: advisory only, never blocks the launch. A
    // duplicated core copy in the profile silently breaks every tool call at
    // runtime, so surface it here instead of leaving users to dig through logs.
    void reportHealth(selectedInstanceId.value, selectedProfile.value)
  } catch (e) {
    Message.error(String(e))
  }
}

async function reportHealth(instanceId: string, profile: string) {
  try {
    const report = await api.checkInstanceHealth(instanceId, profile)
    for (const f of report.findings.slice(0, 3)) {
      const content = `${t('home.health.prefix')}${f.message}`
      if (f.level === 'error') Notification.error({ title: t('home.health.errorTitle'), content, duration: 0, closable: true })
      else Notification.warning({ title: t('home.health.warnTitle'), content, duration: 8000, closable: true })
    }
  } catch {
    // A failed preflight must never affect the launch.
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
  try {
    await api.openInstanceWindow(selectedInstanceId.value)
  } catch (e) {
    Message.error(String(e))
  }
}

function copyUrl(url: string) {
  navigator.clipboard?.writeText(url)
  Message.success(t('common.copied'))
}

function goEditSelected() {
  if (selectedInstanceId.value) {
    router.push({ name: 'instance-edit', params: { id: selectedInstanceId.value } })
  }
}
</script>

<template>
  <div class="home-page">
    <!-- Left launch panel -->
    <aside class="launch-panel">
      <div class="identity-block">
        <div class="instance-avatar"><img :src="selectedIcon ?? launcherDefaultIcon" alt="" /></div>
        <div class="instance-name">{{ selectedInstance?.name ?? '—' }}</div>
        <a-tag
          v-if="selectedStatus"
          :color="selectedStatus.state === 'running' ? 'green' : selectedStatus.state === 'starting' ? 'orange' : 'gray'"
          size="small"
        >
          {{ t(`home.status.${selectedStatus.state}`) }}
        </a-tag>
        <div v-if="running && selectedStatus?.url" class="running-url">
          <a-link class="url-link" :title="selectedStatus.url" @click="onOpenWindow">
            {{ selectedStatus.url }}
          </a-link>
          <a-button size="mini" type="text" class="url-copy" @click="copyUrl(selectedStatus.url)">
            {{ t('common.copy') }}
          </a-button>
        </div>
        <a-tooltip v-if="sharedHome" :content="t('home.sharedHomeWarning')">
          <a-tag color="orangered" size="small">{{ t('home.sharedHome') }}</a-tag>
        </a-tooltip>
      </div>

      <div class="selector-block">
        <div class="field">
          <span class="field-label">{{ t('home.instance') }}</span>
          <a-select
            v-model="selectedInstanceId"
            :placeholder="t('home.selectInstance')"
            allow-clear
          >
            <a-option v-for="inst in store.instances" :key="inst.id" :value="inst.id">
              <span class="option-line">
                {{ inst.name }}
                <a-tag
                  v-if="store.statusOf(inst.id).state === 'running'"
                  size="small"
                  color="green"
                >
                  {{ t('home.status.running') }}
                </a-tag>
              </span>
            </a-option>
          </a-select>
        </div>
        <div class="field">
          <span class="field-label">{{ t('home.profile') }}</span>
          <a-select
            v-model="selectedProfile"
            :placeholder="t('home.selectProfile')"
            :loading="profilesLoading"
            :disabled="!selectedInstance"
            allow-clear
          >
            <a-option v-for="p in profiles" :key="p" :value="p">
              <span class="option-line">
                {{ p }}
                <a-tag v-if="profileKinds[p] === 'tui'" size="small" color="purple">
                  {{ t('home.profileTui') }}
                </a-tag>
                <a-tag v-else-if="profileKinds[p] === 'web'" size="small" color="green">Web</a-tag>
              </span>
            </a-option>
          </a-select>
        </div>
      </div>

      <div class="action-block">
        <template v-if="!running">
          <a-button
            type="primary"
            size="large"
            long
            :disabled="!canStart"
            :loading="starting"
            class="launch-button"
            @click="onStart"
          >
            <span class="launch-text">{{ starting ? t('home.starting') : t('home.start') }}</span>
            <span v-if="launchSubtitle && !starting" class="launch-sub">{{ launchSubtitle }}</span>
          </a-button>
        </template>
        <template v-else>
          <a-button type="primary" size="large" long class="launch-button" @click="onOpenWindow">
            <span class="launch-text">{{ t('home.openWindow') }}</span>
            <span class="launch-sub">{{ launchSubtitle }}</span>
          </a-button>
          <a-button status="danger" long class="stop-button" @click="onStop">
            {{ t('home.stop') }}
          </a-button>
        </template>
        <div class="mini-actions">
          <a-button class="mini-button" @click="router.push({ name: 'instances' })">
            {{ t('home.instanceList') }}
          </a-button>
          <a-button class="mini-button" :disabled="!selectedInstance" @click="goEditSelected">
            {{ t('home.editSelected') }}
          </a-button>
        </div>
      </div>
    </aside>

    <!-- Right news area: renders the configured md/html source (XSS-sanitized) -->
    <section class="news-area">
      <div v-if="!newsSource" class="news-placeholder">{{ t('home.newsPlaceholder') }}</div>
      <div v-else-if="newsLoading" class="news-placeholder">
        <a-spin :size="20" />
      </div>
      <div v-else-if="newsError" class="news-placeholder news-error">
        <span>{{ newsError }}</span>
        <a-button size="mini" @click="loadNews">{{ t('common.refresh') }}</a-button>
      </div>
      <a-scrollbar v-else outer-style="height: 100%" style="height: 100%; overflow-y: auto">
        <article class="news-body" v-html="newsHtml" @click="onNewsClick"></article>
      </a-scrollbar>
    </section>
  </div>
</template>

<style lang="scss" scoped>
.home-page {
  display: flex;
  height: calc(100vh - var(--dl-header-height));
}

.launch-panel {
  width: 320px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  padding: 20px 16px;
  background: var(--color-bg-2);
  border-right: 1px solid var(--color-border-2);
}

.selector-block {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-bottom: 14px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.field-label {
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
  min-height: 0;
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

.running-url {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  max-width: 100%;
  min-width: 0;

  // Token-bearing URLs are long; ellipsize the link and keep the full URL
  // in the hover title / copy button.
  .url-link {
    flex: 1 1 auto;
    min-width: 0;
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .url-copy {
    flex-shrink: 0;
  }
}

.action-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.launch-button {
  height: 64px;
  display: flex;
  flex-direction: column;

  .launch-text {
    font-size: 17px;
    font-weight: 600;
  }

  .launch-sub {
    font-size: 12px;
    opacity: 0.8;
    margin-top: 2px;
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
  background:
    radial-gradient(circle at 30% 20%, rgb(22 93 255 / 6%), transparent 40%),
    radial-gradient(circle at 70% 80%, rgb(114 46 209 / 6%), transparent 40%);
}

.news-placeholder {
  height: 100%;
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

  // Task-list checkboxes (GitHub 风格)
  :deep(li) {
    list-style: none;

    input[type='checkbox'] {
      margin-right: 6px;
      vertical-align: -2px;
      accent-color: rgb(var(--primary-6));
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
    border-radius: 8px;
    padding: 12px 16px;
    overflow-x: auto;
    margin: 12px 0;

    code {
      background: none;
      padding: 0;
    }
  }

  :deep(blockquote) {
    margin: 12px 0;
    padding: 4px 14px;
    border-left: 3px solid rgb(var(--primary-6));
    background: var(--color-fill-1);
    color: var(--color-text-2);
    border-radius: 0 6px 6px 0;
  }

  // GitHub-style alerts: > [!NOTE] / [!TIP] / [!IMPORTANT] / [!WARNING] / [!CAUTION]
  :deep(.markdown-alert) {
    margin: 12px 0;
    padding: 10px 14px;
    border: 1px solid;
    border-radius: 6px;
    font-size: 13.5px;

    .markdown-alert-title {
      display: flex;
      align-items: center;
      gap: 6px;
      margin: 0 0 6px;
      font-weight: 600;

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
    border-color: rgb(var(--primary-6));
    background: rgb(var(--primary-6) / 6%);

    .markdown-alert-title {
      color: rgb(var(--primary-6));
    }
  }

  :deep(.markdown-alert-tip) {
    border-color: rgb(var(--green-6));
    background: rgb(var(--green-6) / 6%);

    .markdown-alert-title {
      color: rgb(var(--green-6));
    }
  }

  :deep(.markdown-alert-important) {
    border-color: rgb(var(--purple-6));
    background: rgb(var(--purple-6) / 6%);

    .markdown-alert-title {
      color: rgb(var(--purple-6));
    }
  }

  :deep(.markdown-alert-warning) {
    border-color: rgb(var(--orange-6));
    background: rgb(var(--orange-6) / 6%);

    .markdown-alert-title {
      color: rgb(var(--orange-6));
    }
  }

  :deep(.markdown-alert-caution) {
    border-color: rgb(var(--red-6));
    background: rgb(var(--red-6) / 6%);

    .markdown-alert-title {
      color: rgb(var(--red-6));
    }
  }

  :deep(table) {
    border-collapse: collapse;
    margin: 12px 0;
    max-width: 100%;
    display: block;
    overflow-x: auto;

    th,
    td {
      border: 1px solid var(--color-border-2);
      padding: 6px 12px;
      font-size: 13px;
    }

    th {
      background: var(--color-fill-2);
      font-weight: 600;
    }
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
