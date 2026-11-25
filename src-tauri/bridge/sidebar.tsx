import { useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react'
import { defineStore } from '@deepseek-ai/dsh-client-store'
import { observeViewport } from './sidebar-layout'
import { SidebarPage } from './sidebar-page'
import { installExternalLinks } from './sidebar-external'
import type { BrowserBridge, BrowserProps, Context, PageState, SavedPage } from './sidebar-types'

export const name = '@dsh-launcher/sidebar-browser'
const namespace = 'launcherSidebarBrowser'
const labels = {
  zh: { browser: '浏览器', address: '输入 HTTP(S) 地址', back: '后退', forward: '前进', reload: '刷新',
    external: '在系统浏览器中打开', restore: '恢复上次页面', go: '前往', invalid: '请输入有效的 HTTP(S) 网页地址。' },
  en: { browser: 'Browser', address: 'Enter an HTTP(S) address', back: 'Back', forward: 'Forward', reload: 'Reload',
    external: 'Open in system browser', restore: 'Restore previous page', go: 'Go', invalid: 'Enter a valid HTTP(S) web address.' },
}
const styles = `
.dsh-launcher-browser {display:flex;flex:1 1 auto;flex-direction:column;min-width:0;min-height:0;height:100%;color:var(--dsw-alias-label-primary);background:var(--dsw-alias-bg-base)}
.dsh-launcher-browser form {display:flex;gap:4px;align-items:center;padding:5px 6px;border-bottom:1px solid var(--dsw-alias-border-l3)}
.dsh-launcher-browser input {flex:1;min-width:0;color:inherit;font:inherit;border:1px solid var(--dsw-alias-border-l3);border-radius:6px;padding:3px 6px;background:transparent}
.dsh-launcher-browser button {font:inherit;color:inherit;background:transparent;border:0;border-radius:5px;padding:3px 6px;cursor:pointer}
.dsh-launcher-browser button:hover:not(:disabled) {background:var(--dsw-alias-interactive-bg-hover)}
.dsh-launcher-browser button:disabled {opacity:.4;cursor:default}
.dsh-launcher-browser [role=alert] {margin:4px 8px;color:var(--dsw-alias-state-error-primary);overflow-wrap:anywhere}
.dsh-launcher-browser-viewport {flex:1 1 auto;min-height:0;display:grid;place-items:center}
`
const emptyState: PageState = {}
const noSubscribe = () => () => {}
const emptySnapshot = () => emptyState

function webAddress(value: string): string | undefined {
  const text = value.trim()
  if (!text || text.length > 16384) {
    return
  }
  const explicit = /^[A-Za-z][A-Za-z\d+.-]*:(?!\d+(?:[/?#]|$))/.test(text)
  try {
    const url = new URL(explicit ? text : `https://${text}`)
    if (['https:', 'http:'].includes(url.protocol) && !url.username && !url.password && url.origin !== location.origin) {
      return url.href
    }
  } catch {
    // 调用方在地址栏旁显示校验错误。
    return
  }
}

function registerBrowser(ctx: Context, bridge: BrowserBridge): void {
  ctx.effect(() => ctx.locale.register(namespace, labels))
  const translate = ctx.locale.bind(namespace)
  ctx.effect(() => ctx.sidebarRightTabs.register({
    id: name, kind: 'browser', priority: 'extension', multiple: true, keepMounted: true,
    title: () => translate('browser'), guide: [{ id: 'new', order: 30, title: () => translate('browser') }],
  }))
  type Store = { pages: Record<string, SavedPage> }
  const store = defineStore({
    init: (): Store => ({ pages: {} }),
    persist: 'dsh-launcher.sidebar-browser.v1',
    actions: {
      save(state: Store, id: string, page: SavedPage) { state.pages[id] = page },
      forget(state: Store, id: string) { delete state.pages[id] },
    },
  })
  const pages = new Map<AbortSignal, SidebarPage>()
  const lifetime = new AbortController()
  ctx.effect(() => {
    const style = document.createElement('style')
    style.textContent = styles
    document.head.append(style)
    return () => {
      lifetime.abort()
      style.remove()
      pages.forEach(page => page.close())
      pages.clear()
    }
  })

  function BrowserBody({ useTabInfo, useStore, actions, t, sessionId }: BrowserProps) {
    const { tab } = useTabInfo()
    const saved = useStore(state => state.pages[tab.id])
    const [page, setPage] = useState<SidebarPage>()
    const frame = useSyncExternalStore(page?.subscribe ?? noSubscribe, page?.snapshot ?? emptySnapshot)
    const viewport = useRef<HTMLDivElement>(null)
    const [draft, setDraft] = useState<string>()
    const [invalid, setInvalid] = useState(false)
    const lastAddress = frame.url ?? saved?.url ?? tab.navigation.params?.url ?? ''
    const initial = useRef({ saved, url: tab.navigation.params?.url })
    const id = tab.id
    const signal = tab.signal
    useLayoutEffect(() => {
      if (signal.aborted) {
        return
      }
      let active = pages.get(signal)
      if (!active) {
        active = new SidebarPage(bridge, data => actions.save(id, data))
        pages.set(signal, active)
        signal.addEventListener('abort', () => {
          const removed = pages.get(signal)
          pages.delete(signal)
          removed?.close()
          if (!ctx.sidebarRight.openTabs.getSnapshot().some(row => row.sessionId === sessionId && row.tabId === id)) {
            actions.forget(id)
          }
        }, { once: true, signal: lifetime.signal })
        if (!initial.current.saved && initial.current.url) {
          const url = webAddress(initial.current.url)
          if (url) {
            active.navigate(url)
          } else {
            setInvalid(true)
          }
        }
      }
      setPage(active)
    }, [id, signal, sessionId, actions])
    useLayoutEffect(() => {
      if (page && viewport.current && tab.visible) {
        return observeViewport(viewport.current, bounds => page.layout(bounds))
      }
    }, [page, tab.visible])
    const navigate = (address: string) => {
      const url = webAddress(address)
      setInvalid(!url)
      if (url && page) {
        setDraft(undefined)
        page.navigate(url)
      }
    }
    const reload = () => {
      if (frame.url) {
        page?.command('reload')
      } else if (lastAddress) {
        navigate(lastAddress)
      }
    }
    useLayoutEffect(() => tab.actions.bindCommands({ refresh: reload }), [tab.actions, page, frame.url, lastAddress])
    const tool = (key: string, icon: string, disabled: boolean, click: () => void) =>
      <button type="button" title={t(key)} aria-label={t(key)} disabled={disabled} onClick={click}>{icon}</button>
    return <section className="dsh-launcher-browser">
      <form onSubmit={event => { event.preventDefault(); navigate(draft ?? lastAddress) }}>
        {tool('back', '←', !frame.canGoBack, () => page?.command('back'))}
        {tool('forward', '→', !frame.canGoForward, () => page?.command('forward'))}
        {tool('reload', '↻', !page || !lastAddress, reload)}
        <input aria-label={t('address')} placeholder={t('address')} value={draft ?? lastAddress}
          onChange={event => { setDraft(event.target.value); setInvalid(false) }} />
        <button type="submit" disabled={!page}>{t('go')}</button>
        {tool('external', '↗', !lastAddress, () => {
          void bridge.send('external', { url: lastAddress }).catch(error => ctx.contextMenus.notify(String(error)))
        })}
      </form>
      {(invalid || frame.error) && <p role="alert">{invalid ? t('invalid') : frame.error}</p>}
      <div ref={viewport} className="dsh-launcher-browser-viewport">
        {!frame.url && saved && <button type="button" onClick={() => navigate(saved.url)}>{t('restore')}</button>}
      </div>
    </section>
  }

  function BrowserTitle({ useTabInfo, useStore }: BrowserProps) {
    const { tab } = useTabInfo()
    const saved = useStore(state => state.pages[tab.id])
    return <>{saved?.title || tab.title}</>
  }
  for (const [seat, component] of [
    ['sidebar.right.pane.tab', BrowserBody], ['sidebar.right.pane.tab.title', BrowserTitle],
  ] as const) {
    ctx.effect(() => ctx.slots.inject(seat, () => ctx.slots.register({ name: seat, key: name,
      locale: namespace, store }, component)))
  }
}

/** 普通浏览器沿用实例自身配置，旧版 DSH 可独立加载通知插件。 */
export function apply(ctx: Context): void {
  if (window !== window.top) {
    return
  }
  ctx.effect(() => {
    const activate = () => {
      const bridge = (window as Window & { __DSH_LAUNCHER_BROWSER__?: BrowserBridge }).__DSH_LAUNCHER_BROWSER__
      if (!bridge || bridge.version !== 1) {
        return
      }
      window.removeEventListener('dsh-launcher-browser-ready', activate)
      if (bridge.native) {
        ctx.inject(['slots', 'locale', 'sidebarRight', 'sidebarRightTabs', 'contextMenus'], scope => registerBrowser(scope, bridge))
      } else {
        ctx.inject(['locale', 'contextMenus'], scope => installExternalLinks(scope, bridge))
      }
    }
    // 实例重新启动并换端口时，启动器在新主文档加载后安装桥。
    window.addEventListener('dsh-launcher-browser-ready', activate)
    activate()
    return () => window.removeEventListener('dsh-launcher-browser-ready', activate)
  })
}
