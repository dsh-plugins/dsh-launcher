<script setup lang="ts">
// SKILL install picker (issue #10): opens from the instance SKILL tab,
// auto-loads every configured source repo, lists each repo's skills with
// their (possibly multi-line) descriptions, lets the user filter and pick
// which skills to install instead of installing everything.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { HintIcon, useFeedback } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { RepoSkillInfo } from '@/api/types'
import { useLauncherStore } from '@/stores/launcher'
import { shortRepoName } from '@/utils/repo'

const { message: Message } = useFeedback()

const props = defineProps<{
  visible: boolean
  homeId: string
}>()

const emit = defineEmits<{
  'update:visible': [boolean]
  installed: []
}>()

const { t } = useI18n()
const store = useLauncherStore()

interface RepoSkillRow extends RepoSkillInfo {
  /** Repo URL as configured (without any #path). */
  repo: string
  key: string
}

const rows = ref<RepoSkillRow[]>([])
const loading = ref(false)
const installing = ref(false)
const filter = ref('')
const sourceFilter = ref('')
const selectedKeys = ref<string[]>([])
const customUrl = ref('')
/** Repo URL → load error message. */
const errors = ref<Record<string, string>>({})

/** Distinct source repos currently loaded, as select options. */
const repoOptions = computed(() => {
  const seen = new Map<string, string>()
  for (const r of rows.value) {
    if (!seen.has(r.repo)) seen.set(r.repo, shortRepoName(r.repo))
  }
  return [...seen.entries()].map(([value, label]) => ({ value, label }))
})

const filteredRows = computed(() => {
  const q = filter.value.trim().toLowerCase()
  return rows.value.filter((r) => {
    if (sourceFilter.value && r.repo !== sourceFilter.value) return false
    if (!q) return true
    return (
      r.name.toLowerCase().includes(q) ||
      r.description.toLowerCase().includes(q) ||
      r.repo.toLowerCase().includes(q)
    )
  })
})

async function loadRepo(url: string) {
  const base = url.trim().split('#')[0]
  delete errors.value[base]
  try {
    const skills = await api.listRepoSkills(url.trim())
    rows.value = [
      ...rows.value.filter((r) => r.repo !== base),
      ...skills.map((s) => ({ ...s, repo: base, key: `${base}::${s.name}` })),
    ]
  } catch (e) {
    errors.value[base] = String(e)
  }
}

async function loadAll() {
  rows.value = []
  errors.value = {}
  selectedKeys.value = []
  loading.value = true
  try {
    await Promise.all(store.settings.skill_repos.map((r) => loadRepo(r)))
  } finally {
    loading.value = false
  }
}

async function loadCustom() {
  const url = customUrl.value.trim()
  if (!url) return
  loading.value = true
  try {
    await loadRepo(url)
    customUrl.value = ''
  } finally {
    loading.value = false
  }
}

watch(
  () => props.visible,
  (v) => {
    if (v) void loadAll()
  },
)

async function installSelected() {
  if (selectedKeys.value.length === 0) return
  installing.value = true
  const picked = rows.value.filter((r) => selectedKeys.value.includes(r.key))
  const installed: string[] = []
  const failed: string[] = []
  try {
    for (const item of picked) {
      const url = item.subpath ? `${item.repo}#/${item.subpath}` : item.repo
      try {
        installed.push(...(await api.installSkillRepo(props.homeId, url)))
      } catch (e) {
        failed.push(`${item.name}: ${e}`)
      }
    }
    if (installed.length > 0) {
      Message.success(t('instanceEdit.skillInstalled', { names: installed.join(', ') }))
    }
    for (const f of failed) Message.error(f)
    if (installed.length > 0) {
      emit('installed')
      emit('update:visible', false)
    }
  } finally {
    installing.value = false
  }
}

const columns = computed(() => [
  { title: t('instanceEdit.skillColName'), dataIndex: 'name', width: 200 },
  { title: t('instanceEdit.skillColDesc'), dataIndex: 'description', ellipsis: true, tooltip: { position: 'top' } },
  { title: t('instanceEdit.skillColRepo'), dataIndex: 'repo', slotName: 'repo', width: 220, ellipsis: true },
])
</script>

<template>
  <dsh-modal
    :visible="visible"
    :title="t('instanceEdit.skillRepoDialogTitle')"
    width="760px"
    :ok-loading="installing"
    :ok-button-props="{ disabled: selectedKeys.length === 0 }"
    :ok-text="t('instanceEdit.skillInstallSelected', { count: selectedKeys.length })"
    @ok="installSelected"
    @cancel="emit('update:visible', false)"
  >
    <div class="skill-dialog-tools">
      <dsh-select
        v-model="sourceFilter"
        :placeholder="t('instanceEdit.skillSourceAll')"
        allow-clear
        class="skill-source-select"
      >
        <dsh-option v-for="o in repoOptions" :key="o.value" :value="o.value" :label="o.label" />
      </dsh-select>
      <dsh-input
        v-model="filter"
        :placeholder="t('instanceEdit.skillFilterPlaceholder')"
        allow-clear
      />
      <dsh-input
        v-model="customUrl"
        :placeholder="t('instanceEdit.skillCustomRepoPlaceholder')"
        allow-clear
        @press-enter="loadCustom"
      >
        <template #append>
          <dsh-button variant="secondary" :loading="loading" :disabled="!customUrl.trim()" @click="loadCustom">
            {{ t('instanceEdit.skillLoadRepo') }}
          </dsh-button>
        </template>
      </dsh-input>
    </div>

    <dsh-alert
      v-for="(msg, repo) in errors"
      :key="repo"
      type="warning"
      class="skill-repo-error"
    >
      {{ shortRepoName(repo) }}: {{ msg }}
    </dsh-alert>

    <dsh-table
      v-model:selectedKeys="selectedKeys"
      :columns="columns"
      :data="filteredRows"
      :loading="loading"
      :pagination="false"
      :row-selection="{ type: 'checkbox', showCheckedAll: true }"
      row-key="key"
      size="small"
      :scroll="{ y: 380 }"
    >
      <template #repo="{ record }">
        <dsh-tooltip :content="record.repo">
          <span>{{ shortRepoName(record.repo) }}</span>
        </dsh-tooltip>
      </template>
      <template #empty>
        <dsh-empty>
          {{ t('instanceEdit.skillRepoEmpty') }}
          <HintIcon v-if="!store.settings.skill_repos.length" :content="t('instanceEdit.skillRepoEmptyHint')" />
        </dsh-empty>
      </template>
    </dsh-table>
  </dsh-modal>
</template>

<style lang="scss" scoped>
.skill-dialog-tools {
  display: flex;
  gap: 8px;
  margin-bottom: 10px;
}

.skill-source-select {
  flex: none;
  width: 200px;
}

.skill-repo-error {
  margin-bottom: 8px;
}
</style>
