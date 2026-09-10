<script setup lang="ts">
import { IconInfoCircleFill } from '@arco-design/web-vue/es/icon'

/** Small info icon that reveals a hint in a tooltip; used after card titles
 * and option labels instead of inline hint paragraphs. */
defineProps<{ content: string }>()
</script>

<template>
  <a-tooltip :content="content" position="top" mini>
    <IconInfoCircleFill class="hint-icon" />
  </a-tooltip>
</template>

<style scoped>
.hint-icon {
  font-size: 13px;
  color: var(--color-text-3);
  cursor: help;
  flex-shrink: 0;
  margin-left: 4px;
  vertical-align: -2px;
}

.hint-icon:hover {
  color: var(--color-text-2);
}
</style>
