<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch, type Ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useFeedback, useDialog } from '@dsh-plugin/dsh-ui-components-vue'
import { api } from '@/api'
import type { CompatibilityReport as CompatibilityReportType } from '@/api/types'
import CompatibilityReport from '@/components/CompatibilityReport.vue'
import { latestRequest } from '@/utils/latest-request'
import { renderMarkdown } from '@/utils/markdown'
import { useLauncherStore } from '@/stores/launcher'
import type {
  CatalogModel,
  CatalogProvider,
  CredentialInfo,
  DiscoverModelsInput,
  DshInstance,
  HomeLinkInfo,
  HomeLinkSuggestion,
  InstalledPlugin,
  McpKv,
  McpServer,
  McpTransport,
  PluginUpdateInfo,
  ProviderCatalog,
  ProviderModel,
  ProviderRoute,
  SkillInfo,
  SkillUpdateInfo,
} from '@/api/types'
import TerminalEmbed from './TerminalEmbed.vue'
import InstanceNetwork from '@/components/InstanceNetwork.vue'
import SkillRepoDialog from '@/components/SkillRepoDialog.vue'
import MigratePluginsDialog from '@/components/MigratePluginsDialog.vue'
import ModelPickerDialog from '@/components/ModelPickerDialog.vue'
import HintIcon from '@/components/HintIcon.vue'
import PageScrollbar from '@/components/PageScrollbar.vue'
import { shortRepoName } from '@/utils/repo'
import { pluginName } from '@/utils/market'

const { message: Message } = useFeedback()
const { modal: Modal } = useDialog()

const route = useRoute()
const router = useRouter()
const { t, te, locale } = useI18n()
const store = useLauncherStore()

const editingId = computed(() => (route.params.id as string | undefined) ?? null)
const networkInstance = computed(() => editingId.value ? store.instanceById(editingId.value) : undefined)
const isNew = computed(() => !editingId.value)

// --- WSL runtime (issue #19) ---------------------------------------------------

/** Distro of the currently selected HOME; non-empty means a WSL instance. */
const wslDistro = computed(() => store.homes.find((h) => h.id === homeId.value)?.wsl ?? null)
const isWsl = computed(() => !!wslDistro.value)
/** Versions compatible with the instance's runtime (Windows ↔ non-WSL records). */
const versionOptions = computed(() =>
  store.versions.filter((v) => (isWsl.value ? v.wsl === wslDistro.value : !v.wsl)),
)
const homeOptions = computed(() =>
  store.homes.filter((h) => (isWsl.value ? h.wsl === wslDistro.value : !h.wsl)),
)

// --- Sidebar tabs ---------------------------------------------------------------

type TabKey = 'basic' | 'network' | 'env' | 'profiles' | 'plugins' | 'skills' | 'agents' | 'mcp' | 'models' | 'storage' | 'terminal'
const activeTab = ref<TabKey>('basic')

// --- Form state ---------------------------------------------------------------

const name = ref('')
const versionId = ref<string | undefined>(undefined)
const DEDICATED = '__dedicated__'
const homeId = ref<string | undefined>(undefined)
const dedicatedPath = ref('')
/** Custom name for a newly created dedicated DSH_HOME (defaults to the
 * instance name when left blank). */
const homeName = ref('')
const defaultProfile = ref<string | undefined>(undefined)
const profiles = ref<string[]>([])
const newProfileName = ref('')
const creatingProfile = ref(false)
const addingProfile = ref(false)
const saving = ref(false)

// --- TUI working directory (issue #97) ----------------------------------------

const tuiCwdInput = ref('')
const tuiCwdBusy = ref(false)
/** Profile the instance actually launches (mirrors the backend's
 * `tui_active_profile`): the last launched one, else the default. */
const activeProfile = ref<string | null>(null)

/**
 * Kinds of the profiles in the currently selected HOME (issue #31), so the TUI
 * working-directory control hides for a HOME that has no TUI profile at all.
 * Empty until the probe answers — and empty also on a failed probe, so the
 * control stays visible whenever we do not positively know better.
 */
const profileKinds = ref<Record<string, string> | null>(null)

/**
 * Whether this instance can actually launch the TUI, i.e. whether `tui_cwd`
 * has any effect (issue #97 review). A HOME that merely *contains* some TUI
 * profile is not enough: an instance pinned to a web profile would show the
 * control, accept a value, and never use it.
 * Unknown (probe pending/failed) keeps it visible — never hide an option
 * because a probe did not answer.
 */
const hasTuiProfile = computed(() => {
  const kinds = profileKinds.value
  if (kinds === null) return true
  // Any profile this instance would actually launch (last launched, else the
  // default being edited) decides it; an unknown name counts as "no".
  const candidates = [activeProfile.value, defaultProfile.value].filter(
    (p): p is string => !!p,
  )
  if (candidates.length) return candidates.some((p) => kinds[p] === 'tui')
  // No profile chosen yet: offer the control when the HOME has one at all.
  return Object.values(kinds).includes('tui')
})

/** Keeps `activeProfile` in sync with the instance the form is editing. */
function syncActiveProfile(inst: DshInstance | null | undefined) {
  activeProfile.value = inst ? (inst.last_profile ?? inst.default_profile ?? null) : null
}

async function loadProfileKinds(homeIdValue: string | null) {
  profileKinds.value = null
  if (!homeIdValue || homeIdValue === DEDICATED) return
  try {
    const infos = await api.listProfileInfos(homeIdValue)
    const out: Record<string, string> = {}
    for (const p of infos) out[p.name] = p.kind
    profileKinds.value = out
  } catch {
    // A failed probe must not hide an optional control — keep it visible.
    profileKinds.value = null
  }
}

async function browseTuiCwd() {
  if (!editingId.value) return
  // Same pattern as the other pickers in this view (and Settings.vue): the
  // Tauri dialog plugin. There is no native dialog in browser preview.
  if (!api.isTauri) {
    Message.info(t('settings.browserPickHint'))
    return
  }
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const selected = await open({
      directory: true,
      multiple: false,
      title: t('instanceEdit.tuiCwdBrowseTitle'),
      defaultPath: tuiCwdInput.value.trim() || undefined,
    })
    if (typeof selected !== 'string') return
    tuiCwdInput.value = selected
    await applyTuiCwd()
  } catch (e) {
    Message.error(String(e))
  }
}

async function applyTuiCwd() {
  if (!editingId.value) return
  tuiCwdBusy.value = true
  try {
    const path = tuiCwdInput.value.trim() || null
    const updated = await api.setInstanceTuiCwd(editingId.value, path)
    const inst = store.instanceById(editingId.value)
    if (inst) inst.tui_cwd = updated.tui_cwd ?? null
    tuiCwdInput.value = updated.tui_cwd ?? ''
    Message.success(
      updated.tui_cwd
        ? t('instanceEdit.tuiCwdSaved', { path: updated.tui_cwd })
        : t('instanceEdit.tuiCwdSavedDefault'),
    )
  } catch (e) {
    Message.error(String(e))
  } finally {
    tuiCwdBusy.value = false
  }
}

interface EnvRow {
  key: string
  value: string
}
const envRows = ref<EnvRow[]>([])

const ENV_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]*$/
const RESERVED_KEYS = new Set(['DSH_HOME'])

function envKeyError(row: EnvRow): string | null {
  if (!row.key) return null
  if (RESERVED_KEYS.has(row.key)) return t('instanceEdit.envKeyReserved')
  if (!ENV_KEY_RE.test(row.key)) return t('instanceEdit.envKeyInvalid')
  return null
}

const envValid = computed(() => envRows.value.every((r) => !envKeyError(r)))

onMounted(async () => {
  if (!editingId.value) return
  const inst = store.instanceById(editingId.value) ?? (await api.listInstances()).find((i) => i.id === editingId.value)
  if (!inst) {
    Message.error(t('instanceEdit.notFound'))
    router.replace({ name: 'home' })
    return
  }
  name.value = inst.name
  versionId.value = inst.version_id
  homeId.value = inst.home_id
  defaultProfile.value = inst.default_profile ?? undefined
  tuiCwdInput.value = inst.tui_cwd ?? ''
  syncActiveProfile(inst)
  envRows.value = Object.entries(inst.env_overrides).map(([key, value]) => ({ key, value }))
  await loadIcon()
  void loadProfileKinds(inst.home_id)
})

// --- Instance icon (issue #8) --------------------------------------------------

const iconUrl = ref<string | null>(null)
const iconInput = ref('')
const iconBusy = ref(false)

async function loadIcon() {
  if (!editingId.value) return
  try {
    iconUrl.value = await api.readInstanceIcon(editingId.value)
  } catch {
    iconUrl.value = null
  }
}

async function applyIconInput() {
  if (!editingId.value || !iconInput.value.trim()) return
  iconBusy.value = true
  try {
    await api.setInstanceIcon(editingId.value, iconInput.value.trim())
    iconInput.value = ''
    await loadIcon()
    await store.refreshInstances()
    Message.success(t('instanceEdit.iconUpdated'))
  } catch (e) {
    Message.error(String(e))
  } finally {
    iconBusy.value = false
  }
}

async function pickIconFile() {
  if (!editingId.value) return
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'Image', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'] }],
  })
  if (typeof file !== 'string') return
  iconBusy.value = true
  try {
    await api.setInstanceIcon(editingId.value, file)
    await loadIcon()
    await store.refreshInstances()
    Message.success(t('instanceEdit.iconUpdated'))
  } catch (e) {
    Message.error(String(e))
  } finally {
    iconBusy.value = false
  }
}

async function clearIcon() {
  if (!editingId.value) return
  try {
    await api.clearInstanceIcon(editingId.value)
    await loadIcon()
    await store.refreshInstances()
  } catch (e) {
    Message.error(String(e))
  }
}

watch(homeId, async (v) => {
  profiles.value = []
  // A different HOME means a different patch layer: reset the MCP scope.
  mcpScope.value = MCP_GLOBAL
  mcpServers.value = []
  if (v === DEDICATED) {
    dedicatedPath.value = await api.defaultDedicatedHomePath(name.value.trim() || 'instance')
    return
  }
  if (!v) return
  // Issue #97: the TUI working-directory control is offered only when the
  // selected HOME has a TUI profile to launch.
  void loadProfileKinds(v)
  try {
    profiles.value = await api.listProfiles(v)
    if (defaultProfile.value && !profiles.value.includes(defaultProfile.value)) {
      defaultProfile.value = undefined
    }
  } catch (e) {
    Message.error(String(e))
  }
})

watch(name, async (v) => {
  if (homeId.value === DEDICATED) {
    dedicatedPath.value = await api.defaultDedicatedHomePath(v.trim() || 'instance')
  }
})

// --- Open directory / view log (issue: instance folder & log access) -------

const dirBusy = ref(false)
const logBusy = ref(false)

async function onOpenDirectory() {
  if (!editingId.value) return
  dirBusy.value = true
  try {
    const path = await api.openInstanceDirectory(editingId.value)
    Message.success(t('instanceEdit.dirOpened', { path }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    dirBusy.value = false
  }
}

async function onViewLog() {
  if (!editingId.value) return
  logBusy.value = true
  try {
    const path = await api.openInstanceLog(editingId.value)
    Message.success(t('instanceEdit.logOpened', { path }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    logBusy.value = false
  }
}

/** Env overrides apply at process launch only (issue #52): when the user
 * edits them on a running instance, offer an immediate restart so the change
 * visibly takes effect instead of looking broken. */
function maybePromptEnvRestart(previous: DshInstance | null, envChanged: boolean) {
  if (!previous || !envChanged) return
  if (store.statusOf(previous.id).state !== 'running') return
  const profile = previous.last_profile ?? previous.default_profile
  Modal.confirm({
    title: t('instanceEdit.envRestartTitle'),
    content: profile
      ? t('instanceEdit.envRestartHint', { profile })
      : t('instanceEdit.envRestartNoProfile'),
    okText: profile ? t('instanceEdit.envRestartNow') : t('common.confirm'),
    cancelText: t('instanceEdit.envRestartLater'),
    async onOk() {
      if (!profile) return
      try {
        await api.stopInstance(previous.id)
        await api.startInstance(previous.id, profile)
      } catch (e) {
        Message.error(String(e))
      }
    },
  })
}

// --- Save ----------------------------------------------------------------------

const formValid = computed(
  () => name.value.trim().length > 0 && !!versionId.value && !!homeId.value && envValid.value,
)

async function onSave() {
  if (!formValid.value) return
  const envOverrides: Record<string, string> = {}
  for (const row of envRows.value) {
    if (row.key) envOverrides[row.key] = row.value
  }
  saving.value = true
  try {
    // A dedicated DSH_HOME is created on demand for this instance.
    let resolvedHomeId = homeId.value!
    if (homeId.value === DEDICATED) {
      const home = await api.createHome(homeName.value.trim() || name.value.trim(), dedicatedPath.value)
      resolvedHomeId = home.id
      await store.refreshHomes()
    }
    // Issue #52: env changes only reach the process at launch. Detect an
    // env edit on a running instance so we can offer an immediate restart
    // instead of leaving it on stale variables.
    const previous = isNew.value ? null : (store.instanceById(editingId.value!) as DshInstance)
    const envFingerprint = (m: Record<string, string>) => JSON.stringify(Object.entries(m).sort())
    const envChanged = !!previous && envFingerprint(previous.env_overrides) !== envFingerprint(envOverrides)
    if (isNew.value) {
      await api.createInstance({
        name: name.value.trim(),
        version_id: versionId.value!,
        home_id: resolvedHomeId,
        env_overrides: envOverrides,
        default_profile: defaultProfile.value ?? null,
      })
    } else {
      const inst = store.instanceById(editingId.value!) as DshInstance
      await api.updateInstance({
        ...inst,
        name: name.value.trim(),
        version_id: versionId.value!,
        home_id: resolvedHomeId,
        env_overrides: envOverrides,
        default_profile: defaultProfile.value ?? null,
      })
    }
    await store.refreshInstances()
    Message.success(t('instanceEdit.saved'))
    maybePromptEnvRestart(previous, envChanged)
    router.push({ name: 'home' })
  } catch (e) {
    Message.error(String(e))
  } finally {
    saving.value = false
  }
}

function addEnvRow() {
  envRows.value.push({ key: '', value: '' })
}

function removeEnvRow(idx: number) {
  envRows.value.splice(idx, 1)
}

async function onCreateProfile() {
  const name = newProfileName.value.trim()
  if (!homeId.value || !name) return
  creatingProfile.value = true
  try {
    await api.createProfile(homeId.value, name)
    profiles.value = await api.listProfiles(homeId.value)
    newProfileName.value = ''
    addingProfile.value = false
    Message.success(t('instanceEdit.profileCreated', { name }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    creatingProfile.value = false
  }
}

function cancelAddProfile() {
  addingProfile.value = false
  newProfileName.value = ''
}

function setDefaultProfile(name: string) {
  defaultProfile.value = name
  Message.success(t('instanceEdit.profileSetDefault', { name }))
}

// --- Profile rename/delete ------------------------------------------------------

const renamingProfile = ref<string | null>(null)
const renameValue = ref('')
const busyProfile = ref<string | null>(null)

// --- Profile copy ---------------------------------------------------------------

const copyingProfile = ref<string | null>(null)
const copyProfileName = ref('')
const copyProfileBusy = ref(false)

function startCopyProfile(name: string) {
  copyingProfile.value = name
  copyProfileName.value = `${name}-copy`
}

function cancelCopyProfile() {
  copyingProfile.value = null
  copyProfileName.value = ''
}

async function confirmCopyProfile() {
  if (!homeId.value || !copyingProfile.value) return
  const source = copyingProfile.value
  const newName = copyProfileName.value.trim()
  if (!newName) return
  copyProfileBusy.value = true
  try {
    await api.copyProfile(homeId.value, source, newName)
    profiles.value = await api.listProfiles(homeId.value)
    cancelCopyProfile()
    Message.success(t('instanceEdit.profileCopied', { source, name: newName }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    copyProfileBusy.value = false
  }
}

function startRenameProfile(name: string) {
  renamingProfile.value = name
  renameValue.value = name
}

function cancelRenameProfile() {
  renamingProfile.value = null
  renameValue.value = ''
}

async function confirmRenameProfile() {
  if (!homeId.value || !renamingProfile.value) return
  const oldName = renamingProfile.value
  const newName = renameValue.value.trim()
  if (!newName || newName === oldName) {
    cancelRenameProfile()
    return
  }
  busyProfile.value = oldName
  try {
    await api.renameProfile(homeId.value, oldName, newName)
    profiles.value = await api.listProfiles(homeId.value)
    cancelRenameProfile()
    Message.success(t('instanceEdit.profileRenamed', { old: oldName, name: newName }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    busyProfile.value = null
  }
}

async function confirmDeleteProfile(name: string) {
  if (!homeId.value) return
  busyProfile.value = name
  try {
    await api.deleteProfile(homeId.value, name)
    profiles.value = await api.listProfiles(homeId.value)
    if (defaultProfile.value === name) defaultProfile.value = undefined
    Message.success(t('instanceEdit.profileDeleted', { name }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    busyProfile.value = null
  }
}

// --- Modpack (整合包) export/import ----------------------------------------------

function startExportModpack(profile: string) {
  if (!homeId.value || !editingId.value) return
  store.modpackExport = {
    instanceId: editingId.value,
    homeId: homeId.value,
    profile,
    displayName: name.value.trim(),
  }
  router.push({ name: 'modpack-export' })
}

/** Multi-profile export (manifest v5 dshhome): pick profiles on the wizard page. */
function startExportModpackMulti() {
  if (!homeId.value || !editingId.value) return
  store.modpackExportMulti = {
    instanceId: editingId.value,
    homeId: homeId.value,
    displayName: name.value.trim(),
    defaultProfile: defaultProfile.value,
  }
  router.push({ name: 'modpack-export-multi' })
}

// --- Local plugin (.tgz) import --------------------------------------------------

async function importLocalPlugin() {
  if (!editingId.value || !pluginProfile.value) return
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'DSH Plugin', extensions: ['tgz'] }],
  })
  if (typeof file !== 'string') return
  try {
    await api.startInstallPluginFileTask(editingId.value, pluginProfile.value, file)
    await store.refreshTasks()
    Message.success(t('download.taskAdded'))
    router.push({ name: 'tasks' })
  } catch (e) {
    Message.error(String(e))
  }
}

// --- SKILL tab (issue #10) ------------------------------------------------------

const skills = ref<SkillInfo[]>([])
const skillsLoading = ref(false)
const skillActionBusy = ref('')
const skillRepoDialogVisible = ref(false)
const skillCreateVisible = ref(false)
const skillCreateForm = ref({ name: '', description: '', content: '' })
const skillCreateBusy = ref(false)
const skillUpdates = ref<SkillUpdateInfo[]>([])
const skillCheckingUpdates = ref(false)
const skillUpdatingAll = ref(false)
const skillOpeningDir = ref(false)
const skillSelectedKeys = ref<string[]>([])
const skillExporting = ref(false)

function onSkillSelectionChange(keys: (string | number)[]) {
  skillSelectedKeys.value = keys.map(String)
}

/** Exports the checked skills into one ZIP chosen via the save dialog (issue #61). */
async function onExportSkills() {
  if (!homeId.value || skillSelectedKeys.value.length === 0) return
  const entries = skills.value
    .filter((s) => skillSelectedKeys.value.includes(s.name))
    .map((s) => s.entry)
  if (entries.length === 0) return
  const { save } = await import('@tauri-apps/plugin-dialog')
  const dest = await save({
    defaultPath: 'skills.zip',
    filters: [{ name: 'ZIP', extensions: ['zip'] }],
  })
  if (typeof dest !== 'string') return
  skillExporting.value = true
  try {
    await api.exportSkills(homeId.value, entries, dest)
    Message.success(t('instanceEdit.skillExported', { path: dest }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillExporting.value = false
  }
}

async function onOpenSkillsDir() {
  if (!homeId.value) return
  skillOpeningDir.value = true
  try {
    await api.openSkillsDirectory(homeId.value)
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillOpeningDir.value = false
  }
}

// --- AGENTS.md tab (issue #57) ------------------------------------------------

const agentsContent = ref('')
const agentsLoading = ref(false)
const agentsSaving = ref(false)
const agentsPreview = ref(true)

const agentsPreviewHtml = computed(() => renderMarkdown(agentsContent.value))

async function loadAgentsMd() {
  if (!homeId.value || !editingId.value) return
  agentsLoading.value = true
  try {
    agentsContent.value = await api.readAgentsMd(homeId.value)
  } catch (e) {
    Message.error(String(e))
  } finally {
    agentsLoading.value = false
  }
}

// Issue #94: the toolbar button switches back to the rendered preview after a
// successful save; Ctrl+S saves in place so an editing flow is not disrupted.
async function saveAgentsMd(backToPreview: boolean) {
  if (!homeId.value || !editingId.value) return
  agentsSaving.value = true
  try {
    await api.writeAgentsMd(homeId.value, agentsContent.value)
    Message.success(t('instanceEdit.agentsSaved'))
    if (backToPreview) agentsPreview.value = true
  } catch (e) {
    Message.error(String(e))
  } finally {
    agentsSaving.value = false
  }
}

function onAgentsEditorKeydown(event: KeyboardEvent) {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
    event.preventDefault()
    void saveAgentsMd(false)
  }
}

const skillColumns = computed(() => [
  { title: t('instanceEdit.skillColName'), slotName: 'skillName' },
  { title: t('instanceEdit.skillColDesc'), slotName: 'skillDesc' },
  { title: t('instanceEdit.skillColOrigin'), slotName: 'origin', headerCellClass: 'cell-nowrap' },
  { title: t('instances.table.actions'), slotName: 'skillActions', width: 170, fixed: 'right' as const, align: 'center' as const },
])

async function loadSkills() {
  if (!homeId.value) return
  skillsLoading.value = true
  skillSelectedKeys.value = []
  try {
    skills.value = await api.listInstanceSkills(homeId.value)
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillsLoading.value = false
  }
}

function skillUpdateOf(name: string): SkillUpdateInfo | undefined {
  return skillUpdates.value.find((u) => u.name === name)
}

async function onCheckSkillUpdates() {
  if (!homeId.value) return
  skillCheckingUpdates.value = true
  try {
    skillUpdates.value = await api.checkSkillUpdates(homeId.value)
    Message.success(
      skillUpdates.value.length > 0
        ? t('instanceEdit.skillUpdatesFound', { count: skillUpdates.value.length })
        : t('instanceEdit.skillNoUpdates'),
    )
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillCheckingUpdates.value = false
  }
}

async function onUpdateSkill(name: string) {
  if (!homeId.value) return
  skillActionBusy.value = name
  try {
    const version = await api.updateSkill(homeId.value, name)
    Message.success(t('instanceEdit.skillUpdated', { name, version }))
    skillUpdates.value = skillUpdates.value.filter((u) => u.name !== name)
    await loadSkills()
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillActionBusy.value = ''
  }
}

async function onUpdateAllSkills() {
  if (skillUpdates.value.length === 0) return
  skillUpdatingAll.value = true
  try {
    for (const u of [...skillUpdates.value]) {
      await onUpdateSkill(u.name)
    }
  } finally {
    skillUpdatingAll.value = false
  }
}

async function onDeleteSkill(name: string) {
  if (!homeId.value) return
  skillActionBusy.value = name
  try {
    await api.deleteSkill(homeId.value, name)
    Message.success(t('instanceEdit.skillDeleted', { name }))
    await loadSkills()
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillActionBusy.value = ''
  }
}

async function onImportSkillFile() {
  if (!homeId.value) return
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'SKILL.md', extensions: ['md'] }],
  })
  if (typeof file !== 'string') return
  try {
    const name = await api.importSkillFile(homeId.value, file)
    Message.success(t('instanceEdit.skillInstalled', { names: name }))
    await loadSkills()
  } catch (e) {
    Message.error(String(e))
  }
}

async function onImportSkillZip() {
  if (!homeId.value) return
  const { open } = await import('@tauri-apps/plugin-dialog')
  const file = await open({
    multiple: false,
    filters: [{ name: 'ZIP', extensions: ['zip'] }],
  })
  if (typeof file !== 'string') return
  try {
    const names = await api.importSkillZip(homeId.value, file)
    Message.success(t('instanceEdit.skillInstalled', { names: names.join(', ') }))
    await loadSkills()
  } catch (e) {
    Message.error(String(e))
  }
}

async function onCreateSkill() {
  if (!homeId.value || !skillCreateForm.value.name.trim()) return
  skillCreateBusy.value = true
  try {
    const name = await api.createSkill(
      homeId.value,
      skillCreateForm.value.name.trim(),
      skillCreateForm.value.description.trim(),
      skillCreateForm.value.content,
    )
    skillCreateVisible.value = false
    skillCreateForm.value = { name: '', description: '', content: '' }
    Message.success(t('instanceEdit.skillInstalled', { names: name }))
    await loadSkills()
  } catch (e) {
    Message.error(String(e))
  } finally {
    skillCreateBusy.value = false
  }
}

// --- MCP tab (`dsh-mcp-client` rows in a cordis.patch.yml patch layer) ----------

/** Scope selector value for the DSH_HOME itself. */
const MCP_GLOBAL = '__global__'

const mcpScope = ref<string>(MCP_GLOBAL)
const mcpServers = ref<McpServer[]>([])
const mcpLoading = ref(false)
const mcpBusy = ref('')
const mcpEditVisible = ref(false)
const mcpSaving = ref(false)
/** Loader row id being edited; '' while adding a new server. */
const mcpOriginalId = ref('')

/** Editable projection of one MCP server (key/value rows like the env editor). */
interface McpFormState {
  serverName: string
  transport: McpTransport
  url: string
  headers: EnvRow[]
  command: string
  args: string[]
  env: EnvRow[]
  cwd: string
  enabled: boolean
  /** Config keys the form does not surface; sent back untouched. */
  extra: Record<string, unknown>
}

function emptyMcpForm(): McpFormState {
  return {
    serverName: '',
    transport: 'stdio',
    url: '',
    headers: [],
    command: '',
    args: [],
    env: [],
    cwd: '',
    enabled: true,
    extra: {},
  }
}

const mcpForm = ref<McpFormState>(emptyMcpForm())

/** null = global scope (the HOME itself), otherwise the selected profile. */
const mcpScopeProfile = computed(() => (mcpScope.value === MCP_GLOBAL ? null : mcpScope.value))

/** The patch file a save writes to, shown under the scope selector. */
const mcpScopePath = computed(() => {
  const home = store.homes.find((h) => h.id === homeId.value)
  if (!home) return ''
  const sep = home.path.includes('\\') ? '\\' : '/'
  const parts = mcpScopeProfile.value
    ? [home.path, 'profiles', mcpScopeProfile.value, 'cordis.patch.yml']
    : [home.path, 'cordis.patch.yml']
  return parts.join(sep)
})

const mcpColumns = computed(() => [
  { title: t('instanceEdit.mcpColName'), slotName: 'mcpName' },
  { title: t('instanceEdit.mcpColTransport'), slotName: 'mcpTransport', width: 150 },
  { title: t('instanceEdit.mcpColTarget'), slotName: 'mcpTarget' },
  { title: t('instanceEdit.mcpColStatus'), slotName: 'mcpStatus', width: 110, fixed: 'right' as const },
  { title: t('instances.table.actions'), slotName: 'mcpActions', width: 150, fixed: 'right' as const, align: 'center' as const },
])

const contentSizedTable = { x: 'max-content', minWidth: '100%' }

/** One-line endpoint summary shown in the target column (stdio → command). */
function mcpTargetText(server: McpServer): string {
  return server.transport === 'stdio' ? server.command : server.url
}

async function loadMcpServers() {
  mcpServers.value = []
  if (!homeId.value || homeId.value === DEDICATED) return
  mcpLoading.value = true
  try {
    mcpServers.value = await api.listMcpServers(homeId.value, mcpScopeProfile.value)
  } catch (e) {
    Message.error(String(e))
  } finally {
    mcpLoading.value = false
  }
}

watch(mcpScope, async () => {
  if (activeTab.value === 'mcp') await loadMcpServers()
})

// --- MCP validation (mirrors src-tauri/src/mcp.rs; a failure never saves) -------

/** dsh-mcp-client's `serverName` budget: it derives `mcp__<serverName>__*`. */
const MCP_NAME_RE = /^[A-Za-z0-9_-]{1,32}$/
/** RFC 7230 header field-name token. */
const HEADER_KEY_RE = /^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value)
    return (url.protocol === 'http:' || url.protocol === 'https:') && !!url.hostname
  } catch {
    return false
  }
}

/** Per-row key error: pattern first, then a duplicate of an earlier row. */
function kvKeyError(rows: EnvRow[], idx: number, re: RegExp, invalid: string, duplicated: string): string {
  const key = rows[idx].key.trim()
  // Blank rows are dropped on save, so they are not an error yet.
  if (!key) return ''
  if (!re.test(key)) return invalid
  return rows.findIndex((r) => r.key.trim() === key) < idx ? duplicated : ''
}

function mcpHeaderKeyError(idx: number): string {
  return kvKeyError(
    mcpForm.value.headers,
    idx,
    HEADER_KEY_RE,
    t('instanceEdit.mcpErrHeaderKey'),
    t('instanceEdit.mcpErrHeaderDuplicated'),
  )
}

function mcpEnvKeyError(idx: number): string {
  return kvKeyError(
    mcpForm.value.env,
    idx,
    ENV_KEY_RE,
    t('instanceEdit.mcpErrEnvKey'),
    t('instanceEdit.mcpErrEnvDuplicated'),
  )
}

const mcpNameError = computed(() => {
  const name = mcpForm.value.serverName.trim()
  if (!name) return t('instanceEdit.mcpErrNameRequired')
  if (!MCP_NAME_RE.test(name)) return t('instanceEdit.mcpErrNamePattern')
  const clash = mcpServers.value.some(
    (s) => s.id !== mcpOriginalId.value && s.serverName === name,
  )
  return clash ? t('instanceEdit.mcpErrNameDuplicated') : ''
})

const mcpUrlError = computed(() => {
  if (mcpForm.value.transport !== 'streamable-http') return ''
  const url = mcpForm.value.url.trim()
  if (!url) return t('instanceEdit.mcpErrUrlRequired')
  return isHttpUrl(url) ? '' : t('instanceEdit.mcpErrUrlInvalid')
})

const mcpCommandError = computed(() => {
  if (mcpForm.value.transport !== 'stdio') return ''
  return mcpForm.value.command.trim() ? '' : t('instanceEdit.mcpErrCommandRequired')
})

const mcpFormValid = computed(
  () =>
    !mcpNameError.value &&
    !mcpUrlError.value &&
    !mcpCommandError.value &&
    mcpForm.value.headers.every((_, idx) => !mcpHeaderKeyError(idx)) &&
    mcpForm.value.env.every((_, idx) => !mcpEnvKeyError(idx)),
)

/** Names listed in the dialog's preserved-config notice. */
const mcpExtraKeys = computed(() => Object.keys(mcpForm.value.extra ?? {}))

function openMcpCreate() {
  mcpOriginalId.value = ''
  mcpForm.value = emptyMcpForm()
  mcpEditVisible.value = true
}

function openMcpEdit(server: McpServer) {
  mcpOriginalId.value = server.id
  mcpForm.value = {
    serverName: server.serverName,
    transport: server.transport,
    url: server.url,
    headers: server.headers.map((kv) => ({ key: kv.key, value: kv.value })),
    command: server.command,
    args: [...server.args],
    env: server.env.map((kv) => ({ key: kv.key, value: kv.value })),
    cwd: server.cwd,
    enabled: server.enabled,
    extra: { ...(server.extra ?? {}) },
  }
  mcpEditVisible.value = true
}

function addMcpHeaderRow() {
  mcpForm.value.headers.push({ key: '', value: '' })
}

function addMcpEnvRow() {
  mcpForm.value.env.push({ key: '', value: '' })
}

function addMcpArgRow() {
  mcpForm.value.args.push('')
}

/** Drops blank rows and trims keys, like the env-override editor. */
function kvPayload(rows: EnvRow[]): McpKv[] {
  return rows.filter((r) => r.key.trim()).map((r) => ({ key: r.key.trim(), value: r.value }))
}

/** Transport decides which fields are written; the other side is cleared. */
function mcpPayload(form: McpFormState, id: string): McpServer {
  const http = form.transport === 'streamable-http'
  return {
    id,
    serverName: form.serverName.trim(),
    transport: form.transport,
    url: http ? form.url.trim() : '',
    headers: http ? kvPayload(form.headers) : [],
    command: http ? '' : form.command.trim(),
    args: http ? [] : form.args.map((a) => a.trim()).filter(Boolean),
    env: http ? [] : kvPayload(form.env),
    cwd: http ? '' : form.cwd.trim(),
    enabled: form.enabled,
    extra: form.extra,
  }
}

async function onSaveMcpServer() {
  if (!homeId.value) return
  if (!mcpFormValid.value) {
    // Errors are rendered per field and nothing is sent, so nothing is written.
    Message.warning(t('instanceEdit.mcpErrForm'))
    return
  }
  const server = mcpPayload(mcpForm.value, mcpOriginalId.value)
  mcpSaving.value = true
  try {
    mcpServers.value = await api.saveMcpServer(
      homeId.value,
      mcpScopeProfile.value,
      server,
      mcpOriginalId.value || null,
    )
    mcpEditVisible.value = false
    Message.success(t('instanceEdit.mcpSaved', { name: server.serverName }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    mcpSaving.value = false
  }
}

// --- mcpServers JSON paste import (issue #93) --------------------------------
// The page accepts a pasted `{"mcpServers": {"name": {…}}}` document and adds
// every entry; the create/edit dialog fills its form from the first entry.
// Anything that does not parse into that shape is ignored so ordinary paste
// into inputs keeps working.

interface McpPastedServer {
  name: string
  form: McpFormState
}

/** Object-valued key/value config (`env`, `headers`) → editable rows. */
function objToKvRows(value: unknown): EnvRow[] {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return []
  return Object.entries(value as Record<string, unknown>).map(([key, v]) => ({
    key,
    value: typeof v === 'string' ? v : String(v),
  }))
}

/** null when the text is not an mcpServers document with at least one entry. */
function parseMcpServersJson(text: string): McpPastedServer[] | null {
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch {
    return null
  }
  if (!raw || typeof raw !== 'object') return null
  const servers = (raw as Record<string, unknown>).mcpServers
  if (!servers || typeof servers !== 'object' || Array.isArray(servers)) return null
  const out: McpPastedServer[] = []
  for (const [name, cfgRaw] of Object.entries(servers as Record<string, unknown>)) {
    if (!cfgRaw || typeof cfgRaw !== 'object') continue
    const cfg = cfgRaw as Record<string, unknown>
    const form = emptyMcpForm()
    form.serverName = name
    if (typeof cfg.url === 'string' && cfg.url.trim()) {
      form.transport = 'streamable-http'
      form.url = cfg.url
      form.headers = objToKvRows(cfg.headers)
    } else {
      form.transport = 'stdio'
      form.command = typeof cfg.command === 'string' ? cfg.command : ''
      form.args = Array.isArray(cfg.args) ? cfg.args.filter((a): a is string => typeof a === 'string') : []
      form.env = objToKvRows(cfg.env)
      form.cwd = typeof cfg.cwd === 'string' ? cfg.cwd : ''
    }
    out.push({ name, form })
  }
  return out.length > 0 ? out : null
}

/** The same field rules the dialog enforces; returns the first problem found. */
function pastedMcpProblem(form: McpFormState, taken: Set<string>): string {
  const name = form.serverName.trim()
  if (!MCP_NAME_RE.test(name)) return t('instanceEdit.mcpErrNamePattern')
  if (mcpServers.value.some((s) => s.serverName === name) || taken.has(name)) {
    return t('instanceEdit.mcpErrNameDuplicated')
  }
  if (form.transport === 'streamable-http') {
    if (!form.url.trim()) return t('instanceEdit.mcpErrUrlRequired')
    if (!isHttpUrl(form.url.trim())) return t('instanceEdit.mcpErrUrlInvalid')
  } else if (!form.command.trim()) {
    return t('instanceEdit.mcpErrCommandRequired')
  }
  return ''
}

const mcpImporting = ref(false)

async function importMcpServers(entries: McpPastedServer[]) {
  if (!homeId.value || mcpImporting.value) return
  mcpImporting.value = true
  const taken = new Set<string>()
  const failures: string[] = []
  let ok = 0
  try {
    for (const entry of entries) {
      const problem = pastedMcpProblem(entry.form, taken)
      if (problem) {
        failures.push(`${entry.name}: ${problem}`)
        continue
      }
      try {
        mcpServers.value = await api.saveMcpServer(
          homeId.value,
          mcpScopeProfile.value,
          mcpPayload(entry.form, ''),
          null,
        )
        taken.add(entry.form.serverName.trim())
        ok++
      } catch (e) {
        failures.push(`${entry.name}: ${String(e)}`)
      }
    }
  } finally {
    mcpImporting.value = false
  }
  if (failures.length === 0) {
    Message.success(t('instanceEdit.mcpPasteImported', { count: ok }))
  } else {
    const summary = t('instanceEdit.mcpPastePartial', {
      ok,
      total: entries.length,
      errors: failures.join('；'),
    })
    if (ok > 0) Message.warning(summary)
    else Message.error(summary)
  }
}

function onMcpPagePaste(event: ClipboardEvent) {
  // The dialog fills its own form; do not double-handle its paste events.
  if (mcpEditVisible.value) return
  const target = event.target as HTMLElement | null
  if (target?.closest('input, textarea, [contenteditable="true"]')) return
  const entries = parseMcpServersJson(event.clipboardData?.getData('text/plain') ?? '')
  if (!entries) return
  event.preventDefault()
  void importMcpServers(entries)
}

function onMcpDialogPaste(event: ClipboardEvent) {
  const entries = parseMcpServersJson(event.clipboardData?.getData('text/plain') ?? '')
  if (!entries) return
  event.preventDefault()
  // `extra` holds keys of the server being edited that the form cannot show;
  // the pasted document knows nothing about them, so keep them attached.
  mcpForm.value = { ...entries[0].form, extra: mcpForm.value.extra }
  Message.success(t('instanceEdit.mcpPasteFilled', { name: entries[0].name }))
}

/** Enable / disable in place: the row keeps its config, only `disabled` moves. */
async function onToggleMcpServer(server: McpServer, enabled: boolean) {
  if (!homeId.value) return
  mcpBusy.value = server.id
  try {
    mcpServers.value = await api.saveMcpServer(
      homeId.value,
      mcpScopeProfile.value,
      { ...server, enabled },
      server.id,
    )
  } catch (e) {
    Message.error(String(e))
    await loadMcpServers()
  } finally {
    mcpBusy.value = ''
  }
}

async function onDeleteMcpServer(server: McpServer) {
  if (!homeId.value) return
  mcpBusy.value = server.id
  try {
    mcpServers.value = await api.deleteMcpServer(homeId.value, mcpScopeProfile.value, server.id)
    Message.success(t('instanceEdit.mcpDeleted', { name: server.serverName }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    mcpBusy.value = ''
  }
}

// --- Models tab (issue #89): configure model providers exactly like DSH's
// "Settings → Models" page. The launcher writes the same `cordis.patch.yml`
// `llm-pi-ai.config.providers` dict DSH does, plus the key in `.credentials.yaml`.

const MODELS_GLOBAL = '__global__'
const modelsScope = ref<string>(MODELS_GLOBAL)
const providerRoutes = ref<ProviderRoute[]>([])
const catalog = ref<ProviderCatalog | null>(null)
const catalogLoading = ref(false)
const modelsLoading = ref(false)
const providerSaving = ref(false)
const modelsBusy = ref('')

/** null = the HOME itself, otherwise the selected profile. */
const modelsScopeProfile = computed(() => (modelsScope.value === MODELS_GLOBAL ? null : modelsScope.value))

const modelsScopePath = computed(() => {
  const home = store.homes.find((h) => h.id === homeId.value)
  if (!home) return ''
  const sep = home.path.includes('\\') ? '\\' : '/'
  const parts = modelsScopeProfile.value
    ? [home.path, 'profiles', modelsScopeProfile.value, 'cordis.patch.yml']
    : [home.path, 'cordis.patch.yml']
  return parts.join(sep)
})

const catalogError = ref('')
/** Guards against a stale scope's response racing in after a scope switch. */
const modelsRequests = latestRequest()

async function loadModels() {
  const request = modelsRequests.begin()
  providerRoutes.value = []
  catalog.value = null
  catalogError.value = ''
  if (!homeId.value || homeId.value === DEDICATED || !editingId.value) return
  modelsLoading.value = true
  catalogLoading.value = true
  try {
    const [routes, cat] = await Promise.all([
      api.listProviderRoutes(homeId.value, modelsScopeProfile.value, editingId.value),
      // A failed catalogue must not silently look like "nothing to add":
      // surface the error in the add dialog instead (no silent fallbacks).
      api.listProviderCatalog(editingId.value).then(
        (value) => ({ ok: true as const, value }),
        (error) => ({ ok: false as const, error }),
      ),
    ])
    if (!modelsRequests.isCurrent(request)) return
    providerRoutes.value = routes
    if (cat.ok) {
      catalog.value = cat.value
    } else {
      catalogError.value = String(cat.error)
      Message.error(catalogError.value)
    }
  } catch (e) {
    if (modelsRequests.isCurrent(request)) Message.error(String(e))
  } finally {
    if (modelsRequests.isCurrent(request)) {
      modelsLoading.value = false
      catalogLoading.value = false
    }
  }
}

watch(modelsScope, async () => {
  // Switching scopes invalidates any in-flight load so an older scope's
  // response can never overwrite the newer one's list.
  modelsRequests.invalidate()
  if (activeTab.value === 'models') await loadModels()
})

const PROVIDER_ID_RE = /^[a-z][a-z0-9-]*$/
const PROVIDER_API_OPTIONS = ['openai-completions', 'openai-responses', 'anthropic-messages'] as const

/** Editable projection of one provider route (issue #89). */
interface ProviderFormState {
  /** Catalogue id when adding a built-in provider; '' for custom/official. */
  catalogProviderId: string
  /** True for the synthetic DeepSeek card, credential-only. */
  official: boolean
  id: string
  displayName: string
  api: string
  baseUrl: string
  /** Write-only: never read back, only stored in `.credentials.yaml`. */
  apiKey: string
  models: ProviderModel[]
}

function emptyProviderForm(): ProviderFormState {
  return {
    catalogProviderId: '',
    official: false,
    id: '',
    displayName: '',
    api: '',
    baseUrl: '',
    apiKey: '',
    models: [],
  }
}

const providerForm = ref<ProviderFormState>(emptyProviderForm())
const providerEditVisible = ref(false)
const providerOriginalId = ref('')
const addModeVisible = ref(false)

/** The route being edited, if any (to reuse its stored credential). */
const providerEditTarget = ref<ProviderRoute | null>(null)

const providerTitle = computed(() => {
  if (providerForm.value.official) return t('instanceEdit.modelsDeepSeekTitle')
  // The edit check must come first: an existing built-in provider also has a
  // catalogue id, and its dialog is an edit, not an add.
  if (providerOriginalId.value) return t('instanceEdit.modelsEditTitle')
  if (providerForm.value.catalogProviderId) return t('instanceEdit.modelsAddCatalogTitle')
  return t('instanceEdit.modelsAddCustomTitle')
})

/** Built-in providers the form may add (OAuth-only ones are excluded). */
const catalogProviders = computed(() => (catalog.value?.providers ?? []).filter((p) => p.apiKey))

/** Catalogue entry for the route currently being added. */
const catalogEntry = computed(() =>
  catalog.value?.providers.find((p) => p.id === providerForm.value.catalogProviderId) ?? null,
)

function openAddCatalog() {
  addModeVisible.value = false
  const entry = catalogEntry.value
  if (!entry) return
  providerOriginalId.value = ''
  providerEditTarget.value = null
  providerForm.value = {
    ...emptyProviderForm(),
    catalogProviderId: entry.id,
    id: entry.id,
    displayName: entry.name,
    api: entry.api,
    baseUrl: entry.baseUrl,
    models: [],
  }
  providerEditVisible.value = true
}

function onPickCatalog(p: CatalogProvider) {
  addModeVisible.value = false
  providerOriginalId.value = ''
  providerEditTarget.value = null
  providerForm.value = {
    ...emptyProviderForm(),
    catalogProviderId: p.id,
    id: p.id,
    displayName: p.name,
    api: p.api,
    baseUrl: p.baseUrl,
    models: [],
  }
  providerEditVisible.value = true
}

function openAddCustom() {
  addModeVisible.value = false
  providerOriginalId.value = ''
  providerEditTarget.value = null
  providerForm.value = emptyProviderForm()
  providerEditVisible.value = true
}

function openEditProvider(route: ProviderRoute) {
  providerOriginalId.value = route.id
  providerEditTarget.value = route
  providerForm.value = {
    catalogProviderId: route.catalog ? route.id : '',
    official: route.official,
    id: route.id,
    displayName: route.displayName,
    api: route.api,
    baseUrl: route.baseUrl,
    apiKey: '',
    models: route.models.map((m) => ({ ...m })),
  }
  providerEditVisible.value = true
}

const providerIdError = computed(() => {
  if (!providerForm.value.catalogProviderId) {
    const id = providerForm.value.id.trim()
    if (!id) return t('instanceEdit.modelsErrIdRequired')
    if (!PROVIDER_ID_RE.test(id)) return t('instanceEdit.modelsErrIdPattern')
    if (providerRoutes.value.some((r) => r.id !== providerOriginalId.value && r.id === id)) {
      return t('instanceEdit.modelsErrIdDuplicated')
    }
  }
  return ''
})

const providerBaseUrlError = computed(() => {
  if (providerForm.value.catalogProviderId || providerForm.value.official) return ''
  return providerForm.value.baseUrl.trim() ? '' : t('instanceEdit.modelsErrBaseUrlRequired')
})

const providerApiError = computed(() => {
  if (providerForm.value.catalogProviderId || providerForm.value.official) return ''
  return providerForm.value.api ? '' : t('instanceEdit.modelsErrApiRequired')
})

/** A key is only mandatory where the route has nothing else to resolve to. */
const providerApiKeyError = computed(() => {
  const form = providerForm.value
  if (!form.official && !form.catalogProviderId) return ''
  // An existing credential may be kept by leaving the field blank.
  if (providerEditTarget.value?.credential?.configured) return ''
  return form.apiKey.trim() ? '' : t('instanceEdit.modelsErrApiKeyRequired')
})

const providerModelsError = computed(() => {
  if (providerForm.value.catalogProviderId || providerForm.value.official) return ''
  return providerForm.value.models.length > 0 ? '' : t('instanceEdit.modelsErrModelsRequired')
})

const providerFormValid = computed(
  () =>
    !providerIdError.value &&
    !providerApiError.value &&
    !providerBaseUrlError.value &&
    !providerApiKeyError.value &&
    !providerModelsError.value,
)

/** Built-in routes inherit their models from the catalogue; never store them. */
function formRoutesPayload(): ProviderRoute {
  const form = providerForm.value
  const catalog = !!form.catalogProviderId
  return {
    id: form.id.trim(),
    displayName: form.displayName.trim(),
    apiKeyEnv: '',
    api: catalog ? '' : form.api,
    baseUrl: catalog ? '' : form.baseUrl.trim(),
    catalog,
    official: form.official,
    models: catalog ? [] : form.models,
    extraKeys: [],
    credential: null,
  }
}

async function onSaveProvider() {
  if (!homeId.value || !editingId.value) return
  if (!providerFormValid.value) {
    Message.warning(t('instanceEdit.modelsErrForm'))
    return
  }
  const route = formRoutesPayload()
  providerSaving.value = true
  try {
    providerRoutes.value = await api.saveProviderRoute(
      homeId.value,
      modelsScopeProfile.value,
      editingId.value,
      route,
      providerOriginalId.value || null,
      providerForm.value.apiKey,
    )
    providerEditVisible.value = false
    Message.success(t('instanceEdit.modelsSaved', { name: route.displayName || route.id }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    providerSaving.value = false
  }
}

async function onDeleteProvider(route: ProviderRoute) {
  if (!homeId.value || !editingId.value) return
  modelsBusy.value = route.id
  try {
    providerRoutes.value = await api.deleteProviderRoute(
      homeId.value,
      modelsScopeProfile.value,
      editingId.value,
      route.id,
    )
    if (route.official) Message.success(t('instanceEdit.modelsDeepSeekCleared'))
    else Message.success(t('instanceEdit.modelsDeleted', { name: route.displayName || route.id }))
  } catch (e) {
    Message.error(String(e))
  } finally {
    modelsBusy.value = ''
  }
}

// --- Model discovery ("fetch available models") -----------------------------

const modelsPickerVisible = ref(false)
const modelsPickerLoading = ref(false)
const pickerModels = ref<CatalogModel[]>([])
const pickerSelected = computed(() => providerForm.value.models.map((m) => m.id))

async function openModelPicker() {
  const form = providerForm.value
  modelsPickerLoading.value = true
  modelsPickerVisible.value = true
  pickerModels.value = []
  try {
    // Only the custom form opens the picker: built-in providers inherit
    // their models from the catalogue and never show a model list here.
    const input: DiscoverModelsInput = {
      instanceId: editingId.value ?? '',
      homeId: homeId.value ?? '',
      profile: modelsScopeProfile.value,
      provider: null,
      baseUrl: form.baseUrl.trim(),
      api: form.api,
      apiKey: form.apiKey,
      routeId: providerOriginalId.value || null,
    }
    pickerModels.value = await api.discoverProviderModels(input)
  } catch (e) {
    Message.error(String(e))
  } finally {
    modelsPickerLoading.value = false
  }
}

function onAdoptModels(ids: string[]) {
  const known = new Map(providerForm.value.models.map((m) => [m.id, m]))
  const next: ProviderModel[] = ids.map((id) => {
    const existing = known.get(id)
    if (existing) return existing
    const catalog = pickerModels.value.find((m) => m.id === id)
    return {
      id,
      name: catalog?.name ?? id,
      contextWindow: catalog?.contextWindow ?? null,
      maxTokens: catalog?.maxTokens ?? null,
    }
  })
  providerForm.value.models = next
}

function removeModel(id: string) {
  providerForm.value.models = providerForm.value.models.filter((m) => m.id !== id)
}

// Manual model entry: the picker can fail (offline endpoint, unsupported
// listing), so a hand-typed row must work exactly the same (issue #89 #3).

/** Model IDs are endpoint-defined, so only whitespace is forbidden. */
const MODEL_ID_RE = /^\S+$/

const newModel = ref<{ id: string; name: string; contextWindow?: number; maxTokens?: number }>({
  id: '',
  name: '',
})

const newModelError = computed(() => {
  const id = newModel.value.id.trim()
  if (!id) return ''
  if (!MODEL_ID_RE.test(id)) return t('instanceEdit.modelsErrModelIdPattern')
  if (providerForm.value.models.some((m) => m.id === id)) {
    return t('instanceEdit.modelsErrModelIdDuplicated')
  }
  return ''
})

function addManualModel() {
  const id = newModel.value.id.trim()
  if (!id || newModelError.value) return
  const cw = newModel.value.contextWindow
  const mt = newModel.value.maxTokens
  providerForm.value.models = [
    ...providerForm.value.models,
    {
      id,
      name: newModel.value.name.trim() || id,
      contextWindow: cw && cw > 0 ? cw : null,
      maxTokens: mt && mt > 0 ? mt : null,
    },
  ]
  newModel.value = { id: '', name: '' }
}

const officialCard = computed(() => providerRoutes.value.find((r) => r.official) ?? null)
const configuredRoutes = computed(() => providerRoutes.value.filter((r) => !r.official))

// --- Launch shortcut (issue #9) -----------------------------------------------

/** Writes a dsh-launcher://launch .url shortcut for this instance + profile. */
async function createShortcut(profile: string) {
  if (!editingId.value) return
  const { save } = await import('@tauri-apps/plugin-dialog')
  const dest = await save({
    defaultPath: `${name.value.trim() || 'instance'}-${profile}.url`,
    filters: [{ name: 'Shortcut', extensions: ['url'] }],
  })
  if (typeof dest !== 'string') return
  try {
    await api.createLaunchShortcut(editingId.value, profile, dest)
    Message.success(t('instanceEdit.shortcutCreated', { path: dest }))
  } catch (e) {
    Message.error(String(e))
  }
}

// --- Plugins tab ---------------------------------------------------------------

const pluginProfile = ref<string>('')
const compatibility = ref<CompatibilityReportType | null>(null)
const compatibilityBusy = ref(false)
const compatibilityRequests = latestRequest()
watch([editingId, pluginProfile], () => { compatibilityRequests.invalidate(); compatibility.value = null })
async function checkCompatibility() {
  const id = editingId.value
  const profile = pluginProfile.value
  if (!id || !profile) return
  const request = compatibilityRequests.begin()
  compatibility.value = null
  compatibilityBusy.value = true
  try {
    const report = await api.checkPluginCompatibility(id, profile)
    if (compatibilityRequests.isCurrent(request)) compatibility.value = report
  } catch (e) {
    if (compatibilityRequests.isCurrent(request)) Message.error(String(e))
  } finally {
    if (compatibilityRequests.isCurrent(request)) compatibilityBusy.value = false
  }
}
function invalidateCompatibility() { compatibilityRequests.invalidate(); compatibility.value = null; compatibilityBusy.value = false }
const installedPlugins = ref<InstalledPlugin[]>([])
const pluginsLoading = ref(false)
const selectedPlugins = ref<string[]>([])
const pluginsBusy = ref(false)
/** Migrate-plugins dialog (move plugins from another instance's profile). */
const migrateVisible = ref(false)
/** Latest-version info per installed plugin id, from checkPluginUpdates (issue #27). */
const pluginUpdates = ref<Record<string, PluginUpdateInfo>>({})

const visiblePlugins = computed(() =>
  // Backend already excludes @deepseek-ai/*; double-filter for safety.
  installedPlugins.value.filter((p) => !p.id.startsWith('@deepseek-ai/')),
)

/**
 * 版本号显示：Git commit 哈希（40 位十六进制）只显示前 7 位。
 */
function displayVersion(v: string | undefined): string {
  if (v && /^[0-9a-f]{40}$/i.test(v)) return v.slice(0, 7)
  return v ?? ''
}

watch([pluginProfile, homeId], async () => {
  await loadPlugins()
})

// --- Storage redirection tab (issue #51) ---------------------------------------

// Issue #92: text columns have no fixed width — the tables below use auto
// layout (no scroll.x) so nowrap cells size to their content instead of
// wrapping; the wrapper scrolls horizontally when the table outgrows the card.
const storageColumns = [
  { title: t('instanceEdit.storageEntry'), dataIndex: 'entry', width: 180 },
  { title: t('instanceEdit.storageTarget'), slotName: 'storageTarget' },
  { title: t('instanceEdit.storageStatus'), slotName: 'storageStatus', width: 110, align: 'center' as const },
  { title: t('instances.table.actions'), slotName: 'storageActions', width: 190, align: 'center' as const },
]

const homeLinks = ref<HomeLinkInfo[]>([])
const homeLinksLoading = ref(false)
// Issue #91: rows are selectable so several entries can be redirected into one
// directory in a single batch operation.
const storageRowSelection = { type: 'checkbox' as const, showCheckedAll: true }
const storageSelected = ref<string[]>([])

function onStorageSelectionChange(keys: (string | number)[]) {
  storageSelected.value = keys.map(String)
}

const linkDialogVisible = ref(false)
const linkEntry = ref('')
// Issue #91: the target is split into a directory (picked via a directory
// dialog) plus an entry name alias — no more hand-typing of full paths.
const linkDir = ref('')
const linkAlias = ref('')
const linkBusy = ref(false)
/** Preset root candidates for the open dialog (issue #65), from the backend. */
const linkPresets = ref<HomeLinkSuggestion[]>([])
const linkPresetsLoading = ref(false)
/** Picked preset id in the single-entry dialog ('' = no preset). */
const linkPresetId = ref('')
/** The open entry is a directory (controls the file-entry hint). */
const linkIsDir = ref(true)
/** A browse/file dialog is currently open. */
const linkPicking = ref(false)
/** Batch redirection dialog (issue #91): directory only, the per-entry name
 * is kept from the existing target or defaults to the entry name. */
const batchDialogVisible = ref(false)
const batchDir = ref('')
const batchPresetId = ref('')

/** Preset cache per `<homeId>::<entry>` so reopening a dialog is instant. */
const linkPresetCache = new Map<string, HomeLinkSuggestion[]>()

/**
 * Preset options with resolved labels. The backend emits a bare `label_key`
 * (it never hardcodes prose); the `instanceEdit.` namespace lives here. An
 * unknown key falls back to the raw id rather than rendering an empty option.
 */
const linkPresetOptions = computed(() =>
  linkPresets.value.map((preset) => {
    const path = `instanceEdit.${preset.label_key}`
    return {
      ...preset,
      label: te(path) ? t(path) : preset.id,
    }
  }),
)

async function loadHomeLinks() {
  if (!homeId.value || homeId.value === DEDICATED) return
  // WSL (issue #49 G4): the backend hard-rejects storage redirection for WSL
  // HOMEs (links.rs), so calling it would surface a raw error toast on every
  // visit. The tab shows a capability notice instead; nothing to load.
  if (isWsl.value) {
    homeLinks.value = []
    return
  }
  homeLinksLoading.value = true
  try {
    homeLinks.value = await api.listHomeLinks(homeId.value)
  } catch (e) {
    Message.error(String(e))
  } finally {
    homeLinksLoading.value = false
  }
}

/** Joins a directory and a name using the directory's own separator, so a
 * Windows directory stays `D:\dsh-data\sessions` and a POSIX one `.../sessions`. */
function joinLinkPath(dir: string, name: string): string {
  const sep = dir.includes('\\') ? '\\' : '/'
  const trimmed = dir.replace(/[\\/]+$/, '')
  return `${trimmed}${sep}${name}`
}

/**
 * Splits a full target path into its directory and final name (issue #91).
 * A drive root keeps its separator (`D:\foo` → dir `D:\`), and a name-only
 * value (no separator at all) yields an empty directory.
 */
function splitLinkPath(p: string): { dir: string; name: string } {
  const norm = normalizeLinkPath(p)
  const idx = Math.max(norm.lastIndexOf('\\'), norm.lastIndexOf('/'))
  if (idx < 0) return { dir: '', name: norm }
  let dir = norm.slice(0, idx)
  if (/^[A-Za-z]:$/.test(dir)) dir += '\\'
  return { dir, name: norm.slice(idx + 1) }
}

/**
 * Normalizes a user/picker supplied path: strips the Windows `\\?\` long-path
 * prefix the native dialog may return (the backend `canonicalize` validation
 * is sensitive to it) and any trailing separator.
 *
 * Two edges are deliberate (issue #65 F3/F4):
 * - the verbatim **UNC** form `\\?\UNC\srv\share` would lose its leading `\\`
 *   if the prefix were stripped naively, degrading to the *relative* path
 *   `UNC\srv\share`; it is rewritten to `\\srv\share` instead;
 * - a bare **drive root** (`D:\`) must not become the drive-relative `D:` —
 *   the separator is put back once the trailing one is trimmed.
 */
function normalizeLinkPath(p: string): string {
  const stripped = p
    .trim()
    .replace(/^\\\\\?\\UNC\\/i, '\\\\')
    .replace(/^\\\\\?\\/, '')
  const trimmed = stripped.replace(/[\\/]+$/, '')
  if (/^[A-Za-z]:$/.test(trimmed)) return `${trimmed}\\`
  return trimmed || stripped
}

function cfgInsensitiveEqual(a: string, b: string): boolean {
  return a.toLowerCase() === b.toLowerCase()
}

/** True when `target` is the HOME itself or lives inside it (case-insensitive
 * on Windows). Mirrors the backend rule that rejects such targets.
 *
 * Both separators are folded to `/` before comparing, so a Windows HOME typed
 * with forward slashes (`C:/Users/x/.dsh`) is still caught (issue #65 F7).
 * The backend stays the authority; this is only a round-trip saver. */
function isInsideHome(target: string, home: string): boolean {
  const t = normalizeLinkPath(target).replace(/\\/g, '/')
  const h = normalizeLinkPath(home).replace(/\\/g, '/')
  if (!t || !h) return false
  // A drive root normalizes to `D:\` → `D:/`; trim it so the containment
  // check below stays a plain prefix test.
  const base = h.replace(/\/+$/, '')
  if (!base) return t.startsWith('/')
  if (cfgInsensitiveEqual(t, base)) return true
  return (
    t.length > base.length &&
    cfgInsensitiveEqual(t.slice(0, base.length), base) &&
    t.charAt(base.length) === '/'
  )
}

/**
 * Monotonic id bumped every time a dialog opens. A preset response that comes
 * back after a newer dialog was opened belongs to the previous entry and would
 * otherwise overwrite the current one's candidates (issue #65 F6).
 */
let linkPresetReqSeq = 0

async function loadLinkPresets(entry: string) {
  const reqId = linkPresetReqSeq
  const key = `${homeId.value}::${entry}`
  const cached = linkPresetCache.get(key)
  if (cached) {
    linkPresets.value = cached
    return
  }
  linkPresetsLoading.value = true
  try {
    const list = await api.suggestHomeLinkTargets(homeId.value!, entry)
    linkPresetCache.set(key, list)
    if (reqId !== linkPresetReqSeq) return
    linkPresets.value = list
  } catch (e) {
    // Presets are advisory: a failure must not block hand-entry. A stale
    // failure must not toast over an unrelated dialog either.
    if (reqId !== linkPresetReqSeq) return
    linkPresets.value = []
    Message.error(String(e))
  } finally {
    if (reqId === linkPresetReqSeq) linkPresetsLoading.value = false
  }
}

// A different HOME invalidates every cached candidate set, and the batch
// selection belongs to the previous HOME's entry list.
watch(homeId, () => {
  linkPresetCache.clear()
  linkPresets.value = []
  storageSelected.value = []
})

async function openLinkDialog(link: HomeLinkInfo) {
  linkEntry.value = link.entry
  linkIsDir.value = link.is_dir
  // An existing redirect prefills its own directory and name (issue #91);
  // a fresh entry starts blank and the alias defaults to the entry name.
  const { dir, name } = splitLinkPath(link.target)
  linkDir.value = dir
  linkAlias.value = link.target ? name : ''
  linkPresetId.value = ''
  linkPresets.value = []
  linkDialogVisible.value = true
  linkPresetReqSeq += 1
  await loadLinkPresets(link.entry)
}

/** Alias validation (issue #91): the name becomes the last path segment, so
 * it must not contain separators or Windows-reserved characters, and `.`/`..`
 * would escape the picked directory. Empty means "use the entry name". */
const linkAliasError = computed(() => {
  const alias = linkAlias.value.trim()
  if (!alias) return ''
  if (alias === '.' || alias === '..' || /[\\/:*?"<>|]/.test(alias)) {
    return t('instanceEdit.storageAliasInvalid')
  }
  return ''
})

/**
 * Fills a directory input from a preset root (issue #91): presets are plain
 * directories now — the entry name comes from the alias field — so they serve
 * file and directory entries alike. The directory stays editable (D3).
 */
function applyLinkPreset(id: unknown, dirRef: Ref<string>, presetIdRef: Ref<string>) {
  const presetId = typeof id === 'string' ? id : ''
  presetIdRef.value = presetId
  if (!presetId) return
  const preset = linkPresets.value.find((p) => p.id === presetId)
  if (!preset?.path) return
  dirRef.value = normalizeLinkPath(preset.path)
}

/** Template handlers: refs unwrap in templates, so the pickers go through
 * these script-level wrappers. */
function applyLinkPresetSingle(id: unknown) {
  applyLinkPreset(id, linkDir, linkPresetId)
}

function applyLinkPresetBatch(id: unknown) {
  applyLinkPreset(id, batchDir, batchPresetId)
}

/** The directory a browse dialog should open in: the first preset root when
 * it exists, else the HOME itself. */
function browseStartDir(): string {
  const existing = linkPresets.value.find((p) => p.exists) ?? linkPresets.value[0]
  const home = store.homes.find((h) => h.id === homeId.value)?.path ?? ''
  return normalizeLinkPath(existing?.path ?? '') || normalizeLinkPath(home)
}

/** Browse button: always a directory picker (issue #91) — the target file or
 * folder name lives in the alias field, not in a hand-typed full path. */
async function pickLinkDir(which: 'single' | 'batch') {
  if (linkPicking.value) return
  if (!api.isTauri) {
    // Browser preview: there is no native dialog, so guide the user instead of
    // throwing (the settings page does the same for its directory picker).
    Message.info(t('settings.browserPickHint'))
    return
  }
  linkPicking.value = true
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    // An already-typed directory wins (it is what the user is editing);
    // otherwise fall back to a candidate root so the dialog opens somewhere
    // meaningful instead of the HOME — which is rejected as a target anyway.
    const current = which === 'single' ? linkDir.value : batchDir.value
    const defaultPath = normalizeLinkPath(current) || browseStartDir()
    const picked = await open({
      directory: true,
      multiple: false,
      title: t('instanceEdit.storageBrowseDirTitle'),
      defaultPath,
    })
    if (typeof picked !== 'string') return
    if (which === 'single') {
      linkDir.value = normalizeLinkPath(picked)
      linkPresetId.value = ''
    } else {
      batchDir.value = normalizeLinkPath(picked)
      batchPresetId.value = ''
    }
  } catch (e) {
    Message.error(String(e))
  } finally {
    linkPicking.value = false
  }
}

async function confirmSetLink() {
  const dir = normalizeLinkPath(linkDir.value)
  if (!dir || linkAliasError.value) return
  const target = joinLinkPath(dir, linkAlias.value.trim() || linkEntry.value)
  // Frontend pre-check (the deterministic half only): a target inside the HOME
  // is always rejected by the backend, so catch it before the request.
  // Existence and UNC/relative-path subtleties stay the backend's call.
  const home = store.homes.find((h) => h.id === homeId.value)?.path
  if (home && isInsideHome(target, home)) {
    Message.error(t('instanceEdit.storageTargetInsideHome'))
    return
  }
  linkBusy.value = true
  try {
    await api.setHomeLink(homeId.value!, linkEntry.value, target)
    Message.success(t('instanceEdit.storageSetDone'))
    linkDialogVisible.value = false
    await store.refreshHomes()
    await loadHomeLinks()
    // The just-used root is now the "last used" preset: drop stale caches.
    linkPresetCache.clear()
  } catch (e) {
    Message.error(String(e))
  } finally {
    linkBusy.value = false
  }
}

function openBatchDialog() {
  batchDir.value = ''
  batchPresetId.value = ''
  linkPresets.value = []
  batchDialogVisible.value = true
  linkPresetReqSeq += 1
  // Preset roots are entry-independent (links.rs::suggest_targets), so the
  // first selected entry is as good a cache key as any.
  void loadLinkPresets(storageSelected.value[0] ?? 'sessions')
}

/**
 * Batch redirection (issue #91): every selected entry lands in the picked
 * directory under its own name — the basename of its existing target when it
 * already redirects somewhere, else the entry name. Entries are applied
 * sequentially; one failure does not stop the rest, and every failure is
 * surfaced in the summary instead of being swallowed.
 */
async function confirmBatchLink() {
  const dir = normalizeLinkPath(batchDir.value)
  if (!dir) return
  const home = store.homes.find((h) => h.id === homeId.value)?.path
  linkBusy.value = true
  let ok = 0
  const failures: string[] = []
  try {
    for (const entry of storageSelected.value) {
      const existing = homeLinks.value.find((l) => l.entry === entry)
      const alias = existing?.target ? splitLinkPath(existing.target).name : entry
      const target = joinLinkPath(dir, alias)
      if (home && isInsideHome(target, home)) {
        failures.push(`${entry}: ${t('instanceEdit.storageTargetInsideHome')}`)
        continue
      }
      try {
        await api.setHomeLink(homeId.value!, entry, target)
        ok += 1
      } catch (e) {
        failures.push(`${entry}: ${String(e)}`)
      }
    }
  } finally {
    linkBusy.value = false
  }
  if (failures.length === 0) {
    Message.success(t('instanceEdit.storageBatchDone', { count: ok }))
    batchDialogVisible.value = false
    storageSelected.value = []
  } else {
    const summary = t('instanceEdit.storageBatchPartial', {
      ok,
      total: storageSelected.value.length,
      errors: failures.join('；'),
    })
    if (ok > 0) Message.warning(summary)
    else Message.error(summary)
  }
  await store.refreshHomes()
  await loadHomeLinks()
  linkPresetCache.clear()
}

async function clearLink(link: HomeLinkInfo) {
  linkBusy.value = true
  try {
    await api.clearHomeLink(homeId.value!, link.entry)
    Message.success(t('instanceEdit.storageClearDone'))
    await store.refreshHomes()
    await loadHomeLinks()
  } catch (e) {
    Message.error(String(e))
  } finally {
    linkBusy.value = false
  }
}

// 进入插件页时若未选择 Profile：优先选中实例的默认 Profile；若实例没有
// 设置默认 Profile，则选中找到的第一个 Profile。
watch(activeTab, async (tab) => {
  if (tab === 'skills') {
    await loadSkills()
    return
  }
  if (tab === 'agents') {
    await loadAgentsMd()
    return
  }
  if (tab === 'mcp') {
    // The scope defaults to the instance's default profile when it has one:
    // that is where a per-instance MCP server usually belongs. Changing it
    // loads through the mcpScope watcher, so do not load twice here.
    if (
      mcpScope.value === MCP_GLOBAL &&
      defaultProfile.value &&
      profiles.value.includes(defaultProfile.value)
    ) {
      mcpScope.value = defaultProfile.value
      return
    }
    await loadMcpServers()
    return
  }
  if (tab === 'storage') {
    await loadHomeLinks()
    return
  }
  if (tab === 'models') {
    if (
      modelsScope.value === MODELS_GLOBAL &&
      defaultProfile.value &&
      profiles.value.includes(defaultProfile.value)
    ) {
      modelsScope.value = defaultProfile.value
      return
    }
    await loadModels()
    return
  }
  if (tab !== 'plugins') return
  if (pluginProfile.value) return
  if (profiles.value.length === 0) return
  if (defaultProfile.value && profiles.value.includes(defaultProfile.value)) {
    pluginProfile.value = defaultProfile.value
  } else {
    pluginProfile.value = profiles.value[0]
  }
})

async function loadPlugins() {
  invalidateCompatibility()
  installedPlugins.value = []
  selectedPlugins.value = []
  pluginUpdates.value = {}
  if (!editingId.value || !pluginProfile.value) return
  pluginsLoading.value = true
  try {
    installedPlugins.value = await api.listInstalledPlugins(editingId.value, pluginProfile.value)
    // Update detection is best-effort (network/registry): never block the list.
    try {
      const updates = await api.checkPluginUpdates(editingId.value, pluginProfile.value)
      pluginUpdates.value = Object.fromEntries(updates.map((u) => [u.id, u]))
    } catch {
      pluginUpdates.value = {}
    }
  } catch (e) {
    Message.error(String(e))
  } finally {
    pluginsLoading.value = false
  }
}

async function onTogglePlugin(p: InstalledPlugin, enabled: boolean) {
  if (!editingId.value || !pluginProfile.value) return
  invalidateCompatibility()
  pluginsBusy.value = true
  try {
    await api.setPluginsEnabled({
      instanceId: editingId.value,
      profile: pluginProfile.value,
      pluginIds: [p.id],
      enabled,
    })
    p.enabled = enabled
    Message.success(
      enabled
        ? t('instanceEdit.pluginEnabled', { name: p.id })
        : t('instanceEdit.pluginDisabled', { name: p.id }),
    )
  } catch (e) {
    Message.error(String(e))
    await loadPlugins()
  } finally {
    pluginsBusy.value = false
  }
}

async function onUninstallPlugin(p: InstalledPlugin) {
  if (!editingId.value || !pluginProfile.value) return
  invalidateCompatibility()
  pluginsBusy.value = true
  try {
    await api.uninstallPlugin({
      instanceId: editingId.value,
      profile: pluginProfile.value,
      pluginId: p.id,
    })
    Message.success(t('instanceEdit.pluginUninstalled', { name: p.id }))
    Message.info(t('instanceEdit.pluginRestartHint'))
    await loadPlugins()
  } catch (e) {
    Message.error(String(e))
  } finally {
    pluginsBusy.value = false
  }
}

function onSwitchChange(p: InstalledPlugin, val: string | number | boolean) {
  onTogglePlugin(p, val === true)
}

async function batchSetEnabled(enabled: boolean) {
  if (!editingId.value || !pluginProfile.value || selectedPlugins.value.length === 0) return
  invalidateCompatibility()
  pluginsBusy.value = true
  const ids = [...selectedPlugins.value]
  try {
    await api.setPluginsEnabled({
      instanceId: editingId.value,
      profile: pluginProfile.value,
      pluginIds: ids,
      enabled,
    })
    for (const p of installedPlugins.value) {
      if (ids.includes(p.id)) p.enabled = enabled
    }
    selectedPlugins.value = []
    Message.success(
      enabled
        ? t('instanceEdit.pluginsBatchEnabled', { count: ids.length })
        : t('instanceEdit.pluginsBatchDisabled', { count: ids.length }),
    )
  } catch (e) {
    Message.error(String(e))
    await loadPlugins()
  } finally {
    pluginsBusy.value = false
  }
}

function onSelectionChange(rowKeys: (string | number)[]) {
  selectedPlugins.value = rowKeys.map(String)
}

const rowSelection = {
  type: 'checkbox' as const,
  showCheckedAll: true,
  onlyCurrent: true,
}

// --- Plugin updates (issue #27) --------------------------------------------

/** Ids of installed plugins with a newer version available. */
const updatableIds = computed(() =>
  visiblePlugins.value.filter((p) => pluginUpdates.value[p.id]?.has_update).map((p) => p.id),
)

/** Selected rows that actually have an update available. */
const selectedUpdatableIds = computed(() =>
  selectedPlugins.value.filter((id) => updatableIds.value.includes(id)),
)

/** Update task ids started from this page; the list reloads as they settle. */
const pendingUpdateTasks = ref<string[]>([])

/** Starts one background install task per plugin at its latest stable version. */
async function updatePlugins(ids: string[]) {
  if (!editingId.value || !pluginProfile.value || ids.length === 0) return
  invalidateCompatibility()
  pluginsBusy.value = true
  let started = 0
  try {
    for (const id of ids) {
      const info = pluginUpdates.value[id]
      if (!info?.has_update || !info.latest) continue
      const taskId = await api.startInstallPluginTask({
        pluginId: id,
        version: info.latest,
        channel: info.channel ?? 'stable',
        instanceId: editingId.value,
        profile: pluginProfile.value,
        installSource: info.install_source ?? undefined,
      })
      pendingUpdateTasks.value.push(taskId)
      started += 1
    }
    if (started > 0) {
      Message.success(t('instanceEdit.pluginUpdatesStarted', { count: started }))
      await store.refreshTasks()
    }
  } catch (e) {
    Message.error(String(e))
  } finally {
    pluginsBusy.value = false
  }
}

const unlistenTaskProgress = ref<(() => void) | null>(null)
onMounted(async () => {
  unlistenTaskProgress.value = await api.onTaskProgress((p) => {
    const idx = pendingUpdateTasks.value.indexOf(p.id)
    if (idx === -1 || (p.state !== 'done' && p.state !== 'error' && p.state !== 'cancelled')) return
    pendingUpdateTasks.value.splice(idx, 1)
    if (p.state === 'done') {
      Message.success(t('instanceEdit.pluginUpdateDone'))
    } else if (p.state === 'error') {
      Message.error(p.message ?? t('instanceEdit.pluginUpdateFailed'))
    }
    loadPlugins()
  })
})
onBeforeUnmount(() => unlistenTaskProgress.value?.())

// --- Terminal tab ------------------------------------------------------------

const terminalRunning = ref(false)
</script>

<template>
  <div class="edit-page dl-split-page">
    <aside class="edit-sidebar dl-sidebar">
      <dsh-menu :selected-keys="[activeTab]" @menu-item-click="(key: string) => (activeTab = key as TabKey)">
        <dsh-menu-item key="basic">{{ t('instanceEdit.tabs.basic') }}</dsh-menu-item>
        <dsh-menu-item v-if="networkInstance" key="network">{{ t('network.title') }}</dsh-menu-item>
        <dsh-menu-item key="env">{{ t('instanceEdit.tabs.env') }}</dsh-menu-item>
        <dsh-menu-item key="profiles">{{ t('instanceEdit.tabs.profiles') }}</dsh-menu-item>
        <dsh-menu-item key="plugins">{{ t('instanceEdit.tabs.plugins') }}</dsh-menu-item>
        <dsh-menu-item key="skills">{{ t('instanceEdit.tabs.skills') }}</dsh-menu-item>
        <dsh-menu-item key="agents">{{ t('instanceEdit.tabs.agents') }}</dsh-menu-item>
        <dsh-menu-item key="mcp">{{ t('instanceEdit.tabs.mcp') }}</dsh-menu-item>
        <dsh-menu-item key="models">{{ t('instanceEdit.tabs.models') }}</dsh-menu-item>
        <dsh-menu-item key="storage">{{ t('instanceEdit.tabs.storage') }}</dsh-menu-item>
        <dsh-menu-item key="terminal">{{ t('instanceEdit.tabs.terminal') }}</dsh-menu-item>
      </dsh-menu>
    </aside>
    <section class="edit-content dl-page-content">
      <PageScrollbar>
        <div class="edit-inner">
          <!-- Basic settings -->
          <div v-if="activeTab === 'basic'" class="dl-card edit-card">
            <dsh-form layout="vertical" class="edit-form" :model="{}">
              <dsh-form-item :label="t('instanceEdit.name')" required>
                <dsh-space>
                  <dsh-input v-model="name" :placeholder="t('instanceEdit.namePlaceholder')" style="max-width: 360px" />
                  <dsh-tag v-if="isWsl" color="arcoblue">WSL · {{ wslDistro }}</dsh-tag>
                </dsh-space>
              </dsh-form-item>

              <dsh-form-item v-if="editingId">
                <template #label>
                  {{ t('instanceEdit.icon') }}
                  <HintIcon :content="t('instanceEdit.iconHint')" />
                </template>
                <div class="icon-editor">
                  <img v-if="iconUrl" :src="iconUrl" class="icon-preview" alt="" />
                  <img v-else src="@/assets/launcher-icon-v3.png" class="icon-preview" alt="" />
                  <div class="icon-actions">
                    <dsh-input
                      v-model="iconInput"
                      :placeholder="t('instanceEdit.iconUrlHint')"
                      allow-clear
                      style="max-width: 300px"
                    />
                    <dsh-space>
                      <dsh-button variant="secondary" :loading="iconBusy" :disabled="!iconInput.trim()" @click="applyIconInput">
                        {{ t('instanceEdit.iconApply') }}
                      </dsh-button>
                      <dsh-button variant="secondary" :loading="iconBusy" @click="pickIconFile">
                        {{ t('instanceEdit.iconPickFile') }}
                      </dsh-button>
                      <dsh-button variant="danger" v-if="iconUrl" @click="clearIcon">
                        {{ t('instanceEdit.iconClear') }}
                      </dsh-button>
                    </dsh-space>
                  </div>
                </div>
              </dsh-form-item>

              <dsh-form-item :label="t('instanceEdit.version')" required>
                <template v-if="versionOptions.length">
                  <dsh-select v-model="versionId" style="max-width: 360px">
                    <dsh-option v-for="v in versionOptions" :key="v.id" :value="v.id">{{ v.version }}</dsh-option>
                  </dsh-select>
                </template>
                <dsh-alert v-else type="warning">
                  {{ t('instanceEdit.noVersion') }}
                  <dsh-link href="#" @click.prevent="router.push({ name: 'download' })">{{ t('instanceEdit.goDownload') }}</dsh-link>
                </dsh-alert>
              </dsh-form-item>

              <dsh-form-item required>
                <template #label>
                  {{ t('instanceEdit.home') }}
                  <HintIcon v-if="isWsl" :content="t('instanceEdit.wslHomeFixed')" />
                  <HintIcon
                    v-else-if="homeId === DEDICATED"
                    :content="t('instanceEdit.dedicatedHomeHint', { path: dedicatedPath })"
                  />
                </template>
                <dsh-select v-model="homeId" style="max-width: 360px" :disabled="isWsl">
                  <dsh-option v-if="!isWsl" :value="DEDICATED">{{ t('instanceEdit.dedicatedHome') }}</dsh-option>
                  <dsh-option v-for="h in homeOptions" :key="h.id" :value="h.id">
                    {{ h.name }}（{{ h.path }}）
                  </dsh-option>
                </dsh-select>
                <dsh-input
                  v-if="homeId === DEDICATED && !isWsl"
                  v-model="homeName"
                  :placeholder="t('instanceEdit.homeNamePlaceholder')"
                  style="max-width: 360px; margin-top: 8px"
                />
              </dsh-form-item>

              <dsh-form-item v-if="editingId && hasTuiProfile">
                <template #label>
                  {{ t('instanceEdit.tuiCwd') }}
                  <HintIcon :content="t('instanceEdit.tuiCwdHint')" />
                </template>
                <dsh-space>
                  <dsh-input
                    v-model="tuiCwdInput"
                    :placeholder="t('instanceEdit.tuiCwdPlaceholder')"
                    allow-clear
                    style="width: 400px"
                    @press-enter="applyTuiCwd"
                  />
                  <dsh-button variant="secondary" :loading="tuiCwdBusy" @click="browseTuiCwd">
                    {{ t('instanceEdit.tuiCwdBrowse') }}
                  </dsh-button>
                  <dsh-button variant="secondary" :loading="tuiCwdBusy" @click="applyTuiCwd">
                    {{ t('instanceEdit.tuiCwdApply') }}
                  </dsh-button>
                </dsh-space>
              </dsh-form-item>

              <dsh-form-item v-if="editingId">
                <template #label>
                  {{ t('instanceEdit.files') }}
                  <HintIcon :content="t('instanceEdit.filesHint')" />
                </template>
                <dsh-space>
                  <dsh-button variant="secondary" size="sm" :loading="dirBusy" @click="onOpenDirectory">
                    {{ t('instanceEdit.openDirectory') }}
                  </dsh-button>
                  <dsh-button variant="secondary" size="sm" :loading="logBusy" @click="onViewLog">
                    {{ t('instanceEdit.viewLog') }}
                  </dsh-button>
                </dsh-space>
              </dsh-form-item>
            </dsh-form>

            <div class="footer-actions">
              <dsh-button variant="primary" size="lg" :disabled="!formValid" :loading="saving" @click="onSave">
                {{ t('instanceEdit.save') }}
              </dsh-button>
              <dsh-button variant="secondary" size="lg" @click="router.push({ name: 'home' })">{{ t('instanceEdit.cancel') }}</dsh-button>
            </div>
          </div>

          <InstanceNetwork v-else-if="activeTab === 'network' && networkInstance" :key="networkInstance.id" :instance="networkInstance" />

          <!-- Environment overrides -->
          <div v-else-if="activeTab === 'env'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.env') }}
              <HintIcon :content="t('instanceEdit.envDesc')" />
            </h4>

            <div v-for="(row, idx) in envRows" :key="idx" class="env-row">
              <dsh-input
                v-model="row.key"
                :placeholder="t('instanceEdit.envKey')"
                :status="envKeyError(row) ? 'error' : undefined"
                class="env-key"
              />
              <dsh-input v-model="row.value" :placeholder="t('instanceEdit.envValue')" class="env-value" />
              <dsh-button variant="danger-soft" @click="removeEnvRow(idx)">
                {{ t('instances.table.delete') }}
              </dsh-button>
              <div v-if="envKeyError(row)" class="env-error">{{ envKeyError(row) }}</div>
            </div>
            <dsh-empty v-if="envRows.length === 0" :description="t('instanceEdit.envAdd')" />
            <dsh-button variant="secondary" size="sm" class="env-add-btn" @click="addEnvRow">{{ t('instanceEdit.envAdd') }}</dsh-button>

            <div class="footer-actions">
              <dsh-button variant="primary" size="lg" :disabled="!formValid" :loading="saving" @click="onSave">
                {{ t('instanceEdit.save') }}
              </dsh-button>
              <dsh-button variant="secondary" size="lg" @click="router.push({ name: 'home' })">{{ t('instanceEdit.cancel') }}</dsh-button>
            </div>
          </div>

          <!-- Profiles -->
          <div v-else-if="activeTab === 'profiles'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.profiles') }}
              <HintIcon :content="t('instanceEdit.profilesDesc')" />
            </h4>
            <div class="profiles-toolbar">
              <dsh-button variant="secondary" size="sm" @click="startExportModpackMulti">
                {{ t('instanceEdit.modpackExportMulti') }}
              </dsh-button>
            </div>

            <template v-if="homeId && homeId !== DEDICATED">
              <div v-if="profiles.length === 0" class="profiles-empty">
                <dsh-empty :description="t('instanceEdit.profilesEmpty')" />
              </div>

              <div v-for="p in profiles" :key="p" class="profile-item">
                <template v-if="renamingProfile === p">
                  <dsh-input
                    v-model="renameValue"
                    class="profile-item-name"
                    :status="renameValue.trim() && renameValue.trim() !== p ? undefined : 'error'"
                    @press-enter="confirmRenameProfile"
                  />
                  <dsh-button variant="primary" size="sm" :loading="busyProfile === p" @click="confirmRenameProfile">
                    {{ t('instanceEdit.profileRenameSave') }}
                  </dsh-button>
                  <dsh-button variant="secondary" size="sm" @click="cancelRenameProfile">{{ t('instanceEdit.cancel') }}</dsh-button>
                </template>
                <template v-else-if="copyingProfile === p">
                  <dsh-input
                    v-model="copyProfileName"
                    class="profile-item-name"
                    :status="copyProfileName.trim() ? undefined : 'error'"
                    @press-enter="confirmCopyProfile"
                  />
                  <dsh-button variant="primary" size="sm" :loading="copyProfileBusy" @click="confirmCopyProfile">
                    {{ t('instanceEdit.profileCopySave') }}
                  </dsh-button>
                  <dsh-button variant="secondary" size="sm" @click="cancelCopyProfile">{{ t('instanceEdit.cancel') }}</dsh-button>
                </template>
                <template v-else>
                  <span class="profile-item-name">
                    {{ p }}
                    <dsh-tag v-if="defaultProfile === p" color="arcoblue" size="small">
                      {{ t('instanceEdit.profileDefaultTag') }}
                    </dsh-tag>
                  </span>
                  <span class="profile-item-actions">
                    <dsh-button variant="secondary" size="sm" @click="startRenameProfile(p)">{{ t('instanceEdit.profileRename') }}</dsh-button>
                    <dsh-button variant="secondary" size="sm" @click="startCopyProfile(p)">{{ t('instanceEdit.profileCopy') }}</dsh-button>
                    <dsh-button variant="secondary" size="sm" @click="startExportModpack(p)">{{ t('instanceEdit.modpackExport') }}</dsh-button>
                    <dsh-button variant="secondary" size="sm" @click="createShortcut(p)">{{ t('instanceEdit.createShortcut') }}</dsh-button>
                    <dsh-button variant="primary"
                      v-if="defaultProfile !== p"
                      size="sm"
                      @click="setDefaultProfile(p)"
                    >
                      {{ t('instanceEdit.profileSetDefaultBtn') }}
                    </dsh-button>
                    <dsh-popconfirm
                      :content="t('instanceEdit.profileDeleteConfirm', { name: p })"
                      @ok="confirmDeleteProfile(p)"
                    >
                      <dsh-button variant="danger" size="sm" :loading="busyProfile === p">
                        {{ t('instances.table.delete') }}
                      </dsh-button>
                    </dsh-popconfirm>
                  </span>
                </template>
              </div>

              <div v-if="addingProfile" class="profile-item">
                <dsh-input
                  v-model="newProfileName"
                  :placeholder="t('instanceEdit.profileCreatePlaceholder')"
                  class="profile-item-name"
                  @press-enter="onCreateProfile"
                />
                <dsh-button variant="primary" size="sm" :loading="creatingProfile" @click="onCreateProfile">
                  {{ t('instanceEdit.profileCreate') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" @click="cancelAddProfile">{{ t('instanceEdit.cancel') }}</dsh-button>
              </div>

              <dsh-button variant="secondary" v-if="!addingProfile" size="sm" class="profile-add-btn" @click="addingProfile = true">
                {{ t('instanceEdit.profileAdd') }}
              </dsh-button>
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>

            <div class="footer-actions">
              <dsh-button variant="primary" size="lg" :disabled="!formValid" :loading="saving" @click="onSave">
                {{ t('instanceEdit.save') }}
              </dsh-button>
              <dsh-button variant="secondary" size="lg" @click="router.push({ name: 'home' })">{{ t('instanceEdit.cancel') }}</dsh-button>
            </div>
          </div>

          <!-- Plugins -->
          <div v-else-if="activeTab === 'plugins'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.plugins') }}
              <HintIcon :content="t('instanceEdit.pluginsDesc')" />
            </h4>

            <template v-if="homeId && homeId !== DEDICATED">
              <div class="plugins-toolbar">
                <dsh-button variant="secondary" :disabled="!pluginProfile || pluginsBusy" :loading="compatibilityBusy" @click="checkCompatibility">
                  {{ t('compat.check') }}
                </dsh-button>
                <dsh-select
                  v-model="pluginProfile"
                  :placeholder="t('plugins.chooseProfile')"
                  style="width: 220px"
                >
                  <dsh-option v-for="p in profiles" :key="p" :value="p">{{ p }}</dsh-option>
                </dsh-select>
                <dsh-button variant="secondary"
                  :disabled="!pluginProfile"
                  @click="migrateVisible = true"
                >
                  {{ t('plugins.migrateOpen') }}
                </dsh-button>
                <dsh-button variant="secondary"
                  :disabled="!pluginProfile"
                  @click="importLocalPlugin"
                >
                  {{ t('instanceEdit.pluginImportLocal') }}
                </dsh-button>
                <dsh-button variant="primary"
                  :disabled="!pluginProfile || updatableIds.length === 0 || pluginsBusy"
                  @click="updatePlugins(updatableIds)"
                >
                  {{ t('instanceEdit.pluginUpdateAll', { count: updatableIds.length }) }}
                </dsh-button>
                <dsh-button variant="secondary"
                  :disabled="!pluginProfile"
                  :loading="pluginsLoading"
                  @click="loadPlugins"
                >
                  {{ t('common.refresh') }}
                </dsh-button>
              </div>
              <CompatibilityReport v-if="compatibility" :report="compatibility" />

              <template v-if="pluginProfile">
                <div :aria-busy="pluginsLoading" class="plugins-table">
                  <dsh-table
                    :data="visiblePlugins"
                    :inert="pluginsLoading"
                    :row-selection="rowSelection"
                    row-key="id"
                    :pagination="false"
                    :scroll="contentSizedTable"
                    @selection-change="onSelectionChange"
                  >
                    <template #columns>
                      <dsh-table-column
                        :title="t('instanceEdit.pluginNameId')"
                        data-index="id"
                        :header-cell-style="{ width: '1%', whiteSpace: 'nowrap' }"
                      >
                        <template #cell="{ record }">
                          <span v-if="record.name" class="plugin-cell-name" :title="pluginName(record.name, locale)">
                            {{ pluginName(record.name, locale) }}
                          </span>
                          <span class="plugin-cell-id" :class="{ 'plugin-cell-id-secondary': record.name }">
                            {{ record.id }}
                          </span>
                        </template>
                      </dsh-table-column>
                      <dsh-table-column :title="t('instanceEdit.pluginVersion')" data-index="version" cell-class="cell-nowrap">
                        <template #cell="{ record }">
                          <span v-if="pluginUpdates[record.id]?.has_update" class="plugin-update-available">
                            {{ pluginUpdates[record.id].current ?? displayVersion(record.version) }} →
                            {{ pluginUpdates[record.id].latest }}
                          </span>
                          <span v-else-if="record.version">{{
                            displayVersion(pluginUpdates[record.id]?.current ?? record.version)
                          }}</span>
                          <span v-else class="plugin-no-version">-</span>
                        </template>
                      </dsh-table-column>
                      <dsh-table-column :title="t('instanceEdit.pluginStatus')" data-index="enabled" :width="76" fixed="right">
                        <template #cell="{ record }">
                          <dsh-switch
                            :model-value="record.enabled"
                            :disabled="pluginsBusy"
                            :aria-label="t('instanceEdit.pluginStatus') + ': ' + record.id"
                            @change="onSwitchChange(record, $event)"
                          />
                        </template>
                      </dsh-table-column>
                      <dsh-table-column :title="t('instanceEdit.pluginActions')" :width="160" fixed="right">
                        <template #cell="{ record }">
                          <dsh-space>
                            <dsh-button variant="primary"
                              v-if="pluginUpdates[record.id]?.has_update"
                              size="sm"
                              :disabled="pluginsBusy"
                              @click="updatePlugins([record.id])"
                            >
                              {{ t('instanceEdit.pluginUpdate') }}
                            </dsh-button>
                            <dsh-popconfirm
                              :content="t('instanceEdit.pluginUninstallConfirm', { name: record.id })"
                              @ok="onUninstallPlugin(record)"
                            >
                              <dsh-button variant="danger" size="sm" :disabled="pluginsBusy">
                                {{ t('instances.table.delete') }}
                              </dsh-button>
                            </dsh-popconfirm>
                          </dsh-space>
                        </template>
                      </dsh-table-column>
                    </template>
                  </dsh-table>
                  <dsh-spin v-if="pluginsLoading" class="plugins-table-spinner" />
                </div>

                <div class="plugins-batch">
                  <dsh-button variant="primary"
                    size="sm"
                    :disabled="selectedPlugins.length === 0 || pluginsBusy"
                    @click="batchSetEnabled(true)"
                  >
                    {{ t('instanceEdit.pluginsBatchEnable', { count: selectedPlugins.length }) }}
                  </dsh-button>
                  <dsh-button variant="danger"
                    size="sm"
                    :disabled="selectedPlugins.length === 0 || pluginsBusy"
                    @click="batchSetEnabled(false)"
                  >
                    {{ t('instanceEdit.pluginsBatchDisable', { count: selectedPlugins.length }) }}
                  </dsh-button>
                  <dsh-button variant="primary"
                    size="sm"
                    :disabled="selectedUpdatableIds.length === 0 || pluginsBusy"
                    @click="updatePlugins(selectedUpdatableIds)"
                  >
                    {{ t('instanceEdit.pluginsBatchUpdate', { count: selectedUpdatableIds.length }) }}
                  </dsh-button>
                </div>

                <dsh-empty
                  v-if="!pluginsLoading && visiblePlugins.length === 0"
                  :description="t('instanceEdit.pluginsEmpty')"
                />
              </template>
              <dsh-empty v-else :description="t('instanceEdit.pluginsPickProfile')" />
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>

            <MigratePluginsDialog
              v-model:visible="migrateVisible"
              :target-instance-id="editingId ?? ''"
              :target-profile="pluginProfile"
              :existing-ids="installedPlugins.map((p) => p.id)"
            />
          </div>

          <!-- SKILL -->
          <div v-else-if="activeTab === 'skills'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.skills') }}
              <HintIcon :content="t('instanceEdit.skillsDesc')" />
            </h4>

            <template v-if="homeId && editingId">
              <div class="skill-toolbar">
                <dsh-button variant="primary" size="sm" @click="skillRepoDialogVisible = true">
                  {{ t('instanceEdit.skillInstall') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" @click="onImportSkillFile">
                  {{ t('instanceEdit.skillImportFile') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" @click="onImportSkillZip">
                  {{ t('instanceEdit.skillImportZip') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" @click="skillCreateVisible = true">
                  {{ t('instanceEdit.skillCreate') }}
                </dsh-button>
                <dsh-button variant="secondary"
                  size="sm"
                  :loading="skillCheckingUpdates"
                  @click="onCheckSkillUpdates"
                >
                  {{ t('instanceEdit.skillCheckUpdates') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" :loading="skillOpeningDir" @click="onOpenSkillsDir">
                  {{ t('instanceEdit.skillOpenDir') }}
                </dsh-button>
                <dsh-button variant="secondary"
                  size="sm"
                  :disabled="skillSelectedKeys.length === 0"
                  :loading="skillExporting"
                  @click="onExportSkills"
                >
                  {{ t('instanceEdit.skillExport') }}
                </dsh-button>
                <dsh-button variant="secondary"
                  v-if="skillUpdates.length > 0"
                  size="sm"
                  status="warning"
                  :loading="skillUpdatingAll"
                  @click="onUpdateAllSkills"
                >
                  {{ t('instanceEdit.skillUpdateAll', { count: skillUpdates.length }) }}
                </dsh-button>
              </div>

              <dsh-table
                :columns="skillColumns"
                :data="skills"
                :loading="skillsLoading"
                :pagination="false"
                :scroll="contentSizedTable"
                :row-selection="rowSelection"
                row-key="name"
                size="small"
                @selection-change="onSkillSelectionChange"
              >
                <template #skillName="{ record }">
                  <span class="cell-nowrap">{{ record.name }}</span>
                </template>
                <template #skillDesc="{ record }">
                  <dsh-tooltip :content="record.description" :disabled="!record.description">
                    <span class="cell-ellipsis">{{ record.description }}</span>
                  </dsh-tooltip>
                </template>
                <template #origin="{ record }">
                  <div v-if="record.origin" class="cell-nowrap">
                    <dsh-tag size="small" color="blue">{{ record.origin.tag ?? record.origin.commit.slice(0, 7) }}</dsh-tag>
                    <dsh-tooltip :content="record.origin.repo">
                      <span class="skill-repo-ref">{{ shortRepoName(record.origin.repo) }}</span>
                    </dsh-tooltip>
                    <dsh-tag v-if="skillUpdateOf(record.name)" size="small" color="orange">
                      {{ t('instanceEdit.skillHasUpdate', { version: skillUpdateOf(record.name)!.latest }) }}
                    </dsh-tag>
                  </div>
                  <span v-else class="skill-repo-ref">—</span>
                </template>
                <template #skillActions="{ record }">
                  <dsh-space>
                    <dsh-button variant="secondary"
                      v-if="record.origin"
                      size="sm"
                      :status="skillUpdateOf(record.name) ? 'warning' : 'normal'"
                      :loading="skillActionBusy === record.name || skillUpdatingAll"
                      @click="onUpdateSkill(record.name)"
                    >
                      {{ t('instanceEdit.skillUpdate') }}
                    </dsh-button>
                    <dsh-popconfirm
                      :content="t('instanceEdit.skillDeleteConfirm', { name: record.name })"
                      @ok="onDeleteSkill(record.name)"
                    >
                      <dsh-button variant="danger" size="sm" :loading="skillActionBusy === record.name">
                        {{ t('instances.table.delete') }}
                      </dsh-button>
                    </dsh-popconfirm>
                  </dsh-space>
                </template>
                <template #empty>
                  <dsh-empty :description="t('instanceEdit.skillsEmpty')" />
                </template>
              </dsh-table>
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>
          </div>

          <!-- AGENTS.md (issue #57): preview-first; save right-aligned on the
               toolbar row so it lines up with 编辑/刷新. -->
          <div v-else-if="activeTab === 'agents'" class="dl-card edit-card">
            <div class="agents-head">
              <h4 class="env-title">
                {{ t('instanceEdit.tabs.agents') }}
                <HintIcon :content="t('instanceEdit.agentsDesc')" />
              </h4>
            </div>

            <template v-if="homeId && editingId">
              <div class="skill-toolbar">
                <dsh-button variant="secondary" size="sm" @click="agentsPreview = !agentsPreview">
                  {{ agentsPreview ? t('instanceEdit.agentsEdit') : t('instanceEdit.agentsPreview') }}
                </dsh-button>
                <dsh-button variant="secondary" size="sm" :loading="agentsLoading" @click="loadAgentsMd">
                  {{ t('common.refresh') }}
                </dsh-button>
                <dsh-button variant="primary"
                  size="sm"
                  class="agents-save"
                  :loading="agentsSaving"
                  @click="saveAgentsMd(true)"
                >
                  {{ t('instanceEdit.agentsSave') }}
                </dsh-button>
              </div>
              <dsh-scrollbar outer-style="height: 480px" style="height: 480px; overflow-y: auto">
                <dsh-textarea
                  v-if="!agentsPreview"
                  v-model="agentsContent"
                  class="agents-editor"
                  :auto-size="{ minRows: 20 }"
                  :placeholder="t('instanceEdit.agentsEmpty')"
                  @keydown="onAgentsEditorKeydown"
                />
                <!-- eslint-disable-next-line vue/no-v-html -- sanitized by renderMarkdown -->
                <div
                  v-else
                  class="agents-preview markdown-body"
                  v-html="agentsPreviewHtml || `<p class='agents-empty'>${t('instanceEdit.agentsEmpty')}</p>`"
                />
              </dsh-scrollbar>
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>
          </div>

          <!-- MCP -->
          <!-- MCP servers (issue #93: the card accepts a pasted mcpServers JSON) -->
          <div v-else-if="activeTab === 'mcp'" class="dl-card edit-card" @paste="onMcpPagePaste">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.mcp') }}
              <HintIcon :content="t('instanceEdit.mcpDesc')" />
            </h4>

            <template v-if="homeId && homeId !== DEDICATED">
              <div class="mcp-toolbar">
                <dsh-select v-model="mcpScope" style="width: 300px">
                  <dsh-option :value="MCP_GLOBAL">{{ t('instanceEdit.mcpScopeGlobal') }}</dsh-option>
                  <dsh-option v-for="p in profiles" :key="p" :value="p">
                    {{ t('instanceEdit.mcpScopeProfile') }} · {{ p }}
                  </dsh-option>
                </dsh-select>
                <dsh-button variant="primary" @click="openMcpCreate">
                  {{ t('instanceEdit.mcpAdd') }}
                </dsh-button>
                <dsh-button variant="ghost" :loading="mcpLoading" @click="loadMcpServers">
                  ⟳
                </dsh-button>
              </div>
              <p class="mcp-path">{{ t('instanceEdit.mcpScopePath', { path: mcpScopePath }) }}</p>
              <p class="mcp-path">{{ t('instanceEdit.mcpPasteTip') }}</p>

              <dsh-table
                :columns="mcpColumns"
                :data="mcpServers"
                :loading="mcpLoading"
                :pagination="false"
                :scroll="contentSizedTable"
                row-key="id"
                size="small"
              >
                <template #mcpName="{ record }">
                  <span class="cell-nowrap">{{ record.serverName }}</span>
                </template>
                <template #mcpTransport="{ record }">
                  <dsh-tag size="small" :color="record.transport === 'stdio' ? 'arcoblue' : 'green'">
                    {{
                      record.transport === 'stdio'
                        ? t('instanceEdit.mcpTransportStdio')
                        : t('instanceEdit.mcpTransportHttp')
                    }}
                  </dsh-tag>
                </template>
                <template #mcpTarget="{ record }">
                  <dsh-tooltip :content="mcpTargetText(record)" :disabled="!mcpTargetText(record)">
                    <span class="mcp-target cell-ellipsis">{{ mcpTargetText(record) }}</span>
                  </dsh-tooltip>
                </template>
                <template #mcpStatus="{ record }">
                  <dsh-switch
                    :model-value="record.enabled"
                    :disabled="mcpBusy === record.id"
                    :aria-label="t('instanceEdit.mcpColStatus') + ': ' + record.serverName"
                    @change="onToggleMcpServer(record, $event === true)"
                  />
                </template>
                <template #mcpActions="{ record }">
                  <dsh-space>
                    <dsh-button variant="secondary" size="sm" :disabled="mcpBusy === record.id" @click="openMcpEdit(record)">
                      {{ t('instanceEdit.mcpEdit') }}
                    </dsh-button>
                    <dsh-popconfirm
                      :content="t('instanceEdit.mcpDeleteConfirm', { name: record.serverName })"
                      @ok="onDeleteMcpServer(record)"
                    >
                      <dsh-button variant="danger" size="sm" :loading="mcpBusy === record.id">
                        {{ t('instances.table.delete') }}
                      </dsh-button>
                    </dsh-popconfirm>
                  </dsh-space>
                </template>
                <template #empty>
                  <dsh-empty :description="t('instanceEdit.mcpEmpty')" />
                </template>
              </dsh-table>
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>
          </div>

          <!-- Models (issue #89): the same "Settings → Models" flow DSH ships -->
          <div v-else-if="activeTab === 'models'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.models') }}
              <HintIcon :content="t('instanceEdit.modelsDesc')" />
            </h4>

            <template v-if="homeId && homeId !== DEDICATED && editingId">
              <div class="models-toolbar">
                <dsh-select v-model="modelsScope" style="width: 300px">
                  <dsh-option :value="MODELS_GLOBAL">{{ t('instanceEdit.modelsScopeGlobal') }}</dsh-option>
                  <dsh-option v-for="p in profiles" :key="p" :value="p">
                    {{ t('instanceEdit.modelsScopeProfile') }} · {{ p }}
                  </dsh-option>
                </dsh-select>
                <dsh-button variant="primary" @click="addModeVisible = true">
                  {{ t('instanceEdit.modelsAdd') }}
                </dsh-button>
                <dsh-button variant="ghost" :loading="modelsLoading" @click="loadModels">⟳</dsh-button>
              </div>
              <p class="models-path">{{ t('instanceEdit.modelsScopePath', { path: modelsScopePath }) }}</p>

              <dsh-spin :loading="modelsLoading" class="models-body">
                <!-- DeepSeek official card: credential-only -->
                <div v-if="officialCard" class="model-card model-card-official">
                  <div class="model-card-head">
                    <span class="model-card-id">DeepSeek</span>
                    <dsh-tag v-if="officialCard.credential?.configured" color="green">
                      {{ t('instanceEdit.modelsConfigured') }}
                    </dsh-tag>
                    <dsh-tag v-else color="gray">{{ t('instanceEdit.modelsNotConfigured') }}</dsh-tag>
                  </div>
                  <p class="model-card-sub">{{ t('instanceEdit.modelsDeepSeekHint') }}</p>
                  <div class="model-card-actions">
                    <dsh-button variant="secondary" size="sm" @click="openEditProvider(officialCard)">
                      {{ t('instanceEdit.modelsConfigure') }}
                    </dsh-button>
                    <dsh-popconfirm
                      :content="t('instanceEdit.modelsDeepSeekClearConfirm')"
                      @ok="onDeleteProvider(officialCard)"
                    >
                      <dsh-button variant="danger" size="sm" :loading="modelsBusy === officialCard.id">
                        {{ t('instanceEdit.modelsClear') }}
                      </dsh-button>
                    </dsh-popconfirm>
                  </div>
                </div>

                <!-- Configured catalogue / custom routes -->
                <div v-for="route in configuredRoutes" :key="route.id" class="model-card">
                  <div class="model-card-head">
                    <span class="model-card-id">{{ route.displayName || route.id }}</span>
                    <dsh-tag v-if="route.catalog" color="arcoblue" size="small">
                      {{ t('instanceEdit.modelsCatalogTag') }}
                    </dsh-tag>
                    <dsh-tag v-if="route.credential?.configured" color="green" size="small">
                      {{ t('instanceEdit.modelsConfigured') }}
                    </dsh-tag>
                    <dsh-tag v-else color="gray" size="small">{{ t('instanceEdit.modelsNotConfigured') }}</dsh-tag>
                  </div>
                  <p v-if="route.api" class="model-card-sub">{{ route.api }} · {{ route.baseUrl }}</p>
                  <p v-else class="model-card-sub">{{ route.id }}</p>
                  <p class="model-card-models">
                    {{ t('instanceEdit.modelsCount', { count: route.models.length }) }}
                  </p>
                  <div class="model-card-actions">
                    <dsh-button variant="secondary" size="sm" :disabled="modelsBusy === route.id" @click="openEditProvider(route)">
                      {{ t('instanceEdit.modelsEdit') }}
                    </dsh-button>
                    <dsh-popconfirm
                      :content="t('instanceEdit.modelsDeleteConfirm', { name: route.displayName || route.id })"
                      @ok="onDeleteProvider(route)"
                    >
                      <dsh-button variant="danger" size="sm" :loading="modelsBusy === route.id">
                        {{ t('instances.table.delete') }}
                      </dsh-button>
                    </dsh-popconfirm>
                  </div>
                </div>

                <dsh-empty v-if="!modelsLoading && !officialCard && configuredRoutes.length === 0" :description="t('instanceEdit.modelsEmpty')" />
              </dsh-spin>
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>
          </div>

          <!-- Storage redirection (issue #51) -->
          <div v-else-if="activeTab === 'storage'" class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.storage') }}
              <HintIcon :content="t('instanceEdit.storageDesc')" />
            </h4>

            <!-- WSL (issue #49 G4): storage redirection is not implemented for
                 WSL HOMEs — the backend rejects it outright (links.rs). Surface
                 that as a scoped capability notice here instead of letting the
                 user click through to a raw backend error. -->
            <dsh-alert v-if="isWsl" type="info" class="storage-caveat">
              {{ t('instanceEdit.storageWslUnsupported') }}
            </dsh-alert>

            <template v-if="homeId && homeId !== DEDICATED && !isWsl">
              <dsh-alert type="warning" class="storage-caveat">
                {{ t('instanceEdit.storageCaveat') }}
              </dsh-alert>
              <!-- Issue #91: check a set of entries and redirect them all into
                   one directory, keeping each entry's current name. -->
              <div class="storage-toolbar">
                <dsh-button variant="secondary"
                  size="sm"
                  :disabled="storageSelected.length === 0 || linkBusy"
                  @click="openBatchDialog"
                >
                  {{ t('instanceEdit.storageBatchSet') }}
                  <template v-if="storageSelected.length">({{ storageSelected.length }})</template>
                </dsh-button>
              </div>
              <div class="table-scroll">
                <dsh-table
                  :columns="storageColumns"
                  :data="homeLinks"
                  :loading="homeLinksLoading"
                  :pagination="false"
                  row-key="entry"
                  :row-selection="storageRowSelection"
                  :selected-keys="storageSelected"
                  size="small"
                  @selection-change="onStorageSelectionChange"
                >
                <template #storageTarget="{ record }">
                  <span v-if="record.target" class="cell-nowrap">{{ record.target }}</span>
                  <span v-else class="storage-default">{{ t('instanceEdit.storageDefault') }}</span>
                </template>
                <template #storageStatus="{ record }">
                  <dsh-tag v-if="record.active" color="green" size="small">
                    {{ t('instanceEdit.storageActive') }}
                  </dsh-tag>
                  <dsh-tag v-else-if="record.target" color="orange" size="small">
                    {{ t('instanceEdit.storageInactive') }}
                  </dsh-tag>
                  <span v-else>-</span>
                </template>
                <template #storageActions="{ record }">
                  <dsh-button variant="secondary" size="sm" :disabled="linkBusy" @click="openLinkDialog(record)">
                    {{ record.target ? t('instanceEdit.storageModify') : t('instanceEdit.storageSet') }}
                  </dsh-button>
                  <dsh-popconfirm
                    v-if="record.target"
                    :content="t('instanceEdit.storageClearConfirm')"
                    @ok="clearLink(record)"
                  >
                    <dsh-button variant="danger" size="sm" :disabled="linkBusy">
                      {{ t('instanceEdit.storageClear') }}
                    </dsh-button>
                  </dsh-popconfirm>
                </template>
                </dsh-table>
              </div>
            </template>

            <dsh-alert v-else-if="!isWsl" type="info">
              {{ t('instanceEdit.profilesNeedHome') }}
            </dsh-alert>
          </div>

          <!-- Terminal -->
          <div v-else class="dl-card edit-card">
            <h4 class="env-title">
              {{ t('instanceEdit.tabs.terminal') }}
              <HintIcon :content="t('instanceEdit.terminalDesc')" />
            </h4>

            <template v-if="editingId">
              <TerminalEmbed
                v-if="editingId"
                :key="editingId"
                :instance-id="editingId"
                class="terminal-embed"
                @status="(v: boolean) => (terminalRunning = v)"
              />
            </template>

            <dsh-alert v-else type="info">
              {{ t('instanceEdit.terminalNoHome') }}
            </dsh-alert>
          </div>
        </div>
      </PageScrollbar>
    </section>

    <!-- SKILL repo install picker -->
    <SkillRepoDialog
      v-if="editingId && homeId"
      v-model:visible="skillRepoDialogVisible"
      :home-id="homeId"
      @installed="loadSkills"
    />

    <!-- SKILL create -->
    <dsh-modal
      v-model:visible="skillCreateVisible"
      :title="t('instanceEdit.skillCreateTitle')"
      :ok-loading="skillCreateBusy"
      :ok-button-props="{ disabled: !skillCreateForm.name.trim() || !skillCreateForm.content.trim() }"
      @ok="onCreateSkill"
    >
      <dsh-form :model="skillCreateForm" layout="vertical">
        <dsh-form-item :label="t('instanceEdit.skillName')" required>
          <dsh-input v-model="skillCreateForm.name" placeholder="my-skill" />
        </dsh-form-item>
        <dsh-form-item :label="t('instanceEdit.skillDescription')">
          <dsh-input v-model="skillCreateForm.description" />
        </dsh-form-item>
        <dsh-form-item :label="t('instanceEdit.skillContent')" required>
          <dsh-textarea
            v-model="skillCreateForm.content"
            :auto-size="{ minRows: 6, maxRows: 14 }"
            :placeholder="t('instanceEdit.skillContentHint')"
          />
        </dsh-form-item>
      </dsh-form>
    </dsh-modal>

    <!-- MCP server create / edit -->
    <dsh-modal
      v-model:visible="mcpEditVisible"
      :title="
        mcpOriginalId
          ? t('instanceEdit.mcpEditTitle', { name: mcpForm.serverName })
          : t('instanceEdit.mcpCreateTitle')
      "
      :width="680"
      :ok-loading="mcpSaving"
      :ok-button-props="{ disabled: !mcpFormValid }"
      @ok="onSaveMcpServer"
    >
      <!-- Issue #93: the dialog grew tall with rows; cap the body and scroll.
           A pasted mcpServers JSON anywhere in the form fills the first entry. -->
      <div class="mcp-dialog-body" @paste="onMcpDialogPaste">
      <dsh-form :model="mcpForm" layout="vertical">
        <dsh-form-item
          :label="t('instanceEdit.mcpServerName')"
          required
          :validate-status="mcpNameError ? 'error' : undefined"
          :help="mcpNameError || t('instanceEdit.mcpServerNameHint')"
        >
          <dsh-input v-model="mcpForm.serverName" placeholder="codegraph" />
        </dsh-form-item>

        <dsh-form-item :label="t('instanceEdit.mcpTransport')">
          <dsh-radio-group v-model="mcpForm.transport" type="button">
            <dsh-radio value="stdio">{{ t('instanceEdit.mcpTransportStdio') }}</dsh-radio>
            <dsh-radio value="streamable-http">{{ t('instanceEdit.mcpTransportHttp') }}</dsh-radio>
          </dsh-radio-group>
        </dsh-form-item>

        <!-- Streamable HTTP: endpoint + request headers -->
        <template v-if="mcpForm.transport === 'streamable-http'">
          <dsh-form-item
            :label="t('instanceEdit.mcpUrl')"
            required
            :validate-status="mcpUrlError ? 'error' : undefined"
            :help="mcpUrlError || undefined"
          >
            <dsh-input v-model="mcpForm.url" placeholder="http://127.0.0.1:64342/stream" />
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.mcpHeaders')">
            <div class="mcp-rows">
              <div v-for="(row, idx) in mcpForm.headers" :key="idx" class="env-row">
                <dsh-input
                  v-model="row.key"
                  :placeholder="t('instanceEdit.mcpHeaderKey')"
                  :status="mcpHeaderKeyError(idx) ? 'error' : undefined"
                  class="env-key"
                />
                <dsh-input
                  v-model="row.value"
                  :placeholder="t('instanceEdit.mcpHeaderValue')"
                  class="env-value"
                />
                <dsh-button variant="danger-soft" @click="mcpForm.headers.splice(idx, 1)">
                  {{ t('instances.table.delete') }}
                </dsh-button>
                <div v-if="mcpHeaderKeyError(idx)" class="env-error">{{ mcpHeaderKeyError(idx) }}</div>
              </div>
              <dsh-button variant="secondary" size="sm" class="env-add-btn" @click="addMcpHeaderRow">
                {{ t('instanceEdit.mcpHeaderAdd') }}
              </dsh-button>
            </div>
          </dsh-form-item>
        </template>

        <!-- stdio: command + args + env + cwd -->
        <template v-else>
          <dsh-form-item
            :label="t('instanceEdit.mcpCommand')"
            required
            :validate-status="mcpCommandError ? 'error' : undefined"
            :help="mcpCommandError || undefined"
          >
            <dsh-input v-model="mcpForm.command" :placeholder="t('instanceEdit.mcpCommandPlaceholder')" />
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.mcpArgs')" :extra="t('instanceEdit.mcpArgPlaceholder')">
            <div class="mcp-rows">
              <div v-for="(_arg, idx) in mcpForm.args" :key="idx" class="env-row">
                <dsh-input v-model="mcpForm.args[idx]" class="env-value" />
                <dsh-button variant="danger-soft" @click="mcpForm.args.splice(idx, 1)">
                  {{ t('instances.table.delete') }}
                </dsh-button>
              </div>
              <dsh-button variant="secondary" size="sm" class="env-add-btn" @click="addMcpArgRow">
                {{ t('instanceEdit.mcpArgAdd') }}
              </dsh-button>
            </div>
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.mcpEnv')">
            <div class="mcp-rows">
              <div v-for="(row, idx) in mcpForm.env" :key="idx" class="env-row">
                <dsh-input
                  v-model="row.key"
                  :placeholder="t('instanceEdit.envKey')"
                  :status="mcpEnvKeyError(idx) ? 'error' : undefined"
                  class="env-key"
                />
                <dsh-input v-model="row.value" :placeholder="t('instanceEdit.envValue')" class="env-value" />
                <dsh-button variant="danger-soft" @click="mcpForm.env.splice(idx, 1)">
                  {{ t('instances.table.delete') }}
                </dsh-button>
                <div v-if="mcpEnvKeyError(idx)" class="env-error">{{ mcpEnvKeyError(idx) }}</div>
              </div>
              <dsh-button variant="secondary" size="sm" class="env-add-btn" @click="addMcpEnvRow">
                {{ t('instanceEdit.mcpEnvAdd') }}
              </dsh-button>
            </div>
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.mcpCwd')">
            <dsh-input v-model="mcpForm.cwd" :placeholder="t('instanceEdit.mcpCwdPlaceholder')" />
          </dsh-form-item>
        </template>

        <dsh-form-item>
          <dsh-switch v-model="mcpForm.enabled" />
          <span class="switch-label">{{ t('instanceEdit.mcpEnabledLabel') }}</span>
        </dsh-form-item>

        <dsh-alert v-if="mcpExtraKeys.length" type="info">
          {{ t('instanceEdit.mcpExtraKept', { keys: mcpExtraKeys.join(', ') }) }}
        </dsh-alert>
      </dsh-form>
      </div>
    </dsh-modal>

    <!-- Model provider editor (issue #89) -->
    <dsh-modal
      v-model:visible="providerEditVisible"
      modalClass="model-provider-modal"
      :title="providerTitle"
      :width="680"
      :ok-loading="providerSaving"
      :ok-button-props="{ disabled: !providerFormValid }"
      @ok="onSaveProvider"
    >
      <dsh-form :model="providerForm" layout="vertical">
        <!-- API key: write-only, never read back -->
        <dsh-form-item
          :label="t('instanceEdit.modelsApiKey')"
          :required="!!providerApiKeyError"
          :validate-status="providerApiKeyError ? 'error' : providerEditTarget?.credential?.overriddenByInstance ? 'warning' : undefined"
          :help="
            providerApiKeyError ||
            (providerEditTarget?.credential?.overriddenByInstance
              ? t('instanceEdit.modelsKeyOverridden')
              : providerEditTarget?.credential?.configured
                ? t('instanceEdit.modelsKeyConfigured')
                : t('instanceEdit.modelsApiKeyHint'))
          "
        >
          <dsh-input-password
            v-model="providerForm.apiKey"
            :placeholder="providerEditTarget?.credential?.configured ? t('instanceEdit.modelsApiKeyKeep') : t('instanceEdit.modelsApiKeyPlaceholder')"
          />
        </dsh-form-item>

        <!-- Official DeepSeek card: only the key above is shown -->
        <template v-if="providerForm.official">
          <p class="model-form-note">{{ t('instanceEdit.modelsDeepSeekFormNote') }}</p>
        </template>

        <!-- Built-in provider: id/endpoint/models come from the catalogue -->
        <template v-else-if="providerForm.catalogProviderId">
          <dsh-form-item :label="t('instanceEdit.modelsProviderId')">
            <dsh-input :model-value="providerForm.id" disabled />
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.modelsDisplayName')">
            <dsh-input v-model="providerForm.displayName" :placeholder="catalogEntry?.name ?? ''" />
          </dsh-form-item>
          <dsh-alert type="info">
            {{ t('instanceEdit.modelsCatalogInherited', { api: providerForm.api, baseUrl: providerForm.baseUrl }) }}
          </dsh-alert>
        </template>

        <!-- Custom provider: every field is user-owned -->
        <template v-else>
          <dsh-form-item
            :label="t('instanceEdit.modelsProviderId')"
            required
            :validate-status="providerIdError ? 'error' : undefined"
            :help="providerIdError || t('instanceEdit.modelsProviderIdHint')"
          >
            <dsh-input v-model="providerForm.id" :disabled="!!providerOriginalId" placeholder="my-gateway" />
          </dsh-form-item>
          <dsh-form-item :label="t('instanceEdit.modelsDisplayName')">
            <dsh-input v-model="providerForm.displayName" :placeholder="providerForm.id" />
          </dsh-form-item>
          <dsh-form-item
            :label="t('instanceEdit.modelsBaseUrl')"
            required
            :validate-status="providerBaseUrlError ? 'error' : undefined"
            :help="providerBaseUrlError || undefined"
          >
            <dsh-input v-model="providerForm.baseUrl" placeholder="https://gateway.example/v1" />
          </dsh-form-item>
          <dsh-form-item
            :label="t('instanceEdit.modelsApi')"
            required
            :validate-status="providerApiError ? 'error' : undefined"
            :help="providerApiError || undefined"
          >
            <dsh-select
              v-model="providerForm.api"
              :placeholder="t('instanceEdit.modelsErrApiRequired')"
              style="width: 320px"
            >
              <dsh-option v-for="opt in PROVIDER_API_OPTIONS" :key="opt" :value="opt">{{ opt }}</dsh-option>
            </dsh-select>
          </dsh-form-item>

          <dsh-form-item
            :label="t('instanceEdit.modelsModels')"
            :validate-status="providerModelsError ? 'error' : undefined"
            :help="providerModelsError || t('instanceEdit.modelsModelsHint')"
          >
            <div class="model-rows">
              <div v-for="model in providerForm.models" :key="model.id" class="model-row">
                <span class="model-id">{{ model.id }}</span>
                <dsh-input v-model="model.name" :placeholder="model.id" class="model-name" />
                <dsh-input-number
                  :model-value="model.contextWindow ?? undefined"
                  :placeholder="t('instanceEdit.modelsContextWindow')"
                  hide-button
                  class="model-num"
                  @update:model-value="(v) => (model.contextWindow = typeof v === 'number' && v > 0 ? v : null)"
                />
                <dsh-input-number
                  :model-value="model.maxTokens ?? undefined"
                  :placeholder="t('instanceEdit.modelsMaxTokens')"
                  hide-button
                  class="model-num"
                  @update:model-value="(v) => (model.maxTokens = typeof v === 'number' && v > 0 ? v : null)"
                />
                <dsh-button variant="danger-soft" @click="removeModel(model.id)">
                  {{ t('instances.table.delete') }}
                </dsh-button>
              </div>
              <!-- Manual entry row: works exactly like the picker (issue #89 #3) -->
              <div class="model-row model-row-new">
                <dsh-input
                  v-model="newModel.id"
                  :placeholder="t('instanceEdit.modelsModelId')"
                  class="model-id-input"
                />
                <dsh-input v-model="newModel.name" :placeholder="t('instanceEdit.modelsDisplayName')" class="model-name" />
                <dsh-input-number
                  v-model="newModel.contextWindow"
                  :placeholder="t('instanceEdit.modelsContextWindow')"
                  hide-button
                  class="model-num"
                />
                <dsh-input-number
                  v-model="newModel.maxTokens"
                  :placeholder="t('instanceEdit.modelsMaxTokens')"
                  hide-button
                  class="model-num"
                />
                <dsh-button variant="ghost"
                  size="sm"
                  :disabled="!newModel.id.trim() || !!newModelError"
                  @click="addManualModel"
                >
                  {{ t('instanceEdit.modelsAddModel') }}
                </dsh-button>
              </div>
              <p v-if="newModelError" class="model-row-error">{{ newModelError }}</p>
              <dsh-button variant="secondary" size="sm" class="model-fetch-btn" :loading="modelsPickerLoading" @click="openModelPicker">
                {{ t('instanceEdit.modelsFetch') }}
              </dsh-button>
            </div>
          </dsh-form-item>
        </template>

      </dsh-form>
    </dsh-modal>

    <!-- Add provider: choose built-in catalogue or custom (issue #89) -->
    <dsh-modal
      v-model:visible="addModeVisible"
      modalClass="model-provider-modal"
      :title="t('instanceEdit.modelsAddTitle')"
      :footer="false"
      :width="560"
    >
      <dsh-spin :loading="catalogLoading" class="add-mode-content">
        <div class="add-mode-grid">
          <button class="add-mode-card" type="button" @click="openAddCustom">
            <span class="add-mode-title">{{ t('instanceEdit.modelsAddCustom') }}</span>
            <span class="add-mode-sub">{{ t('instanceEdit.modelsAddCustomSub') }}</span>
          </button>
          <button
            v-for="p in catalogProviders"
            :key="p.id"
            class="add-mode-card"
            type="button"
            :disabled="providerRoutes.some((r) => r.id === p.id)"
            @click="onPickCatalog(p)"
          >
            <span class="add-mode-title">{{ p.name }}</span>
            <span class="add-mode-sub" :title="p.api">{{ p.api }}</span>
            <span class="add-mode-sub" :title="p.baseUrl">{{ p.baseUrl }}</span>
          </button>
        </div>
        <!-- A failed catalogue must say so, not masquerade as an empty one. -->
        <dsh-alert v-if="catalogError" type="error">
          {{ t('instanceEdit.modelsCatalogError', { error: catalogError }) }}
        </dsh-alert>
        <dsh-empty
          v-else-if="!catalogLoading && catalogProviders.length === 0"
          :description="t('instanceEdit.modelsCatalogEmpty')"
        />
      </dsh-spin>
    </dsh-modal>

    <ModelPickerDialog
      :visible="modelsPickerVisible"
      :loading="modelsPickerLoading"
      :models="pickerModels"
      :selected="pickerSelected"
      @update:visible="(v: boolean) => (modelsPickerVisible = v)"
      @adopt="onAdoptModels"
    />

    <!-- Storage redirection target picker (issue #51; directory + alias form
         since issue #91) -->
    <dsh-modal
      :visible="linkDialogVisible"
      :title="t('instanceEdit.storageSetTitle', { entry: linkEntry })"
      :ok-text="t('common.confirm')"
      :cancel-text="t('instanceEdit.cancel')"
      :ok-button-props="{ disabled: !linkDir.trim() || !!linkAliasError, loading: linkBusy || linkPicking }"
      width="620px"
      @ok="confirmSetLink"
      @cancel="linkDialogVisible = false"
    >
      <dsh-form layout="vertical" :model="{}">
        <dsh-form-item :label="t('instanceEdit.storagePreset')">
          <dsh-select
            :model-value="linkPresetId"
            :placeholder="t('instanceEdit.storagePresetNone')"
            :loading="linkPresetsLoading"
            allow-clear
            style="width: 100%"
            @change="applyLinkPresetSingle"
          >
            <dsh-option v-for="preset in linkPresetOptions" :key="preset.id" :value="preset.id">
              {{ preset.label }}
              <span class="storage-preset-path">
                {{ preset.path }}
                <!-- A missing root is the normal first-run state: the backend
                     creates it on submit, so it is not "unavailable" (F2). -->
                <template v-if="!preset.exists">
                  · {{ t('instanceEdit.storagePresetWillCreate') }}
                </template>
              </span>
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-alert v-if="!linkIsDir" type="info" class="storage-file-hint">
          {{ t('instanceEdit.storageFileEntryHint') }}
        </dsh-alert>
        <dsh-form-item :label="t('instanceEdit.storageTargetDir')">
          <dsh-input-group>
            <dsh-input
              v-model="linkDir"
              :placeholder="t('instanceEdit.storageTargetPlaceholder')"
              allow-clear
            />
            <dsh-button variant="secondary" :loading="linkPicking" @click="pickLinkDir('single')">
              {{ t('instanceEdit.storageBrowse') }}
            </dsh-button>
          </dsh-input-group>
        </dsh-form-item>
        <dsh-form-item
          :label="t('instanceEdit.storageAlias')"
          :validate-status="linkAliasError ? 'error' : undefined"
          :help="linkAliasError || undefined"
        >
          <dsh-input v-model="linkAlias" :placeholder="linkEntry" allow-clear />
        </dsh-form-item>
        <dsh-alert type="info">
          {{ t('instanceEdit.storageSetHint') }}
        </dsh-alert>
      </dsh-form>
    </dsh-modal>

    <!-- Batch storage redirection (issue #91): one directory for every
         selected entry; each entry keeps its current name. -->
    <dsh-modal
      :visible="batchDialogVisible"
      :title="t('instanceEdit.storageBatchTitle', { count: storageSelected.length })"
      :ok-text="t('common.confirm')"
      :cancel-text="t('instanceEdit.cancel')"
      :ok-button-props="{ disabled: !batchDir.trim(), loading: linkBusy || linkPicking }"
      width="620px"
      @ok="confirmBatchLink"
      @cancel="batchDialogVisible = false"
    >
      <dsh-form layout="vertical" :model="{}">
        <dsh-form-item :label="t('instanceEdit.storagePreset')">
          <dsh-select
            :model-value="batchPresetId"
            :placeholder="t('instanceEdit.storagePresetNone')"
            :loading="linkPresetsLoading"
            allow-clear
            style="width: 100%"
            @change="applyLinkPresetBatch"
          >
            <dsh-option v-for="preset in linkPresetOptions" :key="preset.id" :value="preset.id">
              {{ preset.label }}
              <span class="storage-preset-path">
                {{ preset.path }}
                <template v-if="!preset.exists">
                  · {{ t('instanceEdit.storagePresetWillCreate') }}
                </template>
              </span>
            </dsh-option>
          </dsh-select>
        </dsh-form-item>
        <dsh-form-item :label="t('instanceEdit.storageTargetDir')">
          <dsh-input-group>
            <dsh-input
              v-model="batchDir"
              :placeholder="t('instanceEdit.storageTargetPlaceholder')"
              allow-clear
            />
            <dsh-button variant="secondary" :loading="linkPicking" @click="pickLinkDir('batch')">
              {{ t('instanceEdit.storageBrowse') }}
            </dsh-button>
          </dsh-input-group>
        </dsh-form-item>
        <dsh-alert type="info">
          {{ t('instanceEdit.storageBatchHint') }}
        </dsh-alert>
      </dsh-form>
    </dsh-modal>

  </div>
</template>
<style lang="scss" scoped>
.skill-toolbar {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 12px;
}

.skill-repo-select {
  flex: 1;
  min-width: 0;
}

.skill-repo-ref {
  font-size: 12px;
  color: var(--color-text-3);
  margin-left: 6px;
}

.mcp-toolbar {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
}

.mcp-path {
  margin: 0 0 12px;
  font-size: 12px;
  color: var(--color-text-3);
  word-break: break-all;
}

/* Issue #93: keep the create/edit dialog compact; long header/arg/env row
   lists scroll inside the body instead of stretching the modal. */
.mcp-dialog-body {
  max-height: 56vh;
  overflow-y: auto;
}

.mcp-target {
  font-family: monospace;
  font-size: 13px;
}

.mcp-rows {
  width: 100%;
}

.switch-label {
  margin-left: 10px;
  color: var(--color-text-2);
}

.icon-editor {
  display: flex;
  gap: 16px;
  align-items: flex-start;
}

.icon-preview {
  width: 64px;
  height: 64px;
  border-radius: 12px;
  object-fit: cover;
  flex-shrink: 0;
  border: 1px solid var(--color-border-2);
}

.icon-actions {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.edit-page {
  display: flex;
  height: var(--dl-content-height);
}

.edit-sidebar {
  width: 200px;
  flex-shrink: 0;
}

.edit-content {
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.edit-inner {
  padding: 20px 20px 60px 0;
}

.edit-card {
  // Full-width card: stretch to fill the content area like the download page.
  width: 100%;
  box-sizing: border-box;
}

.edit-form {
  width: 100%;
}

.profile-item {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 10px 12px;
  border: 1px solid var(--color-border-2);
  border-radius: 20px;
  margin-bottom: 8px;
  background: var(--color-fill-1);
}

.profile-item-name {
  flex: 1;
  min-width: 0;
  font-size: 14px;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.profile-item-actions {
  display: inline-flex;
  gap: 8px;
  align-items: center;
  flex-shrink: 0;
}

.profiles-empty {
  padding: 8px 0;
}

.profile-add-btn {
  margin-top: 4px;
}

.storage-caveat {
  margin-bottom: 12px;
}

.storage-toolbar {
  display: flex;
  justify-content: flex-end;
  margin-bottom: 8px;
}

/* Issue #92: content-sized table columns. The tables above use auto layout
   (no scroll.x, no ellipsis columns), so nowrap cells size to their content
   instead of wrapping, and this wrapper scrolls horizontally when a table
   outgrows the card. */
.table-scroll {
  overflow-x: auto;
}

:deep(.cell-nowrap) {
  white-space: nowrap;
}

/* Issue #96: pin the trailing action column with sticky cells riding the
   wrapper's horizontal scroll, so the buttons never scroll away. Arco's
   native fixed columns would force table-layout: fixed and undo the
   content-sized columns above. Colors mirror Arco's own: header cells use
   --color-neutral-2, body cells sit on --color-bg-2. */
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

/* Arco's row hover recolors every td with the translucent --color-fill-1;
   on the pinned cell that lets the content scrolled underneath show
   through. Mirror Arco's native fixed-column treatment instead: keep the
   opaque base and paint the hover tint as a ::before overlay (its negative
   z-index sits above the cell background, below the buttons). */
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

/* Manual single-line clamp: the built-in column ellipsis would force Arco's
   fixed table layout and defeat the content-sized columns above. */
.cell-ellipsis {
  display: inline-block;
  max-width: 420px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
}

.storage-default {
  color: var(--color-text-3);
}

/* Preset option: label first, then the concrete root path in a muted aside. */
.storage-preset-path {
  margin-left: 8px;
  color: var(--color-text-3);
  font-size: 12px;
}

/* File entries get guidance instead of the (root-based) preset dropdown. */
.storage-file-hint {
  margin-bottom: 16px;
}

.env-title {
  margin: 0 0 4px;
  font-size: 15px;
}

.profiles-toolbar {
  display: flex;
  gap: 8px;
  margin: 8px 0 12px;
}

.env-row {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
  margin-bottom: 10px;
}

.env-key {
  width: 240px;
  font-family: monospace;
}

.env-value {
  flex: 1;
  min-width: 220px;
}

.env-error {
  width: 100%;
  color: rgb(var(--red-6));
  font-size: 12px;
}

.env-add-btn {
  margin-top: 4px;
}

.plugins-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.plugins-table {
  position: relative;
  margin-bottom: 12px;

  &[aria-busy='true'] :deep(.arco-table-element > tbody) {
    filter: blur(6px);
    clip-path: inset(0 round calc(var(--radius) * 2));
  }
}

.plugins-table-spinner {
  position: absolute;
  inset: 0;
  z-index: 3;
  display: grid;
  place-items: center;
}

.plugin-cell-name {
  // Let the ID determine the column width, even when the localized name is longer.
  display: block;
  width: 0;
  min-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.plugin-cell-id {
  font-family: monospace;
  font-size: 13px;
  white-space: nowrap;
}

.plugin-cell-id-secondary {
  display: block;
  color: var(--color-text-3);
  font-size: 12px;
  line-height: 16px;
}

.plugin-no-version {
  color: var(--color-text-4);
}

.plugin-update-available {
  color: rgb(var(--orange-6));
  font-weight: 600;
}

.plugins-batch {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}

.footer-actions {
  margin-top: 20px;
  display: flex;
  gap: 12px;
  justify-content: center;
}

.terminal-row {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
}

.terminal-embed {
  height: 480px;
}

.terminal-hint {
  margin: 4px 0 12px;
  color: var(--color-text-3);
  font-size: 12px;
}

.terminal-alert {
  margin-top: 8px;
  max-width: 640px;
}

@media (max-width: 720px) {
  .edit-page {
    flex-direction: column;
  }

  .edit-sidebar {
    width: 100%;
    height: auto;

    :deep(.arco-menu) {
      height: auto;
      display: flex;
      overflow-x: auto;
    }

    :deep(.arco-menu-item) {
      white-space: nowrap;
    }
  }
}

/* AGENTS.md editor / preview (issue #57) */
.agents-head {
  .env-title {
    margin: 0;
  }
}

// Save sits on the toolbar row, pushed to the right edge, level with 编辑/刷新.
.agents-save {
  margin-left: auto;
}

.agents-editor {
  width: 100%;
  font-family: Consolas, 'Courier New', monospace;
}

.agents-preview {
  padding: 12px 16px;
  border: 1px solid var(--color-border-2);
  border-radius: 20px;
  font-size: 14px;
  line-height: 1.7;
  color: var(--color-text-1);
  word-wrap: break-word;

  :deep(h1),
  :deep(h2),
  :deep(h3) {
    margin: 16px 0 8px;
    line-height: 1.35;
  }

  :deep(h2) {
    padding-bottom: 6px;
    border-bottom: 1px solid var(--color-border-2);
  }

  :deep(p) {
    margin: 8px 0;
  }

  :deep(a) {
    color: rgb(var(--primary-6));
  }

  :deep(code) {
    font-family: Consolas, 'Courier New', monospace;
    font-size: 0.9em;
    background: var(--color-fill-2);
    padding: 1px 5px;
    border-radius: 3px;
  }

  :deep(pre) {
    background: var(--color-fill-2);
    padding: 10px 12px;
    border-radius: 4px;
    overflow-x: auto;

    code {
      background: none;
      padding: 0;
    }
  }

  :deep(blockquote) {
    margin: 8px 0;
    padding: 4px 12px;
    border-left: 3px solid var(--color-border-3);
    color: var(--color-text-2);
  }

  .agents-empty {
    color: var(--color-text-3);
  }
}

// --- Models tab (issue #89) ---------------------------------------------------

.models-toolbar {
  display: flex;
  gap: 12px;
  align-items: center;
  margin-bottom: 8px;
}

.models-path {
  font-family: var(--font-family-code, monospace);
  font-size: 12px;
  color: var(--color-text-3);
  margin: 0 0 16px;
  word-break: break-all;
}

.models-body {
  width: 100%;
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-content: flex-start;
}

.model-card {
  width: 280px;
  border: 1px solid var(--color-border-2);
  border-radius: 20px;
  padding: 14px;
  background: var(--color-bg-2);
}

.model-card-official {
  border-color: var(--color-primary-light-3);
}

.model-card-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.model-card-id {
  font-weight: 600;
  font-size: 15px;
}

.model-card-sub {
  margin: 8px 0 4px;
  color: var(--color-text-3);
  font-size: 12px;
  word-break: break-all;
}

.model-card-models {
  margin: 0 0 10px;
  color: var(--color-text-2);
  font-size: 12px;
}

.model-card-actions {
  display: flex;
  gap: 8px;
}

.model-form-note {
  color: var(--color-text-3);
  font-size: 13px;
}

.model-rows {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.model-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.model-id {
  min-width: 0;
  max-width: 40%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-family-code, monospace);
  font-size: 12px;
  color: var(--color-text-2);
}

.model-id-input {
  flex: 1;
}

.model-name {
  flex: 1;
}

.model-num {
  width: 120px;
  flex-shrink: 0;
}

.model-row-error {
  margin: 0;
  font-size: 12px;
  color: rgb(var(--danger-6));
}

.model-fetch-btn {
  align-self: flex-start;
}

:global(.model-provider-modal .dsh-overlay-scroll > .arco-scrollbar-container) {
  margin-inline-end: 22px;
}

.add-mode-content {
  display: block;
}

.add-mode-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

.add-mode-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  text-align: left;
  padding: 14px;
  border: 1px solid var(--color-border-2);
  border-radius: 8px;
  background: var(--color-bg-2);
  /* Native buttons take the OS color scheme (CanvasText), which ignores
     the app theme: set the themed text color explicitly. */
  color: var(--color-text-1);
  cursor: pointer;

  &:hover:not(:disabled) {
    border-color: var(--color-primary-light-3);
  }

  &:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
}

.add-mode-title {
  font-weight: 600;
}

.add-mode-sub {
  align-self: stretch;
  overflow: hidden;
  font-size: 12px;
  color: var(--color-text-3);
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
