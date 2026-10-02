<script setup lang="ts">
// Inline render of the provider pre-launch self-check (issue #83), shown inside
// the early-loading window next to the compatibility report. The data shape is
// the `ProviderRouteReport[]` returned by `providers::check_provider_routes` —
// the same engine and i18n keys (`instanceEdit.providerChecks.<code>`) the
// manual trigger in InstanceEdit.vue uses, so both views always agree.
import type { ProviderRouteReport } from '@/api/types'
import { useI18n } from 'vue-i18n'

defineProps<{ report: ProviderRouteReport[] }>()
const { t } = useI18n()

function tagColor(status: string): string {
  if (status === 'ok') return 'green'
  if (status === 'warn') return 'orange'
  return 'gray'
}

function statusKey(status: string): string {
  if (status === 'warn') return 'instanceEdit.providerCheckStatusWarn'
  if (status === 'unknown') return 'instanceEdit.providerCheckStatusUnknown'
  return 'instanceEdit.providerCheckStatusOk'
}
</script>

<template>
  <section class="provider-report" aria-live="polite">
    <strong class="provider-report-title">{{ t('earlyLoading.providerCheck') }}</strong>
    <div
      v-for="r in report"
      :key="r.route"
      class="provider-report-row"
    >
      <div class="provider-report-head">
        <strong>{{ r.route }}</strong>
        <a-tag :color="tagColor(r.status)" size="small">
          {{ t(statusKey(r.status)) }}
        </a-tag>
      </div>
      <ul v-if="r.checks.length" class="provider-report-msgs">
        <li v-for="(c, i) in r.checks" :key="i">
          {{ t(`instanceEdit.providerChecks.${c.code}`, c.params) }}
        </li>
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
