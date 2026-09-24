<script setup lang="ts">
import type { CompatibilityReport } from '@/api/types'
import { useI18n } from 'vue-i18n'

defineProps<{ report: CompatibilityReport }>()
const { t } = useI18n()
</script>

<template>
  <section class="compat-report" aria-live="polite">
    <strong>{{ t('compat.title') }}: {{ t(`compat.${report.status}`) }}</strong>
    <span>{{ report.profile }} / {{ report.version }} / {{ report.runtime }} / {{ report.checked_at }}</span>
    <span>{{ report.started ? t('compat.spawned') : report.handoff_pending ? t('compat.handoff') : t('compat.notStarted') }}</span>
    <div v-if="report.initial_findings.length">
      <strong>{{ t('compat.before') }}</strong>
      <p v-for="(f, i) in report.initial_findings" :key="`before-${i}`">{{ f.severity }} / {{ f.code }}: {{ f.packages.join(', ') }} {{ f.entries.join(', ') }}. {{ f.evidence }}</p>
    </div>
    <div v-if="report.actions.length">
      <strong>{{ t('compat.actions') }}</strong>
      <p v-for="a in report.actions" :key="a.entry">{{ a.package }} ({{ a.entry }}): {{ a.reason }}. {{ t(`compat.action.${a.status}`) }} {{ a.status === 'applied' ? t('compat.reenable') : '' }}</p>
    </div>
    <div v-if="report.findings.length">
      <strong>{{ t('compat.after') }}</strong>
      <p v-for="(f, i) in report.findings" :key="`${f.code}-${i}`">
        <a-tag :color="f.severity === 'confirmed' ? 'red' : f.severity === 'unknown' ? 'orange' : 'blue'">{{ t(`compat.${f.severity}`) }}</a-tag>
        {{ f.code }}: {{ f.packages.join(', ') }} {{ f.entries.join(', ') }}. {{ f.evidence }}. {{ f.action }}
      </p>
    </div>
    <strong v-if="report.unresolved.length">{{ t('compat.unresolved') }}: {{ report.unresolved.length }}</strong>
    <span v-else-if="!report.findings.length">{{ t('compat.clean') }}</span>
  </section>
</template>

<style scoped>
.compat-report { display: flex; flex-direction: column; gap: 8px; padding: 12px 0; font-size: 12px; overflow-wrap: anywhere; }
.compat-report p { margin: 6px 0; line-height: 1.5; }
</style>
