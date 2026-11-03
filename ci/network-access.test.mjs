import assert from 'node:assert/strict'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { createServer } from 'vite'
import { validNetworkRanges } from '../src/utils/network-ranges.ts'

test('CIDR 白名单接受企业公网与私网前缀，并拒绝无效或合计覆盖全网的规则', () => {
  assert.ok(validNetworkRanges(['192.0.2.0/24', 'fd00::/64', '2001:db8::/48']))
  assert.ok(validNetworkRanges(['::ffff:192.0.2.3/128', '192.168.0.1/32']))
  for (const rules of [[], ['999.0.0.0/24'], ['::1/129'], ['0.0.0.0/1', '128.0.0.0/1'], ['::/1', '8000::/1']]) {
    assert.equal(validNetworkRanges(rules), false)
  }
})

test('预览使用本次运行的配置与令牌，旧回调和普通编辑不能覆盖新配置', async () => {
  const server = await createServer({
    configFile: false,
    resolve: { alias: { '@': fileURLToPath(new URL('../src', import.meta.url)) } },
    server: { middlewareMode: true, hmr: false, watch: null },
    optimizeDeps: { noDiscovery: true }, logLevel: 'silent',
  })
  const records = new Map()
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'localStorage')
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
    getItem: (key) => records.get(key) ?? null, setItem: (key, value) => records.set(key, value),
  } })
  try {
    const { api } = await server.ssrLoadModule('/src/api/index.ts')
    const [old] = await api.listInstances()
    const id = old.id
    const access = { mode: 'proxy', address: null, public_url: 'https://old.example/ui', networks: [], trusted_hosts: [], tls: null }
    await api.setInstanceWebAccess(id, access, 3080)
    await api.startInstance(id, 'web')
    await api.stopInstance(id)
    await api.setInstanceWebAccess(id, { ...access, public_url: 'https://new.example/ui' }, 3081)
    await api.updateInstance({ ...old, name: old.name + '-test' })
    const current = (await api.listInstances()).find((instance) => instance.id === id)
    assert.equal(current.web_access.public_url, 'https://new.example/ui/')
    assert.equal(current.port, 3081)
    await api.startInstance(id, 'web')
    await delay(1650)
    const status = (await api.listInstanceStatus()).find((item) => item.id === id)
    assert.equal(status.url, 'https://new.example/ui/')
    const link = new URL(await api.getInstanceShareUrl(id))
    assert.equal(link.host, 'new.example')
    assert.ok(link.searchParams.get('token'))
    await api.stopInstance(id)
    await assert.rejects(api.getInstanceShareUrl(id))
    await api.startInstance(id, 'web')
    await delay(1650)
    assert.notEqual(await api.getInstanceShareUrl(id), link.href)
    const copy = await api.copyInstance({ source_id: id, name: old.name + '-copy', new_home: false })
    assert.equal(copy.web_access?.mode ?? 'local', 'local')
    assert.equal(copy.port, 3081)
    await api.stopInstance(id)
  } finally {
    if (previous) {
      Object.defineProperty(globalThis, 'localStorage', previous)
    } else {
      delete globalThis.localStorage
    }
    await server.close()
  }
})
