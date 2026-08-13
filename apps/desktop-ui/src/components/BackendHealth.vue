<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { Check, CircleAlert, CircleOff, RefreshCw, Unplug } from 'lucide-vue-next'

import { getBackendHealth } from '../backend'
import type { BackendHealth as BackendHealthState, BackendStatus } from '../types'

const health = ref<BackendHealthState | null>(null)
const loading = ref(false)
const HEALTH_REFRESH_INTERVAL_MS = 10_000
let active = true
let requestGeneration = 0
let refreshTimer: number | null = null

const statusLabel: Record<BackendStatus, string> = {
  healthy: 'healthy',
  degraded: 'degraded',
  unsupported: 'unsupported',
  unreachable: 'unreachable',
}

const statusIcon = computed(() => {
  switch (health.value?.status) {
    case 'healthy':
      return Check
    case 'degraded':
      return CircleAlert
    case 'unsupported':
      return CircleOff
    case 'unreachable':
      return Unplug
    default:
      return RefreshCw
  }
})

async function refresh() {
  if (loading.value) return
  const generation = ++requestGeneration
  loading.value = true
  const result = await getBackendHealth()
  if (!active || generation !== requestGeneration) return
  health.value = result
  loading.value = false
}

onMounted(() => {
  void refresh()
  refreshTimer = window.setInterval(() => void refresh(), HEALTH_REFRESH_INTERVAL_MS)
})

onBeforeUnmount(() => {
  active = false
  requestGeneration += 1
  if (refreshTimer !== null) window.clearInterval(refreshTimer)
})

defineExpose({ health, refresh })
</script>

<template>
  <div class="backend-health" :class="health ? `is-${health.status}` : 'is-checking'" aria-live="polite">
    <component :is="statusIcon" :class="['status-icon', { 'is-spinning': loading }]" :size="16" aria-hidden="true" />
    <div class="health-copy">
      <span class="health-label">桌面桥接</span>
      <strong v-if="health" class="status-token">{{ statusLabel[health.status] }}</strong>
      <strong v-else class="status-token">checking</strong>
      <span v-if="health" class="capability-reason" :title="health.capabilityReason">{{ health.capabilityReason }}</span>
    </div>
    <button class="icon-button compact-button" type="button" aria-label="重新检查桌面桥接" title="重新检查" :disabled="loading" @click="refresh">
      <RefreshCw :size="15" aria-hidden="true" />
    </button>
  </div>
</template>
