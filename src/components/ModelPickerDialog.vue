<script setup lang="ts">
// Model picker for the models page (issue #89): mirrors DSH's "fetch available
// models" dialog — the endpoint's list comes in, the user searches and ticks
// what they want, and "add selected" hands the ids back. Nothing is written
// here: the parent merges them into its form and only a save persists them.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type { CatalogModel } from '@/api/types'
import { HintIcon } from '@dsh-plugin/dsh-ui-components-vue'

const props = defineProps<{
  visible: boolean
  loading: boolean
  models: CatalogModel[]
  /** Ids already selected, so re-opening appends instead of resetting. */
  selected?: string[]
  emptyText?: string
}>()

const emit = defineEmits<{
  'update:visible': [boolean]
  adopt: [string[]]
}>()

const { t } = useI18n()

const query = ref('')
const checked = ref<string[]>([])

watch(
  () => props.visible,
  (open) => {
    if (!open) return
    query.value = ''
    checked.value = [...(props.selected ?? [])]
  },
)

const filtered = computed(() => {
  const needle = query.value.trim().toLowerCase()
  if (!needle) return props.models
  return props.models.filter(
    (m) => m.id.toLowerCase().includes(needle) || m.name.toLowerCase().includes(needle),
  )
})

function close() {
  emit('update:visible', false)
}

function selectAll() {
  const ids = new Set(checked.value)
  filtered.value.forEach((m) => ids.add(m.id))
  checked.value = [...ids]
}

function deselectAll() {
  const drop = new Set(filtered.value.map((m) => m.id))
  checked.value = checked.value.filter((id) => !drop.has(id))
}

function confirm() {
  emit('adopt', checked.value)
  close()
}
</script>

<template>
  <dsh-modal
    :visible="props.visible"
    :width="640"
    unmount-on-close
    @cancel="close"
  >
    <template #title>
      {{ t('instanceEdit.modelsFetchTitle') }}
      <HintIcon :content="t('instanceEdit.modelsFetchDescription')" />
    </template>
    <dsh-input-search
      v-model="query"
      :placeholder="t('instanceEdit.modelsFetchSearch')"
      allow-clear
      class="picker-search"
    />
    <div class="picker-toolbar">
      <dsh-button variant="ghost" size="sm" @click="selectAll">
        {{ t('instanceEdit.modelsFetchSelectAll') }}
      </dsh-button>
      <dsh-button variant="ghost" size="sm" @click="deselectAll">
        {{ t('instanceEdit.modelsFetchDeselectAll') }}
      </dsh-button>
    </div>
    <dsh-scrollbar style="max-height: 320px; overflow-y: auto">
      <dsh-spin v-if="props.loading" class="picker-loading" />
      <dsh-checkbox-group v-else v-model="checked" direction="vertical" class="picker-list">
        <dsh-checkbox v-for="model in filtered" :key="model.id" :value="model.id">
          <span class="picker-id">{{ model.id }}</span>
          <span v-if="model.name && model.name !== model.id" class="picker-name">
            {{ model.name }}
          </span>
        </dsh-checkbox>
      </dsh-checkbox-group>
      <dsh-empty
        v-if="!props.loading && filtered.length === 0"
        :description="query ? t('instanceEdit.modelsFetchNoMatches') : (props.emptyText ?? t('instanceEdit.modelsFetchEmpty'))"
      />
    </dsh-scrollbar>
    <template #footer>
      <span class="picker-count">{{ t('instanceEdit.modelsFetchSelected', { count: checked.length }) }}</span>
      <dsh-button variant="secondary" @click="close">{{ t('instanceEdit.modelsCancel') }}</dsh-button>
      <dsh-button variant="primary" :disabled="checked.length === 0" @click="confirm">
        {{ t('instanceEdit.modelsFetchAdopt') }}
      </dsh-button>
    </template>
  </dsh-modal>
</template>

<style scoped lang="scss">
.picker-search {
  margin-bottom: 8px;
}

.picker-toolbar {
  display: flex;
  gap: 4px;
  margin-bottom: 4px;
}

.picker-list {
  width: 100%;
}

.picker-id {
  font-family: var(--font-family-code, monospace);
}

.picker-name {
  margin-left: 8px;
  color: var(--color-text-3);
}

.picker-loading {
  display: block;
  margin: 24px auto;
}

.picker-count {
  margin-right: auto;
  color: var(--color-text-3);
}
</style>
