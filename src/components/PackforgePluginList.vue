<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ModpackManifest } from '@/api/types'
import MarketIcon from '@/components/MarketIcon.vue'
import { packforgeIcon, packforgeText } from '@/utils/packforge'

const props = withDefaults(defineProps<{ manifest: ModpackManifest; icons?: Record<string, string> }>(), {
  icons: () => ({}),
})
const { t, locale } = useI18n()

const profiles = computed(() => {
  const units = props.manifest.type === 'dshhome'
    ? Object.entries(props.manifest.profiles ?? {})
    : [[props.manifest.profileName || props.manifest.name, props.manifest] as const]
  return units.map(([name, unit]) => ({
    name,
    plugins: [...new Set([...(unit.bundles ?? []), ...Object.keys(unit.dependencies ?? {})])].map(id => {
      const plugin = unit.plugins?.[id]
      return {
        id,
        version: unit.dependencies?.[id],
        name: packforgeText(plugin?.name, locale.value) || id,
        description: packforgeText(plugin?.desc, locale.value),
        registry: plugin?.source?.registry,
        icon: packforgeIcon(plugin?.icon, props.icons),
      }
    }),
  })).filter(profile => profile.plugins.length)
})
</script>

<template>
  <section v-if="profiles.length" class="packforge-plugins">
    <h3>{{ t('modpack.plugins') }}</h3>
    <details v-for="profile in profiles" :key="profile.name" :open="profiles.length === 1">
      <summary>{{ profile.name }} ({{ profile.plugins.length }})</summary>
      <ul>
        <li v-for="plugin in profile.plugins" :key="plugin.id">
          <MarketIcon v-if="plugin.icon" :src="plugin.icon" class="plugin-icon">🧩</MarketIcon>
          <div>
            <strong>{{ plugin.name }}</strong>
            <span v-if="plugin.version"> · {{ plugin.version }}</span>
            <p v-if="plugin.name !== plugin.id" class="plugin-id">{{ plugin.id }}</p>
            <p v-if="plugin.description">{{ plugin.description }}</p>
            <p v-if="plugin.registry">{{ t('modpack.registry') }} · {{ plugin.registry }}</p>
          </div>
        </li>
      </ul>
    </details>
  </section>
</template>

<style scoped>
.packforge-plugins { margin: 14px 0; }
details { margin: 6px 0; }
summary { cursor: pointer; }
ul { list-style: none; padding: 0; margin: 4px 0; }
li { display: flex; gap: 10px; padding: 8px 0; border-bottom: 1px solid var(--color-border-1); overflow-wrap: anywhere; }
.plugin-icon { width: 32px; height: 32px; }
.plugin-icon.has-image { background: transparent; }
.plugin-icon :deep(img) { inset: 0; }
li p { margin: 2px 0 0; }
.plugin-id { color: var(--color-text-3); }
</style>
