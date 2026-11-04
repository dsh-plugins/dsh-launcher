import assert from 'node:assert/strict'
import { test } from 'node:test'
import { inspect, apply, collectSample } from '../src-tauri/assets/exit-inspector.mjs'

const agent = (id = 'parent', extra = {}) => ({ id, status: 'idle', inbox: { nextTurn: [], nextStep: [] }, ...extra })
function host(agents = [], jobs = {}, activity = {}) {
  return {
    get(name) { return name === 'agents' ? { list: () => agents } : { list: id => jobs[id ?? 'global'] ?? [] } },
    waterfall(_event, { sessionId }, _next) { return Promise.resolve(activity[sessionId] ?? []) },
  }
}
test('no agents and idle agents are not active merely because the Host runs', async () => {
  for (const agents of [[], [agent()]]) assert.deepEqual(await inspect(host(agents)), { active: false, scheduled: false })
})
test('generating, tool-running, approval waits and subagents use Host running status', async () => {
  assert.equal((await inspect(host([agent(), agent('child', { status: 'running' })]))).active, true)
})
test('both inbox queues count even for an idle agent', async () => {
  for (const queue of ['nextTurn', 'nextStep']) {
    const a = agent(); a.inbox[queue].push({}); assert.equal((await inspect(host([a]))).active, true)
  }
})
test('running/stopping global and agent jobs count; finished/error jobs do not', async () => {
  for (const owner of ['global', 'child']) for (const status of ['running', 'stopping']) {
    assert.equal((await inspect(host([agent('child')], { [owner]: [{ status }] }))).active, true)
  }
  assert.equal((await inspect(host([agent()], { global: [{ status: 'done' }, { status: 'error' }] }))).active, false)
})
test('only schedule activity of loaded agents is counted, independently from running work', async () => {
  const activity = { parent: [{ kind: 'schedule' }], cold: [{ kind: 'schedule' }] }
  assert.deepEqual(await inspect(host([agent()], {}, activity)), { active: false, scheduled: true })
  assert.deepEqual(await inspect(host([], {}, activity)), { active: false, scheduled: false })
  assert.deepEqual(await inspect(host([agent('parent', { status: 'running' })], {}, activity)), { active: true, scheduled: true })
})
test('missing services and failing activity never report idle', async () => {
  await assert.rejects(inspect({ get() {} }))
  const ctx = host([agent()]); ctx.waterfall = () => Promise.reject(new Error('failed'))
  await assert.rejects(inspect(ctx))
})
test('bridge emits versioned nonce-bound evidence and disposal suppresses late answers', async () => {
  const writes = []; const original = process.stdout.write; let dispose
  process.stdout.write = function (chunk, ...args) {
    if (typeof chunk === 'string' && chunk.startsWith('DSH_LAUNCHER_ACTIVITY:')) { writes.push(chunk); return true }
    return original.call(this, chunk, ...args)
  }
  try {
    const ctx = host(); ctx.effect = fn => { dispose = fn() }
    apply(ctx, { nonce: 'test-run' })
    await new Promise(resolve => setImmediate(resolve)); dispose()
    assert.equal(writes.length, 1)
    assert.match(writes[0], /^DSH_LAUNCHER_ACTIVITY:test-run:/)
    const frame = JSON.parse(writes[0].split(':').slice(2).join(':'))
    assert.equal(typeof frame.sampledAt, 'number')
    assert.deepEqual({ ...frame, sampledAt: 0 }, { version: 1, sampledAt: 0, active: false, scheduled: false })
    let resolve; const delayed = host([agent()]); delayed.effect = fn => { dispose = fn() }
    delayed.waterfall = () => new Promise(r => { resolve = r })
    apply(delayed, { nonce: 'late' }); dispose(); resolve([])
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(writes.length, 1)
  } finally { dispose?.(); process.stdout.write = original }
})

test('slow inspections resolve unknown instead of refreshing stale idle evidence', async () => {
  let elapsed = 0
  const ctx = host([agent()])
  ctx.waterfall = async () => { elapsed = 3000; return [] }
  assert.deepEqual(await collectSample(ctx, () => 1000, () => elapsed), { version: 1, sampledAt: 1000, unknown: true })
})
test('active work is re-read after asynchronous schedule inspection', async () => {
  const live = agent(); const ctx = host([live])
  ctx.waterfall = async () => { live.status = 'running'; return [] }
  assert.equal((await inspect(ctx)).active, true)
})
