import type { BrowserBridge } from './sidebar-types'

interface Observable<T> {
  getSnapshot(): T
}

interface SessionBinding {
  session: Observable<{ openState: string }>
  eventSource: Observable<{ entries: readonly { event: { type: string; data: { turn?: number } } }[] }>
}

interface Context {
  sessions: {
    list: Observable<{ byId: Record<string, { id: string; retainedBy: Record<string, number> }> }>
    binding(id: string): SessionBinding | undefined
  }
  effect(callback: () => () => void, label: string): void
}

export const name = '@dsh-launcher/notifications'
export const inject = ['sessions']

/** 用已渲染会话的轮次位置确认已读，避免打开其他会话时清除提醒。 */
export function apply(ctx: Context): void {
  ctx.effect(() => {
    let stopReporting: (() => void) | undefined
    const activate = () => {
      const bridge = (window as Window & { __DSH_LAUNCHER_BROWSER__?: BrowserBridge }).__DSH_LAUNCHER_BROWSER__
      if (bridge?.version !== 1 || typeof bridge.acknowledgeSession !== 'function') {
        return
      }
      window.removeEventListener('dsh-launcher-browser-ready', activate)
      stopReporting = trackReading(ctx.sessions, bridge)
    }
    window.addEventListener('dsh-launcher-browser-ready', activate)
    activate()
    return () => {
      window.removeEventListener('dsh-launcher-browser-ready', activate)
      stopReporting?.()
    }
  }, 'launcher-notifications: 确认当前会话已查看')
}

function trackReading(sessions: Context['sessions'], bridge: BrowserBridge): () => void {
  let frame = 0
  let previousEvents: ReturnType<SessionBinding['eventSource']['getSnapshot']> | undefined
  let completedTurn = 0
  let sending = false
  const isViewing = () => document.visibilityState === 'visible' && document.hasFocus()
  const report = () => {
    if (!isViewing()) {
      return
    }
    cancelAnimationFrame(frame)
    frame = requestAnimationFrame(async () => {
      if (!isViewing() || sending) {
        return
      }
      const selected = Object.values(sessions.list.getSnapshot().byId)
        .find(row => (row.retainedBy.mainView ?? 0) > 0)
      if (!selected) {
        return
      }
      const binding = sessions.binding(selected.id)
      if (binding?.session.getSnapshot().openState !== 'open') {
        return
      }
      const events = binding.eventSource.getSnapshot()
      if (events !== previousEvents) {
        previousEvents = events
        completedTurn = 0
        for (const { event } of events.entries) {
          if (event.type === 'turn/end') {
            completedTurn = Math.max(completedTurn, event.data.turn ?? 0)
          } else if (event.type === 'turn/start') {
            // 历史窗口可能从新一轮开始，前一轮的结束事件已不在窗口内。
            completedTurn = Math.max(completedTurn, (event.data.turn ?? 0) - 1)
          }
        }
      }
      sending = true
      try {
        await bridge.acknowledgeSession(selected.id, completedTurn)
      } catch (error) {
        console.warn('[launcher-notifications] 发送会话已读回执失败', error)
      } finally {
        sending = false
      }
    })
  }
  // 同一轮内可能出现新的待回答问题，定时确认当前仍在查看的会话。
  const timer = window.setInterval(report, 500)
  window.addEventListener('focus', report)
  document.addEventListener('visibilitychange', report)
  report()
  return () => {
    window.clearInterval(timer)
    cancelAnimationFrame(frame)
    window.removeEventListener('focus', report)
    document.removeEventListener('visibilitychange', report)
  }
}
