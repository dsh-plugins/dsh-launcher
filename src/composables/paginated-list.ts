import { computed, watch, type Ref } from 'vue'

/** Bound mounted rows, including in KeepAlive caches. Appending a batch on
 * every animation frame still eventually renders and updates the whole catalog. */
export function usePaginatedList<T>(source: Ref<T[]>, page: Ref<number>) {
  const pageSize = 50

  // A new catalog or filter starts at the first page, including when a
  // previous last page would no longer exist. KeepAlive preserves the page
  // when navigating away without changing the source.
  watch(source, () => { page.value = 1 })

  const visible = computed(() => {
    const start = (page.value - 1) * pageSize
    return source.value.slice(start, start + pageSize)
  })

  return { visible, pageSize }
}
