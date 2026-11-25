import assert from 'node:assert/strict'
import { test } from 'node:test'
import { apply } from '../src-tauri/bridge/client.ts'

test('已读回执只确认前台实际看到的已结束轮次', (t) => {
  let focused = true
  let selected = 'a'
  const snapshots = { a: { entries: [] }, b: { entries: [] } }
  let frame
  let dispose
  const receipts = []
  globalThis.document = Object.assign(new EventTarget(), {
    visibilityState: 'visible',
    hasFocus: () => focused,
  })
  globalThis.window = Object.assign(new EventTarget(), {
    setInterval: () => 0,
    location: { assign: url => receipts.push(Object.fromEntries(new URL(url).searchParams)) },
  })
  globalThis.requestAnimationFrame = callback => {
    frame = callback
    return 1
  }
  globalThis.cancelAnimationFrame = () => { frame = undefined }
  t.after(() => {
    dispose()
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
  const entry = (type, turn) => ({ event: { type, data: { turn } } })
  for (const [entries, expected] of [
    [[], '0'],
    [[entry('turn/start', 1)], '0'],
    [[entry('turn/end', 2)], '2'],
    [[entry('turn/start', 3)], '2'],
    [[entry('turn/end', 2), entry('turn/start', 3), entry('assistant/message', 3)], '2'],
    [[entry('turn/start', 3), entry('turn/end', 3)], '3'],
  ]) {
    snapshots.a = { entries }
    window.dispatchEvent(new Event('focus'))
    frame()
    assert.deepEqual(receipts.splice(0), [{ id: 'a', turn: expected }])
  }

  for (const [id, turn] of [['b', '0'], ['a', '3']]) {
    selected = id
    window.dispatchEvent(new Event('focus'))
    frame()
    assert.deepEqual(receipts.splice(0), [{ id, turn }])
  }

  window.dispatchEvent(new Event('focus'))
  focused = false
  frame()
  assert.deepEqual(receipts, [])

  focused = true
  window.dispatchEvent(new Event('focus'))
  document.visibilityState = 'hidden'
  frame()
  assert.deepEqual(receipts, [])
})
