<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Scrollbar, type ScrollbarInstance } from '@dsh-plugin/dsh-ui-components-vue'

const props = defineProps<{ horizontal?: boolean }>()

const scrollbar = ref<ScrollbarInstance | null>(null)
const scrolling = ref(false)
const topShadow = ref(false)
const bottomShadow = ref(false)
let hideTimer: ReturnType<typeof setTimeout> | undefined
let resizeObserver: ResizeObserver | undefined
let mutationObserver: MutationObserver | undefined

function updateShadows() {
  if (props.horizontal) return
  const container = scrollbar.value?.$el?.querySelector('.arco-scrollbar-container') as HTMLElement | undefined
  if (!container) return
  topShadow.value = container.scrollTop > 1
  bottomShadow.value = container.scrollHeight - container.clientHeight - container.scrollTop > 1
}

onMounted(() => {
  if (props.horizontal) return
  const container = scrollbar.value?.$el?.querySelector('.arco-scrollbar-container') as HTMLElement | undefined
  if (!container) return
  resizeObserver = new ResizeObserver(updateShadows)
  const observeContent = () => {
    resizeObserver?.disconnect()
    resizeObserver?.observe(container)
    for (const child of container.children) resizeObserver?.observe(child)
    updateShadows()
  }
  observeContent()
  mutationObserver = new MutationObserver(observeContent)
  mutationObserver.observe(container, { childList: true })
})

function onScroll() {
  updateShadows()
  scrolling.value = true
  clearTimeout(hideTimer)
  hideTimer = setTimeout(() => { scrolling.value = false }, 900)
}

onBeforeUnmount(() => {
  clearTimeout(hideTimer)
  resizeObserver?.disconnect()
  mutationObserver?.disconnect()
})
defineExpose({ scrollTop: (top: number) => scrollbar.value?.scrollTop(top) })
</script>

<template>
  <Scrollbar
    ref="scrollbar"
    type="embed"
    :outer-class="['page-scrollbar', { 'is-scrolling': scrolling, 'has-top-shadow': topShadow, 'has-bottom-shadow': bottomShadow }]"
    :outer-style="horizontal ? undefined : 'height: 100%'"
    :style="horizontal ? 'overflow-x: auto; overflow-y: hidden' : 'height: 100%; overflow-y: auto'"
    :disable-vertical="horizontal"
    @scroll="onScroll"
  >
    <slot />
  </Scrollbar>
</template>

<style>
.page-scrollbar.arco-scrollbar::before,
.page-scrollbar.arco-scrollbar::after {
  content: '';
  position: absolute;
  left: 0;
  right: 8px;
  height: 24px;
  z-index: 2;
  pointer-events: none;
  mask-image: linear-gradient(to right, transparent, #000 32px, #000 calc(100% - 32px), transparent);
  opacity: 0;
  transition: opacity 150ms;
}

.page-scrollbar.arco-scrollbar::before {
  top: 0;
  background: linear-gradient(to bottom, var(--dl-scroll-shadow), transparent);
}

.page-scrollbar.arco-scrollbar::after {
  bottom: 0;
  background: linear-gradient(to top, var(--dl-scroll-shadow), transparent);
}

.page-scrollbar.arco-scrollbar.has-top-shadow::before,
.page-scrollbar.arco-scrollbar.has-bottom-shadow::after {
  opacity: 1;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track {
  background: transparent;
  border: 0;
  opacity: 0;
  transition: opacity 150ms;
}

.page-scrollbar.arco-scrollbar:hover > .arco-scrollbar-track,
.page-scrollbar.arco-scrollbar.is-scrolling > .arco-scrollbar-track,
.page-scrollbar.arco-scrollbar:has(.arco-scrollbar-thumb-dragging) > .arco-scrollbar-track {
  opacity: 1;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track::after {
  display: none;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track-direction-vertical {
  width: 8px;
  right: 0;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track .arco-scrollbar-thumb {
  opacity: 1;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track .arco-scrollbar-thumb-bar {
  background: var(--color-fill-4);
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track-direction-vertical .arco-scrollbar-thumb-bar {
  width: 6px;
  margin: 0 1px;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track-direction-horizontal {
  height: 8px;
}

.page-scrollbar.arco-scrollbar > .arco-scrollbar-track-direction-horizontal .arco-scrollbar-thumb-bar {
  height: 6px;
  margin: 1px 0;
}
</style>
