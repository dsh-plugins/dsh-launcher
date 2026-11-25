import type { ComponentType } from 'react'

/** 原生接口由启动器注入当前实例的主页面。 */
export interface BrowserBridge {
  version: number
  native: boolean
  send(action: string, payload: Record<string, string | number>): Promise<void>
}

/** 原生页只向所属窗口报告导航和标题。 */
export interface PageState {
  url?: string
  title?: string
  canGoBack?: boolean
  canGoForward?: boolean
  loading?: boolean
  error?: string | null
}

export interface SavedPage {
  url: string
  title: string
}

export interface BrowserProps {
  sessionId: string
  t: (key: string) => string
  useTabInfo(): { tab: {
    id: string
    title: string
    visible: boolean
    signal: AbortSignal
    navigation: { params?: { url?: string } }
    actions: { bindCommands(commands: { refresh(): void }): () => void }
  } }
  useStore<T>(select: (state: { pages: Record<string, SavedPage> }) => T): T
  actions: { save(id: string, page: SavedPage): void; forget(id: string): void }
}

export type MenuTarget = { kind: 'launcherExternalUrl'; value: { url: string } }

/** 临时插件仅声明当前调用的 DSH 客户端公开接口。 */
export interface Context {
  inject(services: string[], install: (ctx: Context) => void): void
  effect(install: () => () => void): void
  locale: {
    bind(namespace: string): (key: string) => string
    register(namespace: string, locales: Record<string, Record<string, string>>): () => void
  }
  sidebarRight: {
    openTabs: { getSnapshot(): readonly { sessionId: string; tabId: string }[] }
  }
  sidebarRightTabs: {
    register(definition: {
      id: string; kind: string; priority: 'extension'; multiple: boolean; keepMounted: boolean
      title: () => string; guide: { id: string; order: number; title: () => string }[]
    }): () => void
  }
  slots: {
    inject(seat: string, install: () => () => void): () => void
    register(options: { name: string; key: string; locale: string; store: unknown },
      component: ComponentType<BrowserProps>): () => void
  }
  contextMenus: {
    register(options: { id: string; target: MenuTarget['kind']; order: number; resolve(value: MenuTarget['value']): {
      id: string; label: string; disabled?: boolean; run?: () => Promise<void>; failureLabel?: string
    }[] }): () => void
    open(target: MenuTarget, point: { x: number; y: number }): void
    notify(message: string, success?: boolean): void
  }
}
