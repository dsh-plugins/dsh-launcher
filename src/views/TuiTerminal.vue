<script setup lang="ts">
// Full-page PTY terminal for one TUI instance (issue #31), hosted in the
// dedicated `tui-<id>` window opened by the backend. Mirrors the xterm.js
// wiring of TerminalEmbed.vue but drives the TUI session commands
// (`start_tui_session` / `write_tui_input` / `resize_tui_session`) and the
// `tui://data` stream.
//
// Lifecycle: the backend registered the instance as Starting when
// `start_instance` opened this window; mounting here spawns the PTY and the
// first output flips the instance to Running. Closing the window only
// detaches the view — the session keeps running (stop from Home / tray).

import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'

const props = defineProps<{
  instanceId: string
}>()

const { t } = useI18n()

const containerRef = ref<HTMLElement | null>(null)
const starting = ref(false)

let term: Terminal | null = null
let fitAddon: FitAddon | null = null
let dataUn: (() => void) | null = null
let disposed = false

// --- helpers -----------------------------------------------------------------

function b64decode(s: string): string {
  const bin = atob(s)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder().decode(bytes)
}

function b64encode(s: string): string {
  const bytes = new TextEncoder().encode(s)
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin)
}

function writeInput(data: string) {
  api
    .writeTuiInput({ instanceId: props.instanceId, data: b64encode(data) })
    .catch((e) => Message.error(String(e)))
}

// --- session lifecycle -------------------------------------------------------

async function startSession() {
  if (!term || disposed) return
  starting.value = true
  try {
    await api.startTuiSession({
      instanceId: props.instanceId,
      cols: term.cols,
      rows: term.rows,
    })
  } catch (e) {
    // A live session from a previous window (reattach) reports "already
    // exists" — that is fine, just keep streaming.
    const msg = String(e)
    if (!msg.includes('已存在')) {
      Message.error(msg)
      term?.write(`\r\n\x1b[31m${msg}\x1b[0m\r\n`)
    }
  } finally {
    starting.value = false
  }
}

function onResize() {
  if (!term || disposed) return
  api
    .resizeTuiSession({ instanceId: props.instanceId, cols: term.cols, rows: term.rows })
    .catch(() => {
      /* session may have just exited */
    })
}

// --- xterm lifecycle ---------------------------------------------------------

onMounted(async () => {
  await new Promise((r) => setTimeout(r, 50))
  if (disposed || !containerRef.value) return
  const el = containerRef.value

  term = new Terminal({
    cursorBlink: true,
    fontSize: 13,
    fontFamily: 'Consolas, "Courier New", monospace',
    theme: { background: '#1e1e1e', foreground: '#d4d4d4' },
    scrollback: 5000,
  })
  fitAddon = new FitAddon()
  term.loadAddon(fitAddon)
  term.open(el)
  fitAddon.fit()

  term.onData((d) => writeInput(d))

  dataUn = await api.onTuiData((p) => {
    if (p.instanceId === props.instanceId && term && !disposed) {
      term.write(b64decode(p.data))
    }
  })

  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      if (!fitAddon || !term || disposed) return
      try {
        fitAddon.fit()
        onResize()
      } catch {
        /* container hidden */
      }
    })
    ro.observe(el)
  }
  window.addEventListener('resize', onResize)

  await startSession()
})

onBeforeUnmount(() => {
  disposed = true
  window.removeEventListener('resize', onResize)
  dataUn?.()
  dataUn = null
  term?.dispose()
  term = null
  fitAddon = null
})
</script>

<template>
  <div class="tui-terminal">
    <div ref="containerRef" class="tui-terminal-box"></div>
    <div v-if="starting" class="tui-terminal-hint">{{ t('tui.starting') }}</div>
  </div>
</template>

<style scoped>
.tui-terminal {
  position: fixed;
  inset: 0;
  display: flex;
  background: #1e1e1e;
}

.tui-terminal-box {
  flex: 1;
  padding: 6px 8px;
  min-width: 0;
}

.tui-terminal-hint {
  position: absolute;
  top: 8px;
  right: 12px;
  color: #86909c;
  font-size: 12px;
  pointer-events: none;
}
</style>
