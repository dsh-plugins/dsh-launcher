<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import InstanceAccessLinks from '@/components/InstanceAccessLinks.vue'
import { useLauncherStore } from '@/stores/launcher'
import type { DshInstance, InstanceState } from '@/api/types'
import ModpackImportDialog from '@/components/ModpackImportDialog.vue'
import HintIcon from '@/components/HintIcon.vue'

const { message: Message } = useFeedback()

const router = useRouter()
const { t } = useI18n()
const store = useLauncherStore()

const modpackImportVisible = ref(false)

/**
 * Homes that exist in the config but have no instances at all (issue #39,
 * problem 2): the auto-registered `~/.dsh` (and any home registered through
 * the import wizard without picking profiles) would otherwise be invisible
 * in instance management. Shown as a hint above the table.
 */
const homesWithoutInstances = computed(() => {
  const usedHomeIds = new Set(store.instances.map((i) => i.home_id))
  return store.homes.filter((h) => !usedHomeIds.has(h.id))
})

const columns = computed(() => [
  { title: t('instances.table.name'), slotName: 'name', width: 220 },
  { title: t('instances.table.version'), slotName: 'version', width: 140 },
  { title: t('instances.table.home'), slotName: 'home', width: 180 },
  { title: t('instances.table.profile'), slotName: 'profile', width: 120 },
  // No width: the status cell (state tag + external port + URL) sizes to its
  // content and stays on one line (issue #96).
  { title: t('instances.table.status'), slotName: 'status' },
  // The action column is pinned right via sticky CSS (see the style block):
  // it never scrolls away (issue #13) and sizes to its buttons (issue #96),
  // without forcing Arco's fixed table layout onto the columns above.
  { title: t('instances.table.actions'), slotName: 'actions', align: 'center' as const },
])

// --- Instance icons (issue #8): resolved lazily per instance -------------------

const iconMap = ref<Record<string, string | null>>({})

async function loadIcons() {
  const next: Record<string, string | null> = {}
  for (const inst of store.instances) {
    if (!inst.icon) {
      next[inst.id] = null
      continue
    }
    try {
      next[inst.id] = await api.readInstanceIcon(inst.id)
    } catch {
      next[inst.id] = null
    }
  }
  iconMap.value = next
}

watch(
  () => store.instances.map((i) => `${i.id}:${i.icon ?? ''}`).join(','),
  loadIcons,
  { immediate: true },
)

function stateColor(state: InstanceState): string {
  switch (state) {
    case 'running':
      return 'green'
    case 'starting':
      return 'orange'
    case 'exited':
      return 'red'
    default:
      return 'gray'
  }
}

async function onDelete(id: string, name: string) {
  try {
    await api.deleteInstance(id)
    await store.refreshInstances()
    Message.success(t('instances.deleted'))
  } catch (e) {
    Message.error(String(e))
  }
}

// --- Copy instance ---------------------------------------------------------

const copySource = ref<DshInstance | null>(null)
const copyName = ref('')
const copyNewHome = ref(false)
const copyHomeName = ref('')
const copying = ref(false)

function openCopy(inst: DshInstance) {
  copySource.value = inst
  copyName.value = `${inst.name} 副本`
  copyNewHome.value = false
  copyHomeName.value = `${inst.name} 副本`
}

function closeCopy() {
  copySource.value = null
  copyName.value = ''
  copyNewHome.value = false
  copyHomeName.value = ''
}

const copyValid = computed(
  () =>
    copyName.value.trim().length > 0 &&
    !!copySource.value &&
    (!copyNewHome.value || copyHomeName.value.trim().length > 0),
)

async function confirmCopy() {
  if (!copySource.value || !copyValid.value) return
  copying.value = true
  try {
    if (copyNewHome.value) {
      // Full-content copy (sessions, plugins, profiles, …) runs as a
      // background task; stay on this page and play the fly animation.
      await api.startCopyInstanceTask({
        source_id: copySource.value.id,
        name: copyName.value.trim(),
        home_name: copyHomeName.value.trim(),
      })
      await store.refreshTasks()
      store.notifyTaskQueued()
      Message.success(t('instances.copyTaskAdded', { name: copyName.value.trim() }))
    } else {
      const created = await api.copyInstance({
        source_id: copySource.value.id,
        name: copyName.value.trim(),
        new_home: false,
      })
      Message.success(t('instances.copied', { name: created.name }))
    }
    await store.refreshInstances()
    closeCopy()
  } catch (e) {
    Message.error(String(e))
  } finally {
    copying.value = false
  }
}

</script>

<template>
  <div class="dl-page">
    <div class="dl-card">
      <div class="dl-card-title">
        <h3>{{ t('instances.title') }}</h3>
        <div class="dl-toolbar">
          <dsh-button variant="secondary" @click="modpackImportVisible = true">
            {{ t('modpack.importButton') }}
          </dsh-button>
          <dsh-button variant="primary" @click="router.push({ name: 'download' })">
            {{ t('instances.newInstance') }}
          </dsh-button>
        </div>
      </div>

      <div class="table-scroll">
        <dsh-table :columns="columns" :data="store.instances" :pagination="false" row-key="id">
        <template #name="{ record }">
          <span class="inst-name">
            <img v-if="iconMap[record.id]" :src="iconMap[record.id]!" class="inst-icon" alt="" />
            <img v-else src="@/assets/launcher-icon-v3.png" class="inst-icon" alt="" />
            {{ record.name }}
            <dsh-tag v-if="store.homeById(record.home_id)?.wsl" size="small" color="arcoblue">
              WSL · {{ store.homeById(record.home_id)!.wsl }}
            </dsh-tag>
          </span>
        </template>
        <template #version="{ record }">
          {{ store.versionById(record.version_id)?.version ?? record.version_id }}
        </template>
        <template #home="{ record }">
          <dsh-tooltip :content="store.homeById(record.home_id)?.path">
            <span>{{ store.homeById(record.home_id)?.name ?? record.home_id }}</span>
          </dsh-tooltip>
        </template>
        <template #profile="{ record }">
          {{ record.last_profile ?? record.default_profile ?? '—' }}
        </template>
        <template #status="{ record }">
          <span class="cell-nowrap">
          <dsh-tag :color="stateColor(store.statusOf(record.id).state)">
            {{ t(`home.status.${store.statusOf(record.id).state}`) }}
          </dsh-tag>
          <dsh-tooltip
            v-if="store.externalOf(record.id)"
            :content="t('home.externalHint', { port: store.externalOf(record.id)!.port })"
          >
            <dsh-tag size="small" color="arcoblue">
              {{ t('home.statusExternal') }} :{{ store.externalOf(record.id)!.port }}
            </dsh-tag>
          </dsh-tooltip>
          <InstanceAccessLinks :status="store.statusOf(record.id)" />
          </span>
        </template>
        <template #actions="{ record }">
          <dsh-space>
            <dsh-button variant="secondary"
              size="sm"
              @click="router.push({ name: 'instance-edit', params: { id: record.id } })"
            >
              {{ t('instances.table.edit') }}
            </dsh-button>
            <dsh-button variant="secondary" size="sm" @click="openCopy(record)">
              {{ t('instances.table.copy') }}
            </dsh-button>
            <dsh-popconfirm
              :content="t('instances.confirmDelete', { name: record.name })"
              @ok="onDelete(record.id, record.name)"
            >
              <dsh-button variant="danger" size="sm">{{ t('instances.table.delete') }}</dsh-button>
            </dsh-popconfirm>
          </dsh-space>
        </template>
        <template #empty>
          <dsh-empty :description="t('instances.emptyDesc')">
            <template #image>
              <div class="empty-title">{{ t('instances.emptyTitle') }}</div>
            </template>
            <dsh-button variant="primary" @click="router.push({ name: 'download' })">
              {{ t('instances.newInstance') }}
            </dsh-button>
          </dsh-empty>
        </template>
        </dsh-table>
      </div>

      <div v-if="homesWithoutInstances.length > 0" class="homes-without-instances">
        <span class="homes-without-instances-title">{{ t('instances.homesWithoutInstancesTitle') }}</span>
        <span v-for="h in homesWithoutInstances" :key="h.id" class="homes-without-instances-item">
          <dsh-tag size="small" color="arcoblue">{{ h.name }}</dsh-tag>
        </span>
        <dsh-link href="#" @click.prevent="router.push({ name: 'settings' })">
          {{ t('instances.homesWithoutInstancesAction') }}
        </dsh-link>
      </div>
    </div>

    <!-- Copy instance dialog: name it first, then duplicate on save -->
    <dsh-modal
      :visible="!!copySource"
      :title="t('instances.copyTitle')"
      :ok-text="t('instanceEdit.save')"
      :cancel-text="t('instanceEdit.cancel')"
      :ok-button-props="{ disabled: !copyValid, loading: copying }"
      @ok="confirmCopy"
      @cancel="closeCopy"
    >
      <dsh-form layout="vertical" :model="{}">
        <dsh-form-item :label="t('instances.copyNameLabel')" required>
          <dsh-input v-model="copyName" :placeholder="t('instances.copyNamePlaceholder')" />
        </dsh-form-item>
        <dsh-form-item>
          <template #label>
            {{ t('instances.copyHomeLabel') }}
            <HintIcon :content="t('instances.copyHint')" />
          </template>
          <dsh-radio-group v-model="copyNewHome" type="button">
            <dsh-radio :value="false">{{ t('instances.copyHomeReuse') }}</dsh-radio>
            <dsh-radio :value="true">{{ t('instances.copyHomeNew') }}</dsh-radio>
          </dsh-radio-group>
        </dsh-form-item>
        <dsh-form-item v-if="copyNewHome" :label="t('instances.copyHomeNameLabel')" required>
          <dsh-input v-model="copyHomeName" :placeholder="t('instances.copyHomeNamePlaceholder')" />
        </dsh-form-item>
      </dsh-form>
    </dsh-modal>

    <ModpackImportDialog v-model:visible="modpackImportVisible" />
  </div>
</template>

<style lang="scss" scoped>
// Horizontal-scroll wrapper: the table keeps auto layout so every column
// sizes to its content; when the total width overflows, this wrapper
// scrolls instead of squeezing columns.
.table-scroll {
  overflow-x: auto;
}

.cell-nowrap {
  white-space: nowrap;
}

// Pin the trailing action column with sticky cells riding the wrapper's
// horizontal scroll, so the buttons never scroll away. Arco's native fixed
// columns would force table-layout: fixed and undo the content sizing.
// Colors mirror Arco's own: header cells use --color-neutral-2, body cells
// sit on --color-bg-2.
.table-scroll :deep(.arco-table-th:last-child),
.table-scroll :deep(.arco-table-td:last-child) {
  position: sticky;
  right: 0;
  z-index: 1;
}

.table-scroll :deep(.arco-table-th:last-child) {
  background-color: var(--color-neutral-2);
}

.table-scroll :deep(.arco-table-td:last-child) {
  background-color: var(--color-bg-2);
}

// Arco's row hover recolors every td with the translucent --color-fill-1;
// on the pinned cell that lets the content scrolled underneath show
// through. Mirror Arco's native fixed-column treatment instead: keep the
// opaque base and paint the hover tint as a ::before overlay (its negative
// z-index sits above the cell background, below the buttons).
.table-scroll
  :deep(.arco-table-hover:not(.arco-table-dragging) .arco-table-tr:not(.arco-table-tr-empty):not(.arco-table-tr-summary):hover .arco-table-td:last-child) {
  background-color: var(--color-bg-2);
}

.table-scroll
  :deep(.arco-table-hover:not(.arco-table-dragging) .arco-table-tr:not(.arco-table-tr-empty):not(.arco-table-tr-summary):hover .arco-table-td:last-child)::before {
  position: absolute;
  top: 0;
  left: 0;
  z-index: -1;
  width: 100%;
  height: 100%;
  background-color: var(--color-fill-1);
  content: '';
}

.inst-name {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.inst-icon {
  width: 24px;
  height: 24px;
  border-radius: 6px;
  object-fit: cover;
  flex-shrink: 0;
}


.empty-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text-1);
}

.homes-without-instances {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 12px;
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--color-fill-2);
  font-size: 13px;
}

.homes-without-instances-title {
  color: var(--color-text-2);
}

.homes-without-instances-item {
  display: inline-flex;
}
</style>
