<script setup lang="ts">
import { computed } from 'vue'
import MarketIcon from '@/components/MarketIcon.vue'
import { useLauncherStore } from '@/stores/launcher'

const props = withDefaults(defineProps<{
  version: string
  sourceBuild?: boolean
  highlightPrerelease?: boolean
}>(), { highlightPrerelease: true })

const store = useLauncherStore()
const src = computed(() => props.sourceBuild || /-alpha(?:[.\d-]|$)/i.test(props.version)
  ? store.versionIcons.chat
  : store.versionIcons.platform,
)
</script>

<template>
  <MarketIcon :src="src" class="version-icon" :class="{ pre: highlightPrerelease && version.includes('-') }">
    ◆
  </MarketIcon>
</template>

<style scoped>
.version-icon {
  color: #fff;
  font-size: 16px;
}

.version-icon.pre {
  --market-icon-fallback: linear-gradient(135deg, #f7ba1e, #f53f3f);
}
</style>
