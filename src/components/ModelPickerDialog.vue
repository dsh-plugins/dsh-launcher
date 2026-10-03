<script setup lang="ts">
// Model picker for the models page (issue #89): mirrors DSH's "fetch available
// models" dialog — the endpoint's list comes in, the user searches and ticks
// what they want, and "add selected" hands the ids back. Nothing is written
// here: the parent merges them into its form and only a save persists them.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type { CatalogModel } from '@/api/types'

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
  <a-modal
    :visible="props.visible"
    :title="t('instanceEdit.modelsFetchTitle')"
    :width="640"
    unmount-on-close
    @cancel="close"
  >
    <p class="picker-desc">{{ t('instanceEdit.modelsFetchDescription') }}</p>
    <a-input-search
      v-model="query"
      :placeholder="t('instanceEdit.modelsFetchSearch')"
      allow-clear
      class="picker-search"
    />
    <div class="picker-toolbar">
      <a-button type="text" size="mini" @click="selectAll">
        {{ t('instanceEdit.modelsFetchSelectAll') }}
      </a-button>
      <a-button type="text" size="mini" @click="deselectAll">
        {{ t('instanceEdit.modelsFetchDeselectAll') }}
      </a-button>
    </div>
    <a-scrollbar style="max-height: 320px; overflow-y: auto">
      <a-spin v-if="props.loading" class="picker-loading" />
      <a-checkbox-group v-else v-model="checked" direction="vertical" class="picker-list">
        <a-checkbox v-for="model in filtered" :key="model.id" :value="model.id">
          <span class="picker-id">{{ model.id }}</span>
          <span v-if="model.name && model.name !== model.id" class="picker-name">
            {{ model.name }}
          </span>
        </a-checkbox>
      </a-checkbox-group>
      <a-empty
        v-if="!props.loading && filtered.length === 0"
        :description="query ? t('instanceEdit.modelsFetchNoMatches') : (props.emptyText ?? t('instanceEdit.modelsFetchEmpty'))"
      />
    </a-scrollbar>
    <template #footer>
      <span class="picker-count">{{ t('instanceEdit.modelsFetchSelected', { count: checked.length }) }}</span>
      <a-button @click="close">{{ t('instanceEdit.modelsCancel') }}</a-button>
      <a-button type="primary" :disabled="checked.length === 0" @click="confirm">
        {{ t('instanceEdit.modelsFetchAdopt') }}
      </a-button>
    </template>
  </a-modal>
</template>

<style scoped lang="scss">
.picker-desc {
  margin: 0 0 12px;
  color: var(--color-text-3);
}

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
