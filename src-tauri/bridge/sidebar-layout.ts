export interface ViewportBounds {
  x: number
  y: number
  width: number
  height: number
  viewportWidth: number
}

const overlays = '[data-menu-popup], [role="menu"], [role="listbox"], [role="tooltip"], [data-dockkit-float]'

function visibleBounds(element: HTMLElement): ViewportBounds | null {
  if (document.hidden || !element.isConnected || document.querySelector('[data-dockkit-pointer], [role="dialog"][aria-modal="true"]:not([hidden])')) {
    return null
  }
  const rect = element.getBoundingClientRect()
  let left = Math.max(rect.left, 0)
  let top = Math.max(rect.top, 0)
  let right = Math.min(rect.right, window.innerWidth)
  let bottom = Math.min(rect.bottom, window.innerHeight)
  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const style = getComputedStyle(parent)
    if (style.visibility !== 'visible' || style.display === 'none' || Number(style.opacity) === 0) {
      return null
    }
    const clip = parent.getBoundingClientRect()
    if (style.overflowX !== 'visible') {
      left = Math.max(left, clip.left)
      right = Math.min(right, clip.right)
    }
    if (style.overflowY !== 'visible') {
      top = Math.max(top, clip.top)
      bottom = Math.min(bottom, clip.bottom)
    }
  }
  const x = Math.ceil(left)
  const y = Math.ceil(top)
  const width = Math.floor(right) - x
  const height = Math.floor(bottom) - y
  if (width <= 0 || height <= 0) {
    return null
  }
  for (const overlay of document.querySelectorAll<HTMLElement>(overlays)) {
    if (overlay.contains(element) || !overlay.getClientRects().length || getComputedStyle(overlay).visibility !== 'visible') {
      continue
    }
    const box = overlay.getBoundingClientRect()
    const l = Math.max(left, box.left), r = Math.min(right, box.right)
    const t = Math.max(top, box.top), b = Math.min(bottom, box.bottom)
    if (l < r && t < b) {
      // 背后的浮动面板不会遮挡当前页面，提示气泡可能禁用了指针命中。
      const hit = document.elementFromPoint((l + r) / 2, (t + b) / 2)
      if (overlay.matches('[role="tooltip"]') || !hit || !element.contains(hit)) {
        return null
      }
    }
  }
  return { x, y, width, height, viewportWidth: window.innerWidth }
}

/** 窗口原生视图不参与网页合成，网页发生布局或遮挡变化后需要同步其可见范围。 */
export function observeViewport(element: HTMLElement, sync: (bounds: ViewportBounds | null) => void): () => void {
  let frame = 0
  const update = () => {
    if (!frame) {
      frame = requestAnimationFrame(() => {
        frame = 0
        sync(visibleBounds(element))
      })
    }
  }
  const resize = new ResizeObserver(update)
  resize.observe(element)
  const mutations = new MutationObserver(update)
  mutations.observe(document.body, { subtree: true, childList: true, attributes: true,
    attributeFilter: ['hidden', 'style', 'class', 'open', 'aria-modal', 'role', 'data-dockkit-pointer', 'data-menu-popup'] })
  const events = new AbortController()
  const options = { capture: true, signal: events.signal }
  for (const event of ['resize', 'scroll', 'transitionend', 'animationend']) {
    window.addEventListener(event, update, options)
  }
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      sync(null)
    } else {
      update()
    }
  }, options)
  sync(visibleBounds(element))
  return () => {
    cancelAnimationFrame(frame)
    resize.disconnect()
    mutations.disconnect()
    events.abort()
    sync(null)
  }
}
