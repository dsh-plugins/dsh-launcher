/** Read-only Host bridge. No HTTP listener, filesystem cache, or UI dependency. */
export const name = 'launcher-exit-inspector'
export const inject = ['agents', 'jobs']

// Mirrors desktop-host/{update-tasks,quit-inspection}.ts. Failures are unknown,
// never idle. Inspect ALL live agents, including subagents and approval waits.
export async function inspect(ctx) {
  const agents = ctx.get('agents')
  const jobs = ctx.get('jobs')
  if (!agents || !jobs) throw new Error('task services unavailable')
  const live = agents.list()
  let scheduled = false
  for (const agent of live) {
    const activity = await ctx.waterfall('workspace/session-activity', { sessionId: agent.id }, () => Promise.resolve([]))
    if (activity.some(entry => entry.kind === 'schedule')) { scheduled = true; break }
  }
  const current = agents.list()
  const active = current.some(a => a.status === 'running' || a.inbox.nextTurn.length > 0 || a.inbox.nextStep.length > 0)
    || [undefined, ...current].some(a => jobs.list(a?.id).some(j => j.status === 'running' || j.status === 'stopping'))
  return { active, scheduled }
}

// Timestamp the START, not delivery; a slow waterfall or buffered pipe must
// never turn an old idle observation into a freshly authoritative sample.
export async function collectSample(ctx, now = Date.now, monotonic = () => performance.now()) {
  const sampledAt = now()
  const started = monotonic()
  try {
    const result = await inspect(ctx)
    return { version: 1, sampledAt, ...(monotonic() - started <= 1000 ? result : { unknown: true }) }
  } catch { return { version: 1, sampledAt, unknown: true } }
}

export function apply(ctx, config) {
  let disposed = false
  let busy = false
  const emit = value => {
    if (!disposed) process.stdout.write(`DSH_LAUNCHER_ACTIVITY:${config.nonce}:${JSON.stringify(value)}\n`)
  }
  const tick = async () => {
    if (busy || disposed) return
    busy = true
    try { emit(await collectSample(ctx)) }
    finally { busy = false }
  }
  // A hung inspection produces no fresh sample and expires in the launcher.
  const timer = setInterval(tick, 500)
  timer.unref()
  ctx.effect(() => () => { disposed = true; clearInterval(timer) })
  void tick()
}
