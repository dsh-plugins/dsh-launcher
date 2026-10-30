import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import vue from '@vitejs/plugin-vue'
import { createPinia } from 'pinia'
import { createSSRApp } from 'vue'
import { renderToString } from 'vue/server-renderer'
import { createI18n } from 'vue-i18n'
import { createMemoryHistory, createRouter } from 'vue-router'
import Arco from '@arco-design/web-vue'

let server
let useLauncherStore
let api
let originalFetch
let Market

before(async () => {
  server = await createServer({
    configFile: false,
    plugins: [vue()],
    optimizeDeps: { noDiscovery: true, include: [] },
    resolve: { alias: { '@': fileURLToPath(new URL('../src', import.meta.url)) } },
    server: { middlewareMode: true, hmr: false, watch: null },
    logLevel: 'error',
  })
  ;({ useLauncherStore } = await server.ssrLoadModule('/src/stores/launcher.ts'))
  ;({ api } = await server.ssrLoadModule('/src/api/index.ts'))
  ;({ default: Market } = await server.ssrLoadModule('/src/views/plugins/Market.vue'))
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

test('市场页面同时展示失败原因、缓存状态和已有插件', async () => {
  const pinia = createPinia()
  const store = useLauncherStore(pinia)
  store.marketWarnings = [
    { source_id: 'awesome-dsh-plugin', error: '响应超过容量上限', cached: true },
    { source_id: 'github-topic', error: '获取超时', cached: false },
  ]
  store.marketError = '示例整体请求失败'
  store.marketPlugins = [{ id: 'foo', name: '保留插件', source: 'foo', confidence: 'unverified' }]
  const messages = JSON.parse(await readFile(new URL('../src/locales/zh-CN.json', import.meta.url), 'utf8'))
  const app = createSSRApp(Market)
  app.use(pinia)
  app.use(Arco.default)
  app.use(createI18n({ legacy: false, locale: 'zh-CN', messages: { 'zh-CN': messages } }))
  app.use(createRouter({ history: createMemoryHistory(), routes: [] }))
  globalThis.requestAnimationFrame = () => 0
  try {
    const html = await renderToString(app)
    assert.match(html, /Awesome 精选 刷新失败/)
    assert.match(html, /响应超过容量上限/)
    assert.match(html, /最近成功获取的缓存/)
    assert.match(html, /GitHub Topic（社区） 刷新失败/)
    assert.match(html, /获取超时/)
    assert.match(html, /暂无可用缓存/)
    assert.match(html, /示例整体请求失败/)
    assert.match(html, /保留插件/)
  } finally {
    delete globalThis.requestAnimationFrame
  }
})
