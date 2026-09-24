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
