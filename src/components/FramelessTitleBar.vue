<script setup lang="ts">
// Custom title bar for frameless standalone windows (update notice,
// early-loading): a drag region on the left and a close button on the right.
import { computed } from 'vue'

withDefaults(
  defineProps<{
    title: string
    /** Show the close button (default true). */
    closable?: boolean
  }>(),
  { closable: true },
)

const emit = defineEmits<{ (e: 'close'): void }>()

const isTauri = computed(() => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window)

// Manual drag (same pattern as the main window header in App.vue): buttons
// and other interactive elements opt out via data-no-drag.
async function onBarMouseDown(e: MouseEvent) {
  if (!isTauri.value || e.button !== 0) return
  const el = e.target as HTMLElement | null
  if (el?.closest('button, a, input, [data-no-drag]')) return
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  await getCurrentWindow().startDragging()
}

/** Closes this frameless window and lets the parent run its own hook first. */
async function onClose() {
  emit('close')
  if (!isTauri.value) return
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  await getCurrentWindow().close()
}
</script>

<template>
  <div class="frameless-titlebar" @mousedown="onBarMouseDown">
    <div class="bar-left">
      <slot name="icon" />
      <span class="bar-title">{{ title }}</span>
    </div>
    <button v-if="closable" class="bar-btn bar-close" title="关闭" @click="onClose">
      <svg viewBox="0 0 12 12" width="12" height="12">
        <path d="M1 1 L11 11 M11 1 L1 11" stroke="currentColor" stroke-width="1.4" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.frameless-titlebar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 36px;
  padding: 0 4px 0 12px;
  background: var(--color-bg-2);
  border-bottom: 1px solid var(--color-border-2);
  user-select: none;
  flex-shrink: 0;
}

.bar-left {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.bar-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.bar-btn {
  width: 32px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: transparent;
  color: var(--color-text-2);
  border-radius: 6px;
  cursor: pointer;
}

.bar-btn:hover {
  background: rgb(var(--red-6));
  color: #fff;
}
</style>
