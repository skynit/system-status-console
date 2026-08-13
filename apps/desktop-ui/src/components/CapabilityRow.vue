<script setup lang="ts">
import { computed } from 'vue'
import { Check, CircleAlert, CircleOff, Unplug } from 'lucide-vue-next'
import type { RouteLocationRaw } from 'vue-router'

import type { BackendStatus } from '../types'

const props = defineProps<{
  label: string
  detail: string
  status: BackendStatus
  reason: string
  to?: RouteLocationRaw
}>()

const statusIcon = computed(() => {
  switch (props.status) {
    case 'healthy':
      return Check
    case 'degraded':
      return CircleAlert
    case 'unreachable':
      return Unplug
    default:
      return CircleOff
  }
})
</script>

<template>
  <component
    :is="to ? 'RouterLink' : 'div'"
    :to="to"
    :aria-label="to ? `${label}：${status}，${reason}；进入详情` : undefined"
    :class="['capability-row', { 'is-link': to }, `is-${status}`]"
  >
    <component :is="statusIcon" class="status-icon" :size="17" aria-hidden="true" />
    <div class="capability-row-copy">
      <strong>{{ label }}</strong>
      <span>{{ detail }}</span>
    </div>
    <span class="state-pill">{{ status }}</span>
    <code class="capability-reason" :title="reason">{{ reason }}</code>
  </component>
</template>
