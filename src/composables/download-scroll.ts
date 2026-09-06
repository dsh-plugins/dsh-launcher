import { inject, provide } from 'vue'

/** Scope key used by the download pages' shared scrollbar accessor. */
const DOWNLOAD_SCROLL_KEY = Symbol('download-scroll')

/**
 * Callback that reads the current scrollTop of the shared download scrollbar
 * OR sets it to a given pixel offset. The child views (e.g. the plugin market)
 * use this to persist/restore their scroll position across navigations.
 */
export type DownloadScrollAccessor = (top: number | null) => number | void

/** Provided by Download.vue; other pages consume it via injectDownloadScroll. */
export function provideDownloadScroll(accessor: DownloadScrollAccessor) {
  provide(DOWNLOAD_SCROLL_KEY, accessor)
}

export function injectDownloadScroll(): DownloadScrollAccessor | null {
  return inject<DownloadScrollAccessor | null>(DOWNLOAD_SCROLL_KEY, null)
}