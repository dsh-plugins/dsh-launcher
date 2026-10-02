<script setup lang="ts">
// Inline render of the provider pre-launch self-check (issue #83), shown inside
// the early-loading window next to the compatibility report. The data shape is
// the `ProviderRouteReport[]` returned by `providers::check_provider_routes` —
// the same engine and i18n keys (`instanceEdit.providerChecks.<code>`) the
// manual trigger in InstanceEdit.vue uses, so both views always agree.
import type { ProviderRouteReport } from '@/api/types'
import { useI18n } from 'vue-i18n'
import {
  providerCheckStatusKeySuffix,
  providerCheckTagColor,
  translateProviderCheck,
} from '@/utils/provider-check'

defineProps<{ report: ProviderRouteReport[] }>()
const { t, te } = useI18n()
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
        <a-tag :color="providerCheckTagColor(r.status)" size="small">
          {{ t(`instanceEdit.providerCheckStatus${providerCheckStatusKeySuffix(r.status)}`) }}
        </a-tag>
      </div>
      <ul v-if="r.checks.length" class="provider-report-msgs">
        <li v-for="(c, i) in r.checks" :key="i">
          {{ translateProviderCheck(t, te, c.code, c.params) }}
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
