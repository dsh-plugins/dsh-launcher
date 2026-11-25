<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { HintIcon, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import { useLauncherStore } from '@/stores/launcher'
import type { TaskInfo, TaskState } from '@/api/types'

const { message: Message } = useFeedback()

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const expanded = ref<Record<string, boolean>>({})
// a-scrollbar component instances expose scrollTo()/scrollTop().
interface ScrollbarExposed {
  scrollTo: (options: { top: number }) => void
}
const logRefs = ref<Record<string, ScrollbarExposed | null>>({})

onMounted(() => {
  // Refresh once on entry so tasks created before navigation are present.
  store.refreshTasks()
})

function stateColor(state: TaskState): string {
  switch (state) {
    case 'queued':
      return 'blue'
    case 'running':
      return 'orange'
    case 'done':
      return 'green'
    case 'error':
      return 'red'
    case 'cancelled':
      return 'gray'
  }
}

function setLogRef(id: string, el: unknown) {
  logRefs.value[id] = (el as ScrollbarExposed | null) ?? null
}

async function scrollToBottom(id: string) {
  await nextTick()
  // The browser clamps the offset to the real scroll height.
  logRefs.value[id]?.scrollTo({ top: Number.MAX_SAFE_INTEGER })
}

watch(
  () => store.tasks,
  () => {
    for (const task of store.taskList) {
      // Auto-expand newly arrived running tasks.
      if (task.state === 'running' && expanded.value[task.id] === undefined) {
        expanded.value[task.id] = true
      }
      if (expanded.value[task.id]) scrollToBottom(task.id)
    }
  },
  { deep: true },
)

function toggleExpand(id: string) {
  expanded.value[id] = !expanded.value[id]
  if (expanded.value[id]) scrollToBottom(id)
}

async function onCancel(id: string) {
  try {
    await api.cancelTask(id)
  } catch (e) {
    Message.error(String(e))
  }
}

async function onRemove(id: string) {
  try {
    await api.removeTask(id)
    await store.refreshTasks()
  } catch (e) {
    Message.error(String(e))
  }
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString()
}

function instanceName(task: TaskInfo): string | null {
  if (!task.instance_id) return null
  return store.instanceById(task.instance_id)?.name ?? null
}

const sortedTasks = computed(() => store.taskList)
</script>

<template>
  <div class="dl-page">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('tasks.title') }}</h3>
        <div class="dl-toolbar">
          <dsh-button variant="secondary" size="sm" @click="store.refreshTasks()">{{ t('common.refresh') }}</dsh-button>
          <dsh-button variant="ghost" size="sm" @click="router.back()">{{ t('download.back') }}</dsh-button>
        </div>
      </div>

      <div v-if="sortedTasks.length === 0" class="empty-tasks">
        <dsh-empty :description="t('tasks.empty')" />
      </div>

      <div v-for="task in sortedTasks" :key="task.id" class="task-card">
        <div class="task-head" @click="toggleExpand(task.id)">
          <div class="task-info">
            <div class="task-label">
              <template v-if="task.kind === 'download-version'">{{ t('tasks.downloadVersion', { version: task.version }) }}</template>
              <template v-else-if="task.kind === 'repair-version'">{{ t('tasks.repairVersion', { version: task.version }) }}</template>
              <template v-else-if="task.kind === 'download-cua'">{{ t('tasks.downloadCua', { version: task.version }) }}</template>
              <template v-else>{{ task.label }}</template>
            </div>
            <div class="task-meta">
              {{ formatTime(task.created_at) }}
              <template v-if="task.state === 'done' && task.kind !== 'install-plugin' && instanceName(task)">
                · {{ t('tasks.createdInstance', { name: instanceName(task) }) }}
              </template>
              <template v-if="task.message && (task.kind === 'download-cua' || task.state === 'error' || (task.state === 'done' && ['download-version', 'repair-version'].includes(task.kind)))">
                · {{ task.message }}
              </template>
            </div>
          </div>
          <dsh-tag class="task-state" :color="stateColor(task.state)" size="small">
            {{ t(`tasks.state.${task.state}`) }}
            <HintIcon v-if="task.state === 'queued'" :content="t('tasks.queuedHint')" />
          </dsh-tag>
          <div class="task-progress">
            <dsh-progress
              v-if="task.state === 'running'"
              :percent="task.percent / 100"
              size="small"
              :show-text="true"
            />
            <dsh-progress
              v-else-if="task.state === 'done'"
              :percent="1"
              size="small"
              status="success"
            />
            <dsh-progress
              v-else
              :percent="task.percent / 100"
              size="small"
              :status="task.state === 'error' ? 'danger' : 'normal'"
            />
          </div>
          <div v-if="task.kind !== 'download-cua' && task.state === 'running' && task.percent >= 90" class="installing-hint">
            {{ t('tasks.installingHint') }}
          </div>
          <div class="task-actions" @click.stop>
            <dsh-button variant="secondary"
              v-if="task.state === 'running' || task.state === 'queued'"
              size="sm"
              status="warning"
              @click="onCancel(task.id)"
            >
              {{ t('tasks.cancel') }}
            </dsh-button>
            <dsh-button variant="danger" v-else size="sm" @click="onRemove(task.id)">
              {{ t('tasks.remove') }}
            </dsh-button>
          </div>
          <span class="expand-icon">{{ expanded[task.id] ? '▾' : '▸' }}</span>
        </div>

        <div v-if="expanded[task.id]" class="task-log">
          <dsh-scrollbar
            :ref="(el: unknown) => setLogRef(task.id, el)"
            style="max-height: 280px; overflow-y: auto"
          >
            <pre><code>{{ task.logs.join('\n') || t('tasks.noLogs') }}</code></pre>
          </dsh-scrollbar>
        </div>
      </div>
    </div>
  </div>
</template>

<style lang="scss" scoped>
.empty-tasks {
  padding: 40px 0;
}

.task-card {
  border: 1px solid var(--color-border-2);
  border-radius: 8px;
  margin-bottom: 12px;
  overflow: hidden;
}

.task-head {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 12px 16px;
  cursor: pointer;
  user-select: none;

  &:hover {
    background: var(--color-fill-1);
  }
}

.task-info {
  flex: 1;
  min-width: 0;
}

.task-label {
  font-weight: 600;
  overflow-wrap: anywhere;
}

.task-meta {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 4px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.task-state,
.task-actions,
.expand-icon {
  flex-shrink: 0;
}

.task-progress {
  flex: 0 0 clamp(80px, 20%, 180px);
}

.installing-hint {
  font-size: 12px;
  color: rgb(var(--orange-6));
}

.expand-icon {
  color: var(--color-text-3);
  width: 16px;
}

.task-log {
  background: var(--surface-secondary);
  border-top: 1px solid var(--color-border-2);

  pre {
    margin: 0;
    padding: 12px 16px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--color-text-2);
    font-family: Consolas, 'Courier New', monospace;
    white-space: pre-wrap;
    word-break: break-all;
  }
}
</style>
