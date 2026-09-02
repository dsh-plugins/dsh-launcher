import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  // Hash history keeps the Tauri webview happy without server rewrites.
  history: createWebHashHistory(),
  routes: [
    { path: '/', name: 'home', component: () => import('@/views/Home.vue') },
    { path: '/instances', name: 'instances', component: () => import('@/views/Instances.vue') },
    { path: '/instances/:id', name: 'instance-edit', component: () => import('@/views/InstanceEdit.vue') },
    { path: '/modpack/export', name: 'modpack-export', component: () => import('@/views/ExportModpack.vue') },
    {
      path: '/download',
      component: () => import('@/views/Download.vue'),
      children: [
        { path: '', name: 'download', redirect: { name: 'download-create' } },
        { path: 'create', name: 'download-create', component: () => import('@/views/download/CreatePick.vue') },
        { path: 'create/:version', name: 'download-name', component: () => import('@/views/download/NameInstance.vue') },
        { path: 'plugins', name: 'download-plugins', component: () => import('@/views/plugins/Market.vue') },
        { path: 'plugins/version', name: 'plugin-version', component: () => import('@/views/plugins/VersionPick.vue') },
        { path: 'plugins/install', name: 'plugin-install', component: () => import('@/views/plugins/InstallWizard.vue') },
      ],
    },
    { path: '/settings', name: 'settings', component: () => import('@/views/Settings.vue') },
    { path: '/tasks', name: 'tasks', component: () => import('@/views/Tasks.vue') },
    { path: '/setup', name: 'setup', component: () => import('@/views/Setup.vue') },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})

export default router
