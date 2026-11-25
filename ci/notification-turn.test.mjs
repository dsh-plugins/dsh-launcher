import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { runInNewContext } from 'node:vm'
import { apply } from '../src-tauri/bridge/client.ts'

test('普通浏览器即使打开会话也不启动已读回执', (t) => {
  let timer
  let frame
  let dispose
  const navigations = []
  globalThis.document = Object.assign(new EventTarget(), {
    visibilityState: 'visible',
    hasFocus: () => true,
  })
  globalThis.window = Object.assign(new EventTarget(), {
    setInterval(callback) { timer = callback; return 0 },
    location: { assign: url => navigations.push(url) },
  })
  globalThis.requestAnimationFrame = callback => { frame = callback; return 1 }
  globalThis.cancelAnimationFrame = () => { frame = undefined }
  t.after(() => {
    dispose?.()
    delete globalThis.document
    delete globalThis.window
    delete globalThis.requestAnimationFrame
    delete globalThis.cancelAnimationFrame
  })

  apply({
    sessions: {
      list: { getSnapshot: () => ({ byId: { a: { id: 'a', retainedBy: { mainView: 1 } } } }) },
      binding: () => ({
        session: { getSnapshot: () => ({ openState: 'open' }) },
        eventSource: { getSnapshot: () => ({ entries: [] }) },
      }),
    },
    effect: callback => { dispose = callback() },
  })
  window.dispatchEvent(new Event('focus'))
  for (let tick = 0; tick < 3; tick++) {
    timer?.()
    frame?.()
  }
  assert.deepEqual(navigations, [])
  assert.equal(timer, undefined)
})

test('内置窗口通过原生桥接确认当前可见会话的已读轮次', async (t) => {
  let focused = true
  let selected = 'a'
  const snapshots = { a: { entries: [] }, b: { entries: [] } }
  let frame
  let dispose
  let timer
  const receipts = []
  globalThis.document = Object.assign(new EventTarget(), {
    visibilityState: 'visible',
    hasFocus: () => focused,
  })
  globalThis.window = Object.assign(new EventTarget(), {
    setInterval(callback) {
      timer = callback
      return 1
    },
    clearInterval(handle) {
      assert.equal(handle, 1)
      timer = undefined
    },
    __TAURI_INTERNALS__: {
      invoke(command, args) {
        receipts.push({ command, args })
        return Promise.resolve()
      },
    },
  })
  globalThis.requestAnimationFrame = callback => {
    frame = callback
    return 1
  }
  globalThis.cancelAnimationFrame = () => { frame = undefined }
  t.after(() => {
    dispose?.()
    delete globalThis.document
    delete globalThis.window
    delete globalThis.requestAnimationFrame
    delete globalThis.cancelAnimationFrame
  })

  apply({
    sessions: {
      list: { getSnapshot: () => ({ byId: { [selected]: { id: selected, retainedBy: { mainView: 1 } } } }) },
      binding: id => ({
        session: { getSnapshot: () => ({ openState: 'open' }) },
        eventSource: { getSnapshot: () => snapshots[id] },
      }),
    },
    effect: callback => { dispose = callback() },
  })
  assert.equal(timer, undefined)
  const origin = 'http://127.0.0.1:4000'
  const script = readFileSync(new URL('../src-tauri/src/native_browser/bridge.js', import.meta.url), 'utf8')
    .replace('__ORIGIN__', JSON.stringify(origin))
    .replace('__TOKEN__', JSON.stringify('example-token'))
    .replace('__SUPPORTED__', 'false')
  window.top = window
  runInNewContext(script, { window, location: { origin }, Event })
  assert.equal(typeof timer, 'function')

  function expectReceipt(sessionId, turn) {
    assert.deepEqual(JSON.parse(JSON.stringify(receipts.splice(0))), [{
      command: 'plugin:sidebar-browser|send',
      args: { token: 'example-token', action: 'acknowledge-session', payload: { sessionId, turn } },
    }])
  }
  const entry = (type, turn) => ({ event: { type, data: { turn } } })
  for (const [entries, expected] of [
    [[], 0],
    [[entry('turn/start', 1)], 0],
    [[entry('turn/end', 2)], 2],
    [[entry('turn/start', 3)], 2],
    [[entry('turn/end', 2), entry('turn/start', 3), entry('assistant/message', 3)], 2],
    [[entry('turn/start', 3), entry('turn/end', 3)], 3],
  ]) {
    snapshots.a = { entries }
    timer()
    await frame()
    expectReceipt('a', expected)
  }

  for (const [id, turn] of [['b', 0], ['a', 3]]) {
    selected = id
    window.dispatchEvent(new Event('focus'))
    await frame()
    expectReceipt(id, turn)
  }

  window.dispatchEvent(new Event('focus'))
  focused = false
  await frame()
  assert.deepEqual(receipts, [])

  focused = true
  window.dispatchEvent(new Event('focus'))
  document.visibilityState = 'hidden'
  await frame()
  assert.deepEqual(receipts, [])

  dispose()
  dispose = undefined
  assert.equal(timer, undefined)
  assert.equal(frame, undefined)
})
