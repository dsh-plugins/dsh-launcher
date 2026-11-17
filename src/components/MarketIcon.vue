<script setup lang="ts">
import { computed, ref } from 'vue'

const props = defineProps<{ src?: string | null }>()
const emit = defineEmits<{ error: [src: string] }>()
const loadedSrc = ref<string | null>(null)
const loaded = computed(() => !!props.src && loadedSrc.value === props.src)

function onError(src: string) {
  loadedSrc.value = null
  emit('error', src)
}
</script>

<template>
  <span class="market-icon" :class="{ 'has-image': loaded }" aria-hidden="true">
    <slot v-if="!loaded" />
    <img
      v-if="src"
      :key="src"
      :src="src"
      alt=""
      loading="lazy"
      decoding="async"
      referrerpolicy="no-referrer"
      @load="loadedSrc = src"
      @error="onError(src)"
    />
  </span>
</template>

<style scoped>
.market-icon {
  position: relative;
  display: grid;
  place-items: center;
  width: 40px;
  height: 40px;
  flex-shrink: 0;
  border-radius: 8px;
  background: var(--market-icon-fallback, linear-gradient(135deg, #0fc6c2, #165dff));
}

.market-icon.has-image {
  background: #fff;
}

.market-icon img {
  position: absolute;
  inset: 4px;
  width: 32px;
  height: 32px;
  object-fit: contain;
  opacity: 0;
}

.market-icon.has-image img {
  opacity: 1;
}
</style>
