<script setup lang="ts">
import { CircleOff } from 'lucide-vue-next'

defineProps<{
  reason: string
  title?: string
}>()
</script>

<template>
  <div class="capability-notice" role="status">
    <CircleOff :size="18" aria-hidden="true" />
    <div>
      <strong>{{ title ?? '当前能力未实现' }}</strong>
      <p><span class="status-token">unsupported</span> · capability reason: <code>{{ reason }}</code></p>
    </div>
  </div>
</template>
