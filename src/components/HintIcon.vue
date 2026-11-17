<script setup lang="ts">
import { IconInfoCircleFill } from '@dsh-plugin/dsh-ui-components-vue/icons'

/** Small info icon that reveals a hint in a tooltip; used after card titles
 * and option labels instead of inline hint paragraphs. */
defineProps<{ content: string }>()
</script>

<template>
  <dsh-tooltip :content="content" position="top" mini>
    <IconInfoCircleFill class="hint-icon" />
  </dsh-tooltip>
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
