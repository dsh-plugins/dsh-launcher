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
    let frame = 0
    let previousEvents: ReturnType<SessionBinding['eventSource']['getSnapshot']> | undefined
    let completedTurn = 0
    const report = () => {
      if (document.visibilityState !== 'visible' || !document.hasFocus()) {
        return
      }
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const selected = Object.values(ctx.sessions.list.getSnapshot().byId)
          .find(row => (row.retainedBy.mainView ?? 0) > 0)
        if (!selected) {
          return
        }
        const binding = ctx.sessions.binding(selected.id)
        if (binding?.session.getSnapshot().openState !== 'open') {
          return
        }
        const events = binding.eventSource.getSnapshot()
        if (events !== previousEvents) {
          previousEvents = events
          completedTurn = events.entries.reduce((latest, { event }) => event.type === 'turn/end'
            ? Math.max(latest, event.data.turn ?? 0) : latest, 0)
        }
        const url = new URL('dsh-launcher-read://session')
        url.searchParams.set('id', selected.id)
        url.searchParams.set('turn', String(completedTurn))
        window.location.assign(url.href)
      })
    }
    const timer = window.setInterval(report, 500)
    window.addEventListener('focus', report)
    document.addEventListener('visibilitychange', report)
    return () => {
      clearInterval(timer)
      cancelAnimationFrame(frame)
      window.removeEventListener('focus', report)
      document.removeEventListener('visibilitychange', report)
    }
  }, 'launcher-notifications: 确认当前会话已查看')
}
