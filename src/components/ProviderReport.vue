<script setup lang="ts">
// Inline render of the provider pre-launch self-check (issue #83), shown inside
// the early-loading window next to the compatibility report. The shape mirrors
// the manual trigger in InstanceEdit.vue; the messages are already localized by
// the backend (provider_validator), so we only need the route status for coloring.
import type { ValidationReport, ValidationStatus } from '@/api/types'
import { useI18n } from 'vue-i18n'

defineProps<{ report: ValidationReport }>()
const { t } = useI18n()

function tagColor(status: ValidationStatus): string {
  if (status === 'ok') return 'green'
  if (status === 'warning') return 'orange'
  if (status === 'error') return 'red'
  return 'gray'
}
</script>

<template>
  <section class="provider-report" aria-live="polite">
    <strong class="provider-report-title">{{ t('earlyLoading.providerCheck') }}</strong>
    <a-alert
      v-if="report.hasBlockingErrors"
      type="warning"
      class="provider-report-alert"
    >{{ t('earlyLoading.providerCheckBlocking') }}</a-alert>
    <div
      v-for="r in report.routes"
      :key="r.name"
      class="provider-report-row"
    >
      <div class="provider-report-head">
        <strong>{{ r.name }}</strong>
        <a-tag :color="tagColor(r.status)" size="small">
          {{ t(`instanceEdit.providerStatus_${r.status}`) }}
        </a-tag>
      </div>
      <ul v-if="r.messages.length" class="provider-report-msgs">
        <li v-for="(m, i) in r.messages" :key="i">{{ m }}</li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.provider-report {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 8px 0;
  font-size: 12px;
  overflow-wrap: anywhere;
}
.provider-report-title {
  font-size: 13px;
}
.provider-report-alert {
  margin-bottom: 4px;
}
.provider-report-row {
  border-top: 1px solid var(--color-border-2);
  padding-top: 6px;
}
.provider-report-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.provider-report-msgs {
  margin: 6px 0 0;
  padding-left: 18px;
  line-height: 1.5;
  color: var(--color-text-3);
}
.provider-report-msgs li {
  margin: 2px 0;
}
</style>
