import { computed, onBeforeUnmount, ref, watch, type Ref } from 'vue'

/**
 * Progressive list rendering (issue #50): rendering thousands of rich rows
 * in one synchronous pass blocks the renderer for seconds. This composable
 * exposes a `visible` slice that starts at `batchSize` and grows by
 * `batchSize` per animation frame until it covers the source list, so the
 * page paints immediately and stays responsive while the rest streams in.
 *
 * The slice resets whenever the source list identity changes (new fetch,
 * filter edit), and growth stops when the component unmounts.
 */
export function useProgressiveList<T>(source: Ref<T[]>, batchSize = 50) {
  const limit = ref(batchSize)
  let rafId = 0

  function cancel() {
    if (rafId) {
      cancelAnimationFrame(rafId)
      rafId = 0
    }
  }

  function schedule() {
    cancel()
    const step = () => {
      if (limit.value >= source.value.length) {
        rafId = 0
        return
      }
      limit.value += batchSize
      rafId = requestAnimationFrame(step)
    }
    rafId = requestAnimationFrame(step)
  }

  watch(
    () => source.value,
    () => {
      limit.value = batchSize
      schedule()
    },
    { immediate: true },
  )

  onBeforeUnmount(cancel)

  const visible = computed(() => source.value.slice(0, limit.value))
  /** True while more rows are still streaming in. */
  const growing = computed(() => limit.value < source.value.length)

  return { visible, growing }
}
