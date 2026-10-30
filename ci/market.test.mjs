import assert from 'node:assert/strict'
import { test } from 'node:test'
import { ref } from 'vue'
import { compatibility, indexedVersions, useCompatibilityAck } from '../src/utils/market.ts'

test('确认随插件、版本和实例组合变化失效', () => {
  const selection = ref('foo/1.0.0/instance-1')
  const accepted = useCompatibilityAck(selection)
  accepted.value = true
  assert.equal(accepted.value, true)
  for (const next of ['foo/2.0.0/instance-1', 'foo/2.0.0/instance-2', 'bar/2.0.0/instance-2']) {
    selection.value = next
    assert.equal(accepted.value, false)
    accepted.value = true
  }
})

test('兼容、不兼容和无法判断分别显示', () => {
  assert.equal(compatibility('[0.1.0-rc.6,0.2.0-rc.1)', '0.1.7-rc.2').status, 'compatible')
  assert.equal(compatibility('[0.1.0-rc.6,0.2.0-rc.1)', '0.2.1-rc.4').status, 'incompatible')
  assert.equal(compatibility('[foo,)', '1.0.0').status, 'unknown')
  assert.equal(compatibility('*', 'abcdef1').status, 'unknown')
})

test('官方版本保留固定来源和独立兼容范围', () => {
  const source = { type: 'git', url: 'https://github.com/foo/bar', commit: 'a'.repeat(40), path: 'packages/foo' }
  const versions = indexedVersions({ catalog: { versions: [
    { version: '1.0.0-rc.1', publishTimestamp: 100, source, engines: { dsh: '>=0.1.0' } },
  ] } })
  assert.deepEqual(versions[0].install_source, source)
  assert.equal(versions[0].support_versions, '>=0.1.0')
  assert.equal(versions[0].channel, 'beta')
  assert.equal(indexedVersions({ id: 'foo' }), null)
})
