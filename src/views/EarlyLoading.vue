<script setup lang="ts">
// Content of the frameless `early-loading-<instance>` window: shows the
// launch stages (compatibility preflight → spawn → wait for the DSH page →
// open the window) while an instance starts. A real OS window, not a modal.
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { CompatibilityReport, EarlyLoadingContext, LaunchStage, ProviderRouteReport } from '@/api/types'
import FramelessTitleBar from '@/components/FramelessTitleBar.vue'
import CompatReport from '@/components/CompatibilityReport.vue'
import ProviderReport from '@/components/ProviderReport.vue'

const props = defineProps<{ instanceId: string }>()

const { t } = useI18n()

const ctx = ref<EarlyLoadingContext | null>(null)
const stage = ref<LaunchStage>('preflight')
const detail = ref('')
const cancelling = ref(false)
/** Compatibility report forwarded from the launch driver, rendered inline. */
const compat = ref<CompatibilityReport | null>(null)
/** Provider pre-launch self-check report forwarded from the launch driver. */
const providerReport = ref<ProviderRouteReport[] | null>(null)

/** The pipeline stages in display order (terminal states excluded). */
const PIPELINE: LaunchStage[] = ['preflight', 'spawning', 'waiting-ready', 'opening-window']

/** Per-area cap: past this an inline report scrolls inside its own area
 *  instead of growing the window without bound. Generous enough that the
 *  usual short report is never clipped (the window only grows to what the
 *  content needs, see `relayout`). */
const MAX_REPORT_HEIGHT = 420

const terminal = computed(() => stage.value === 'done' || stage.value === 'cancelled' || stage.value === 'failed')
const failed = computed(() => stage.value === 'failed')

/** Index of the active stage; terminal states light up the whole pipeline. */
const activeIndex = computed(() => {
  if (stage.value === 'done') return PIPELINE.length
  return PIPELINE.indexOf(stage.value)
})

function stageState(s: LaunchStage): 'done' | 'active' | 'todo' {
  const idx = PIPELINE.indexOf(s)
  if (idx < activeIndex.value) return 'done'
  if (idx === activeIndex.value) return terminal.value ? 'done' : 'active'
  return 'todo'
}

let unlisten: (() => void) | undefined
let unlistenCompat: (() => void) | undefined
let unlistenProvider: (() => void) | undefined

/** Template refs used to measure the inline reports' natural height. */
const compatAreaEl = ref<HTMLElement | null>(null)
const providerAreaEl = ref<HTMLElement | null>(null)

/** Template-ref setter for the two report scroll areas. Arco's `a-scrollbar`
 *  is a component, so the ref receives the instance: unwrap `$el` to reach the
 *  DOM node whose `scrollHeight` is what we want to measure. */
function setAreaEl(which: 'compat' | 'provider', el: unknown) {
  const maybe = el as { $el?: unknown } | null
  const node = (maybe && maybe.$el ? maybe.$el : el) as HTMLElement | null
  const target = node && typeof node === 'object' && 'scrollHeight' in node ? node : null
  if (which === 'compat') compatAreaEl.value = target
  else providerAreaEl.value = target
}

onMounted(async () => {
  try {
    ctx.value = await api.getEarlyLoadingContext(props.instanceId)
  } catch {
    ctx.value = { instance_id: props.instanceId, name: props.instanceId, profile: null }
  }
  // The self-check is fast local IO and can finish before this webview has
  // mounted, so the backend also hands the report over with the context
  // (issue #83). Seed from it, then keep listening: whichever arrives first
  // renders the same report, and a later one (re-check in a live window)
  // still updates.
  if (ctx.value.provider_report?.length) {
    providerReport.value = ctx.value.provider_report
    await nextTick()
    void relayout()
  }
  unlisten = await api.onEarlyLoadingProgress((p) => {
    if (p.instance_id !== props.instanceId) return
    // Terminal states are sticky: a late or out-of-order report must never
    // revive a finished window (the launch driver's status watcher can race
    // with the window close).
    if (terminal.value) return
    stage.value = p.stage
    if (p.detail) detail.value = p.detail
    // Terminal states: show the outcome briefly (errors stay until closed,
    // the launch-failure dialog in the main window carries the details).
    if (p.stage === 'done') {
      window.setTimeout(() => void closeWindow(), 400)
    } else if (p.stage === 'cancelled') {
      window.setTimeout(() => void closeWindow(), 800)
    }
  })
  unlistenCompat = await api.onEarlyLoadingCompat((report) => {
    if (report.instance_id !== props.instanceId) return
    compat.value = report
    // Grow the window to fit the inline report (it starts compact).
    void relayout()
  })
  unlistenProvider = await api.onEarlyLoadingProvider((report) => {
    providerReport.value = report
    void relayout()
  })
})

/** Resizes this window's height (width stays fixed). */
async function resizeFor(height: number) {
  if (!api.isTauri) return
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  const { LogicalSize } = await import('@tauri-apps/api/dpi')
  await getCurrentWindow().setSize(new LogicalSize(520, height))
}

/** Window height with no inline report shown (title bar + stages + actions). */
const BASE_HEIGHT = 320
/** Fallback inline-report height used only before the DOM can be measured. */
const ESTIMATED_REPORT_HEIGHT = 200

/** Recomputes the window height from the inline reports currently shown
 *  (compatibility + provider self-check), so the frameless window grows just
 *  enough to fit them instead of clipping.
 *
 *  The report height is measured, not estimated: a hardcoded per-report
 *  allowance silently truncates long reports (several routes × several
 *  checks, and the English strings run longer than the Chinese ones) — and a
 *  truncated `warn` / `unknown` is exactly what the user needs to read. Each
 *  report area still scrolls internally past {@link MAX_REPORT_HEIGHT}, so a
 *  very long report stays inside the screen. */
async function relayout() {
  if (!api.isTauri) return
  // Wait for the just-updated report to be laid out before measuring it.
  await nextTick()
  let reportHeight = reportsHeight()
  if (!reportHeight) {
    if (compat.value) reportHeight += ESTIMATED_REPORT_HEIGHT
    if (providerReport.value?.length) reportHeight += ESTIMATED_REPORT_HEIGHT
  }
  const height = BASE_HEIGHT + reportHeight
  // Never grow past the usable screen: the report areas take over scrolling.
  const ceiling = Math.max(BASE_HEIGHT, (window.screen?.availHeight ?? 800) - 80)
  await resizeFor(Math.min(height, ceiling))
}

/** Combined *displayed* height of the inline report areas (0 when hidden).
 *
 *  `scrollHeight` is the full content height, but each area is capped at
 *  {@link MAX_REPORT_HEIGHT} and scrolls the overflow itself, so the window
 *  must only grow by what is actually visible — otherwise one very long
 *  report would inflate the window to the screen ceiling instead of scrolling
 *  in place. */
function reportsHeight(): number {
  let sum = 0
  for (const el of [compatAreaEl.value, providerAreaEl.value]) {
    if (el) sum += Math.min(el.scrollHeight, MAX_REPORT_HEIGHT)
  }
  return sum
}

onUnmounted(() => {
  unlisten?.()
  unlistenCompat?.()
  unlistenProvider?.()
})

async function closeWindow() {
  if (api.isTauri) {
    const { getCurrentWindow } = await import('@tauri-apps/api/window')
    await getCurrentWindow().close()
  }
}

/** 关闭：只关掉窗口，启动继续。 */
async function onClose() {
  await closeWindow()
}

/** 取消启动：停止已拉起的进程；窗口由进程收敛后的 waiter 兜底关闭，
 * 这里只切换到已取消态并等待（关闭按钮仍可作为逃生口）。 */
async function onCancel() {
  if (cancelling.value || terminal.value) return
  cancelling.value = true
  stage.value = 'cancelled'
  try {
    await api.cancelInstanceLaunch(props.instanceId)
  } catch (e) {
    Message.error(String(e))
  } finally {
    cancelling.value = false
  }
}
</script>

<template>
  <div class="early-loading">
    <FramelessTitleBar
      :title="t('earlyLoading.title', { name: ctx?.name ?? props.instanceId })"
      @close="onClose"
    />

    <div class="loading-body">
      <div v-if="ctx?.profile" class="loading-profile">
        <a-tag size="small">{{ ctx.profile }}</a-tag>
      </div>

      <ul class="stage-list">
        <li
          v-for="s in PIPELINE"
          :key="s"
          class="stage-item"
          :class="stageState(s)"
        >
          <span class="stage-dot" />
          <span class="stage-name">{{ t(`earlyLoading.stages.${s}`) }}</span>
          <a-spin v-if="stageState(s) === 'active'" :size="14" class="stage-spin" />
        </li>
      </ul>

      <div v-if="!terminal" class="indeterminate-bar">
        <div class="indeterminate-fill" />
      </div>
      <div v-else class="terminal-state" :class="{ failed }">
        {{ t(`earlyLoading.stages.${stage}`) }}
      </div>

      <div v-if="detail" class="loading-detail" :class="{ failed }">{{ detail }}</div>

      <!-- Compatibility report: rendered inline (the launcher main window no
           longer shows it as a separate modal during launch). -->
      <a-scrollbar
        v-if="compat"
        :ref="(el: unknown) => setAreaEl('compat', el)"
        type="track"
        :outer-style="{ maxHeight: `${MAX_REPORT_HEIGHT}px` }"
        :style="{ maxHeight: `${MAX_REPORT_HEIGHT}px`, overflowY: 'auto' }"
        class="compat-area"
      >
        <CompatReport :report="compat" />
      </a-scrollbar>

      <!-- Provider pre-launch self-check (issue #83): rendered inline, advisory
           only, never blocks the launch. Hidden when there are no routes. -->
      <a-scrollbar
        v-if="providerReport && providerReport.length"
        :ref="(el: unknown) => setAreaEl('provider', el)"
        type="track"
        :outer-style="{ maxHeight: `${MAX_REPORT_HEIGHT}px` }"
        :style="{ maxHeight: `${MAX_REPORT_HEIGHT}px`, overflowY: 'auto' }"
        class="provider-area"
      >
        <ProviderReport :report="providerReport" />
      </a-scrollbar>
    </div>

    <div class="loading-actions">
      <a-button @click="onClose">{{ t('earlyLoading.close') }}</a-button>
      <a-button
        status="danger"
        :disabled="terminal"
        :loading="cancelling"
        @click="onCancel"
      >
        {{ t('earlyLoading.cancel') }}
      </a-button>
    </div>
  </div>
</template>

<style scoped>
.early-loading {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: var(--color-bg-1);
}

.loading-body {
  flex: 1;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 14px;
  min-height: 0;
  padding: 16px 28px;
}

.loading-profile {
  display: flex;
  justify-content: center;
}

.stage-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.stage-item {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
  color: var(--color-text-3);
}

.stage-item.active {
  color: var(--color-text-1);
  font-weight: 600;
}

.stage-item.done {
  color: rgb(var(--green-6));
}

.stage-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--color-fill-3);
  flex-shrink: 0;
}

.stage-item.active .stage-dot {
  background: rgb(var(--primary-6));
}

.stage-item.done .stage-dot {
  background: rgb(var(--green-6));
}

.stage-spin {
  margin-left: auto;
}

.indeterminate-bar {
  height: 4px;
  border-radius: 2px;
  background: var(--color-fill-2);
  overflow: hidden;
}

.indeterminate-fill {
  height: 100%;
  width: 40%;
  border-radius: 2px;
  background: rgb(var(--primary-6));
  animation: early-loading-slide 1.2s ease-in-out infinite;
}

@keyframes early-loading-slide {
  0% {
    transform: translateX(-100%);
  }
  100% {
    transform: translateX(350%);
  }
}

.terminal-state {
  text-align: center;
  font-size: 13px;
  font-weight: 600;
  color: rgb(var(--green-6));
}

.terminal-state.failed {
  color: rgb(var(--red-6));
}

.loading-detail {
  font-size: 12px;
  color: var(--color-text-3);
  text-align: center;
  word-break: break-all;
}

.loading-detail.failed {
  color: rgb(var(--red-6));
}

.compat-area {
  border-top: 1px solid var(--color-border-2);
  padding-top: 8px;
}

.loading-actions {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 12px 16px;
  border-top: 1px solid var(--color-border-2);
}
</style>
