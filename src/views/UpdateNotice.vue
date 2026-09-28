<script setup lang="ts">
// Content of the frameless `update-notice` window: shown when the startup
// update check found a newer, unsuppressed launcher release. Not an in-app
// modal — a real OS window with a custom draggable title bar.
//
// The startup check hands its result over via the window URL query
// (`#/update-notice?version=…&url=…&published=…`) so the window renders
// immediately instead of re-hitting the GitHub API — a second fetch could
// race a freshly published release and disagree with the version the
// suppression logic just evaluated. A manual refresh re-checks on demand.
import { computed, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import type { LauncherUpdateInfo } from '@/api/types'
import FramelessTitleBar from '@/components/FramelessTitleBar.vue'

const { t } = useI18n()
const route = useRoute()

const checking = ref(false)
const checkError = ref('')
const info = ref<LauncherUpdateInfo | null>(null)
const busy = ref<'download' | 'never' | null>(null)

const upToDate = computed(() => !!info.value && info.value.up_to_date)

const publishedAt = computed(() => {
  const raw = info.value?.published_at
  if (!raw) return ''
  const date = new Date(raw)
  return Number.isNaN(date.getTime()) ? raw : date.toLocaleString()
})

function queryString(key: string): string {
  const v = route.query[key]
  return typeof v === 'string' ? v : ''
}

onMounted(() => {
  const version = queryString('version')
  if (version) {
    // The startup check already found this update; render it verbatim.
    info.value = {
      current: '',
      channel: 'dev',
      up_to_date: false,
      latest: version,
      url: queryString('url') || null,
      published_at: queryString('published') || null,
    }
    void fillCurrentVersion()
  } else {
    // Opened without a result (e.g. a leftover shortcut): check directly.
    void refresh()
  }
})

/** Fills the current version label without a network call. */
async function fillCurrentVersion() {
  try {
    const current = await api.getLauncherVersion()
    if (info.value) info.value.current = current
  } catch {
    /* keep the placeholder */
  }
}

/** Manual refresh: a real re-check on the remembered channel. */
async function refresh() {
  checking.value = true
  checkError.value = ''
  try {
    const settings = await api.getSettings()
    const result = await api.checkLauncherUpdate(settings.update_channel ?? 'dev')
    if (result.up_to_date) {
      // Nothing to notify about anymore: the notice is pointless.
      await closeWindow()
      return
    }
    info.value = result
  } catch (e) {
    checkError.value = String(e)
  } finally {
    checking.value = false
  }
}

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
      <div v-if="checking" class="notice-center">
        <a-spin :size="22" />
      </div>
      <div v-else-if="checkError" class="notice-center notice-error">
        <span>{{ checkError }}</span>
        <a-button size="small" @click="refresh">{{ t('updateNotice.retry') }}</a-button>
      </div>
      <template v-else-if="info && !upToDate">
        <div class="notice-version">
          <span class="version-label">{{ t('updateNotice.current') }}</span>
          <span class="version-value">{{ info.current ? `v${info.current}` : '—' }}</span>
        </div>
        <div class="version-arrow">↓</div>
        <div class="notice-version">
          <span class="version-label">{{ t('updateNotice.latest') }}</span>
          <span class="version-value version-new">v{{ info.latest }}</span>
          <a-tag v-if="info.latest?.includes('-')" color="orange" size="small">
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
        :disabled="!info?.url || checking || !!checkError"
        :loading="busy === 'download'"
        @click="onDownload"
      >
        {{ t('updateNotice.download') }}
      </a-button>
      <a-button :disabled="checking" @click="onSkip">
        {{ t('updateNotice.skip') }}
      </a-button>
      <a-button
        status="danger"
        type="text"
        :disabled="!info?.latest || checking || !!checkError"
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
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
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
