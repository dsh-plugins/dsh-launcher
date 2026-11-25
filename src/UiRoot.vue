<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { DshProvider } from '@dsh-plugin/dsh-ui-components-vue'
import enUS from '@arco-design/web-vue/es/locale/lang/en-us'
import zhCN from '@arco-design/web-vue/es/locale/lang/zh-cn'
import { useLauncherStore } from '@/stores/launcher'
import { api } from '@/api'
import App from './App.vue'

const store = useLauncherStore()
const { locale } = useI18n()
const media = window.matchMedia('(prefers-color-scheme: dark)')
const systemDark = ref(media.matches)
const theme = computed(() => store.settings.theme === 'system'
  ? (systemDark.value ? 'dark' : 'light')
  : store.settings.theme)
const uiLocale = computed(() => locale.value === 'en-US' ? enUS : zhCN)
const useWindowShadow = !api.isTauri || /Windows/.test(navigator.userAgent)

function onSystemThemeChange(event: MediaQueryListEvent) {
  systemDark.value = event.matches
}

media.addEventListener('change', onSystemThemeChange)
onBeforeUnmount(() => media.removeEventListener('change', onSystemThemeChange))

watch(theme, (value) => {
  if (value === 'dark') {
    document.body.setAttribute('arco-theme', 'dark')
  } else {
    document.body.removeAttribute('arco-theme')
  }
  document.documentElement.style.colorScheme = value
}, { immediate: true })
</script>

<template>
  <DshProvider :theme="theme" :locale="uiLocale" radius="md" class="launcher-ui" :class="{ 'launcher-window-shadow': useWindowShadow }">
    <App :window-shadow="useWindowShadow" />
  </DshProvider>
</template>
