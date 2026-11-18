<script setup lang="ts">
// Embedded PTY terminal for one instance: xterm.js frontend wired to the
// Rust terminal sessions (start/write/resize/close commands + data/status
// events). The `dsh` shim is injected on the backend, so typing `dsh ...`
// drives the current instance's own DSH CLI.
//
// The parent mounts this component with `:key="instanceId"`, so switching
// instances tears the whole component down (disposing the xterm and closing
// the previous session) and mounts a fresh one.

import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'

const { message: Message } = useFeedback()

const props = defineProps<{
  instanceId: string
}>()

const emit = defineEmits<{
  (e: 'status', running: boolean): void
}>()

const { t } = useI18n()

const containerRef = ref<HTMLElement | null>(null)
const starting = ref(false)

let term: Terminal | null = null
let fitAddon: FitAddon | null = null
let dataUn: (() => void) | null = null
let statusUn: (() => void) | null = null
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
    .writeTerminalInput({ instanceId: props.instanceId, data: b64encode(data) })
    .catch((e) => Message.error(String(e)))
}

// --- session lifecycle -------------------------------------------------------

async function startSession() {
  if (!term || disposed) return
  starting.value = true
  try {
    await api.startTerminalSession({
      instanceId: props.instanceId,
      cols: term.cols,
      rows: term.rows,
    })
    emit('status', true)
  } catch (e) {
    Message.error(String(e))
  } finally {
    starting.value = false
  }
}

function onResize() {
  if (!term || disposed) return
  api
    .resizeTerminalSession({ instanceId: props.instanceId, cols: term.cols, rows: term.rows })
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

  dataUn = await api.onTerminalData((p) => {
    if (p.instanceId === props.instanceId && term && !disposed) {
      term.write(b64decode(p.data))
    }
  })
  statusUn = await api.onTerminalStatus((p) => {
    if (p.instanceId !== props.instanceId) return
    emit('status', p.running)
    if (!p.running && term && !disposed) {
      term.write('\r\n\x1b[31m[session exited]\x1b[0m\r\n')
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

  await startSession()
})

onBeforeUnmount(() => {
  disposed = true
  dataUn?.()
  dataUn = null
  statusUn?.()
  statusUn = null
  api.closeTerminalSession({ instanceId: props.instanceId }).catch(() => {})
  term?.dispose()
  term = null
  fitAddon = null
})

defineExpose({
  restart: () => {
    api.closeTerminalSession({ instanceId: props.instanceId }).catch(() => {})
    startSession()
  },
})
</script>

<template>
  <div class="terminal-embed">
    <div class="terminal-toolbar">
      <dsh-tag :color="starting ? 'arcoblue' : 'green'" size="small">
        {{ starting ? t('instanceEdit.terminalStarting') : t('instanceEdit.terminalRunning') }}
      </dsh-tag>
      <dsh-button variant="secondary" size="sm" :loading="starting" @click="startSession">
        {{ t('instanceEdit.terminalRestart') }}
      </dsh-button>
    </div>
    <div ref="containerRef" class="terminal-container"></div>
  </div>
</template>

<style scoped>
.terminal-embed {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 320px;
  border: 1px solid var(--color-border-2);
  border-radius: 20px;
  overflow: hidden;
  background: #1e1e1e;
}

.terminal-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 10px;
  background: #252526;
  border-bottom: 1px solid #333;
}

.terminal-container {
  flex: 1;
  min-height: 0;
  padding: 4px;
}
</style>
