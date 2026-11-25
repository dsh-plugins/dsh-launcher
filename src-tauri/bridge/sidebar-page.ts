import type { BrowserBridge, PageState, SavedPage } from './sidebar-types'
import type { ViewportBounds } from './sidebar-layout'

function newPageId(): string {
  if (typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID()
  }
  // 局域网 HTTP 地址仍可使用 getRandomValues，但缺少 secure-context 的 randomUUID。
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 15) | 64
  bytes[8] = (bytes[8] & 63) | 128
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-')
}

/** 每个标签独享原生视图，窗口布局、地址变更和关闭按发送顺序执行。 */
export class SidebarPage {
  private readonly id = newPageId()
  private readonly listeners = new Set<() => void>()
  private pending = Promise.resolve()
  private target?: string
  private bounds: ViewportBounds | null = null
  private positioned = ''
  private created = false
  private closed = false
  private state: PageState = {}
  readonly subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }
  readonly snapshot = () => this.state

  constructor(private readonly bridge: BrowserBridge, private readonly save: (page: SavedPage) => void) {
    window.addEventListener('dsh-launcher-browser-state', this.receive)
  }

  private readonly receive = (event: Event) => {
    const detail = (event as CustomEvent<PageState & { id: string }>).detail
    if (!detail || detail.id !== this.id || this.closed) {
      return
    }
    this.update(detail)
    if (this.state.url && (detail.url || detail.title)) {
      this.save({ url: this.state.url, title: this.state.title || this.state.url })
    }
  }

  private update(change: PageState): void {
    this.state = { ...this.state, ...change }
    this.listeners.forEach(listener => listener())
  }

  private enqueue(action: () => Promise<void>): void {
    this.pending = this.pending.then(action).catch(error => {
      if (!this.closed) {
        this.update({ error: String(error), loading: false })
      } else {
        console.warn('关闭侧边栏页面失败', error)
      }
    })
  }

  private sync(): void {
    this.enqueue(async () => {
      if (this.closed) {
        return
      }
      const bounds = this.bounds
      if (!bounds) {
        if (this.created && this.positioned) {
          await this.bridge.send('hide', { id: this.id })
          this.positioned = ''
        }
        return
      }
      const geometry = JSON.stringify(bounds)
      const url = this.target
      if (url) {
        this.target = undefined
        if (!this.created) {
          await this.bridge.send('open', { id: this.id, url, ...bounds })
          this.created = true
          this.positioned = geometry
        } else {
          await this.bridge.send('navigate', { id: this.id, url })
        }
      }
      if (this.created && this.positioned !== geometry) {
        await this.bridge.send('layout', { id: this.id, ...bounds })
        this.positioned = geometry
      }
    })
  }

  navigate(url: string): void {
    this.target = url
    this.update({ url, loading: true, error: null })
    this.sync()
  }

  layout(bounds: ViewportBounds | null): void {
    if (JSON.stringify(bounds) !== JSON.stringify(this.bounds)) {
      this.bounds = bounds
      this.sync()
    }
  }

  command(action: 'back' | 'forward' | 'reload' | 'external'): void {
    this.enqueue(async () => {
      if (this.closed) {
        return
      }
      if (action === 'external' && this.state.url) {
        await this.bridge.send('external', { url: this.state.url })
      } else if (this.created) {
        await this.bridge.send(action, { id: this.id })
      } else if (action === 'reload' && this.state.url) {
        this.navigate(this.state.url)
      }
    })
  }

  close(): void {
    this.closed = true
    window.removeEventListener('dsh-launcher-browser-state', this.receive)
    this.enqueue(async () => {
      if (this.created) {
        await this.bridge.send('close', { id: this.id })
        this.created = false
      }
    })
  }
}
