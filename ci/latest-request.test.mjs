import assert from 'node:assert/strict'
import { test } from 'node:test'
import { latestRequest } from '../src/utils/latest-request.ts'

test('a late response from a previous profile cannot become current', () => {
  const requests = latestRequest()
  const web = requests.begin()
  requests.invalidate()
  const tui = requests.begin()
  assert.equal(requests.isCurrent(web), false)
  assert.equal(requests.isCurrent(tui), true)
})

test('editing plugins invalidates an in-flight check', () => {
  const requests = latestRequest()
  const check = requests.begin()
  requests.invalidate()
  assert.equal(requests.isCurrent(check), false)
})

test('leaving a scope and returning cannot revive its abandoned request', () => {
  const requests = latestRequest()
  const original = requests.begin()
  requests.invalidate() // Leave the scope/tab.
  requests.invalidate() // Return to the same selected value.
  assert.equal(requests.isCurrent(original), false)
})

test('an old request finally block cannot clear a newer request busy state', () => {
  const requests = latestRequest()
  const first = requests.begin()
  requests.invalidate()
  const second = requests.begin()
  let busy = true
  if (requests.isCurrent(first)) busy = false
  assert.equal(busy, true)
  if (requests.isCurrent(second)) busy = false
  assert.equal(busy, false)
})
