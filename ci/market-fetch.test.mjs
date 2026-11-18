import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createPinia } from 'pinia'

let server
let useLauncherStore
let api
let originalFetch

before(async () => {
  server = await createServer({
    configFile: false,
    optimizeDeps: { noDiscovery: true, include: [] },
    resolve: { alias: { '@': fileURLToPath(new URL('../src', import.meta.url)) } },
    server: { middlewareMode: true, hmr: false, watch: null },
    logLevel: 'error',
  })
  ;({ useLauncherStore } = await server.ssrLoadModule('/src/stores/launcher.ts'))
  ;({ api } = await server.ssrLoadModule('/src/api/index.ts'))
  originalFetch = api.fetchPluginMarket
})

after(async () => {
  if (api) {
    api.fetchPluginMarket = originalFetch
  }
  await server?.close()
})

test('来源警告与目录一起保存，成功重试清除警告，整体失败保留目录', async () => {
  const store = useLauncherStore(createPinia())
  const plugins = [{ id: 'foo', name: '示例插件' }]
  const warnings = [{ source_id: 'awesome-dsh-plugin', error: '请求失败', cached: true }]
  api.fetchPluginMarket = async () => ({ plugins, warnings })
  await store.refreshMarketPlugins(true)
  assert.deepEqual(store.marketPlugins, plugins)
  assert.deepEqual(store.marketWarnings, warnings)
  assert.equal(store.marketLoading, false)

  api.fetchPluginMarket = async () => { throw new Error('示例连接错误') }
  await store.refreshMarketPlugins(true)
  assert.match(store.marketError, /示例连接错误/)
  assert.deepEqual(store.marketPlugins, plugins)
  assert.deepEqual(store.marketWarnings, warnings)

  api.fetchPluginMarket = async () => ({ plugins, warnings: [] })
  await store.refreshMarketPlugins(true)
  assert.deepEqual(store.marketWarnings, [])
  assert.equal(store.marketError, null)
})
