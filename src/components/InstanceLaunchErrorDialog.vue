<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Message } from '@arco-design/web-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'

const { t } = useI18n()
const store = useLauncherStore()

const visible = computed(() => !!store.launchError)
const instance = computed(() =>
  store.launchError ? store.instanceById(store.launchError.instanceId) : null,
)

/** Clipboard payload: summary + tail lines (if any). */
const summary = computed(() => {
  const err = store.launchError
  if (!err) return ''
  if (err.message) return err.message
  const name = instance.value?.name ?? err.instanceId
  if (err.exitCode != null) {
    return t('launchError.summaryExited', { name, code: err.exitCode })
  }
  return t('launchError.summaryDefault', { name })
})

const tail = ref<string[]>([])
const tailLoading = ref(false)

// Watch the error object itself (not the derived visibility): a second
// failure while the dialog is still open must refresh summary AND tail.
watch(
  () => store.launchError,
  async (err) => {
    tail.value = []
    if (!err) {
      tailLoading.value = false
      return
    }
    tailLoading.value = true
    try {
      // Flush race: the waiter emits Exited before the stdout/stderr readers
      // have necessarily written the final crash lines to the log file. Give
      // them a beat before the first read, then re-read once to catch
      // late-arriving lines. Bail out if the dialog was dismissed or a newer
      // error replaced this one meanwhile.
      await new Promise((r) => setTimeout(r, 350))
      if (store.launchError !== err) return
      tail.value = await api.readInstanceLogTail(err.instanceId, 40)
      await new Promise((r) => setTimeout(r, 800))
      if (store.launchError !== err) return
      tail.value = await api.readInstanceLogTail(err.instanceId, 40)
    } catch {
      tail.value = []
    } finally {
      if (store.launchError === err) tailLoading.value = false
    }
  },
  { immediate: true },
)

async function copyError() {
  const text = summary.value + (tail.value.length ? '\n\n' + tail.value.join('\n') : '')
  try {
    await navigator.clipboard.writeText(text)
    Message.success(t('launchError.copied'))
  } catch {
    Message.error(t('launchError.copyFailed'))
  }
}

async function revealLog() {
  const err = store.launchError
  if (!err) return
  try {
    await api.openInstanceLog(err.instanceId)
  } catch (e) {
    Message.error(String(e))
  }
}

function close() {
  store.dismissLaunchError()
}
</script>

<template>
  <a-modal
    :visible="visible"
    :title="t('launchError.title')"
    :closable="true"
    @cancel="close"
  >
    <template #footer>
      <a-button @click="copyError">{{ t('launchError.copy') }}</a-button>
      <a-button @click="revealLog">{{ t('launchError.revealLog') }}</a-button>
      <a-button type="primary" @click="close">{{ t('launchError.close') }}</a-button>
    </template>

    <div class="err-body">
      <a-alert type="error" :message="summary" />
      <div class="err-tail-title">{{ t('launchError.tailTitle') }}</div>
      <a-spin :loading="tailLoading" style="width: 100%">
        <pre v-if="tail.length" class="err-tail">{{ tail.join('\n') }}</pre>
        <div v-else class="err-tail-empty">{{ t('launchError.tailEmpty') }}</div>
      </a-spin>
    </div>
  </a-modal>
</template>

<style scoped>
.err-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.err-tail-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-1);
}

.err-tail {
  margin: 0;
  padding: 10px 12px;
  max-height: 260px;
  overflow: auto;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-all;
  color: var(--color-text-2);
  background: var(--color-fill-1);
  border-radius: 6px;
}

.err-tail-empty {
  padding: 14px 12px;
  font-size: 12px;
  color: var(--color-text-3);
  background: var(--color-fill-1);
  border-radius: 6px;
}
</style>
