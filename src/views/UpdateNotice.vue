<script setup lang="ts">
// Content of the frameless `update-notice` window: shown when the startup
// update check found a newer, unsuppressed launcher release. Not an in-app
// modal — a real OS window with a custom draggable title bar.
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { LauncherUpdateInfo } from '@/api/types'
import FramelessTitleBar from '@/components/FramelessTitleBar.vue'

const { t } = useI18n()

const loading = ref(true)
const error = ref('')
const info = ref<LauncherUpdateInfo | null>(null)
const busy = ref<'download' | 'never' | null>(null)
const publishedAt = ref('')

onMounted(async () => {
  try {
    const settings = await api.getSettings()
    info.value = await api.checkLauncherUpdate(settings.update_channel ?? 'dev')
    if (info.value.up_to_date) {
      error.value = t('settings.update.upToDate')
    } else {
      const raw = info.value.published_at
      if (raw) {
        const date = new Date(raw)
        publishedAt.value = Number.isNaN(date.getTime()) ? raw : date.toLocaleString()
      }
    }
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
})

async function closeWindow() {
  if (api.isTauri) {
    const { getCurrentWindow } = await import('@tauri-apps/api/window')
    await getCurrentWindow().close()
  }
}

/** 前往下载：open the release page in the system browser, then close. */
async function onDownload() {
  if (!info.value?.url) return
  busy.value = 'download'
  try {
    await api.openExternal(info.value.url)
    await closeWindow()
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = null
  }
}

/** 忽略本次：close only; the next startup check may show it again. */
async function onSkip() {
  await closeWindow()
}

/** 永不提醒：suppress exactly this version, then close. */
async function onNever() {
  const version = info.value?.latest
  if (!version) return
  busy.value = 'never'
  try {
    await api.dismissUpdateVersion(version)
    await closeWindow()
  } catch (e) {
    Message.error(String(e))
  } finally {
    busy.value = null
  }
}
</script>

<template>
  <div class="update-notice">
    <FramelessTitleBar :title="t('updateNotice.title')" @close="onSkip" />

    <div class="notice-body">
      <div v-if="loading" class="notice-center">
        <a-spin :size="22" />
      </div>
      <div v-else-if="error" class="notice-center notice-error">
        <span>{{ error }}</span>
      </div>
      <template v-else-if="info">
        <div class="notice-version">
          <span class="version-label">{{ t('updateNotice.current') }}</span>
          <span class="version-value">v{{ info.current }}</span>
        </div>
        <div class="version-arrow">↓</div>
        <div class="notice-version">
          <span class="version-label">{{ t('updateNotice.latest') }}</span>
          <span class="version-value version-new">v{{ info.latest }}</span>
          <a-tag v-if="info.channel === 'dev'" color="orange" size="small">
            {{ t('settings.update.channel.dev') }}
          </a-tag>
          <a-tag v-else color="green" size="small">
            {{ t('settings.update.channel.release') }}
          </a-tag>
        </div>
        <div v-if="publishedAt" class="notice-date">
          {{ t('updateNotice.publishedAt', { date: publishedAt }) }}
        </div>
      </template>
    </div>

    <div class="notice-actions">
      <a-button
        type="primary"
        :disabled="!info?.url || loading || !!error"
        :loading="busy === 'download'"
        @click="onDownload"
      >
        {{ t('updateNotice.download') }}
      </a-button>
      <a-button :disabled="loading" @click="onSkip">
        {{ t('updateNotice.skip') }}
      </a-button>
      <a-button
        status="danger"
        type="text"
        :disabled="!info?.latest || loading || !!error"
        :loading="busy === 'never'"
        @click="onNever"
      >
        {{ t('updateNotice.never') }}
      </a-button>
    </div>
  </div>
</template>

<style scoped>
.update-notice {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: var(--color-bg-1);
}

.notice-body {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 0;
  padding: 16px 20px;
}

.notice-center {
  display: flex;
  align-items: center;
  justify-content: center;
}

.notice-error {
  color: rgb(var(--red-6));
  font-size: 13px;
  text-align: center;
}

.notice-version {
  display: flex;
  align-items: center;
  gap: 8px;
}

.version-label {
  font-size: 13px;
  color: var(--color-text-3);
}

.version-value {
  font-size: 16px;
  font-weight: 600;
  color: var(--color-text-1);
}

.version-new {
  color: rgb(var(--green-6));
}

.version-arrow {
  color: var(--color-text-3);
  font-size: 14px;
  line-height: 1;
}

.notice-date {
  font-size: 12px;
  color: var(--color-text-3);
}

.notice-actions {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 12px 16px;
  border-top: 1px solid var(--color-border-2);
}
</style>
