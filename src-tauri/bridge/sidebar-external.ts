import type { BrowserBridge, Context } from './sidebar-types'

/** 旧系统只在启动器的聊天正文接管 HTTP(S) 链接，复用 DSH 的键盘菜单。 */
export function installExternalLinks(ctx: Context, bridge: BrowserBridge): void {
  const menus = ctx.contextMenus
  const t = ctx.locale.bind('open-in-app')
  ctx.effect(() => menus.register({
    id: 'launcher.external', target: 'launcherExternalUrl', order: 100,
    resolve: ({ url }) => [
      { id: 'sidebar', label: t('menu.openInSidebar'), disabled: true },
      { id: 'external', label: t('menu.external'), run: () => bridge.send('external', { url }) },
      { id: 'copy', label: t('menu.copyLink'), failureLabel: t('menu.copyFailed'), run: async () => {
        await navigator.clipboard.writeText(url)
        menus.notify(t('menu.copied'), true)
      } },
    ],
  }))
  ctx.effect(() => {
    const events = new AbortController()
    const options = { capture: true, signal: events.signal }
    function link(event: Event): HTMLAnchorElement | undefined {
      const anchor = event.target instanceof Element
        ? event.target.closest<HTMLAnchorElement>('[data-chat-flow] a[target="_blank"][href]') : null
      if (anchor && (anchor.protocol === 'https:' || anchor.protocol === 'http:')) {
        return anchor
      }
    }
    function menu(event: MouseEvent | KeyboardEvent): void {
      const anchor = link(event)
      if (!anchor || event.defaultPrevented) {
        return
      }
      event.preventDefault()
      event.stopPropagation()
      anchor.focus({ preventScroll: true })
      const rect = anchor.getBoundingClientRect()
      const point = event instanceof MouseEvent && (event.clientX || event.clientY)
        ? { x: event.clientX, y: event.clientY } : { x: rect.left, y: rect.bottom }
      menus.open({ kind: 'launcherExternalUrl', value: { url: anchor.href } }, point)
    }
    document.addEventListener('click', event => {
      const anchor = link(event)
      if (!anchor || event.defaultPrevented || event.button !== 0 || event.ctrlKey
        || event.altKey || event.metaKey || event.shiftKey) {
        return
      }
      event.preventDefault()
      event.stopPropagation()
      void bridge.send('external', { url: anchor.href }).catch(error => menus.notify(String(error)))
    }, options)
    document.addEventListener('contextmenu', menu, options)
    document.addEventListener('keydown', event => {
      if (!event.repeat && !event.isComposing && !event.ctrlKey && !event.altKey && !event.metaKey
        && (event.key === 'ContextMenu' || event.key === 'F10' && event.shiftKey)) {
        menu(event)
      }
    }, options)
    return () => events.abort()
  })
}
