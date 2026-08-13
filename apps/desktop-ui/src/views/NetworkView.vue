<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  AppWindow,
  CheckCircle2,
  CircleAlert,
  CircleHelp,
  CircleOff,
  Gauge,
  Hourglass,
  LoaderCircle,
  Network,
  RefreshCw,
  Unplug,
} from 'lucide-vue-next'

import { getNetworkSnapshot } from '../backend'
import type {
  BackendStatus,
  BridgeError,
  NetworkCapabilityState,
  NetworkRate,
  NetworkSnapshot,
} from '../types'

type NetworkTab = 'interfaces' | 'applications'
type ViewState = 'loading' | 'snapshot' | 'error'

const activeTab = ref<NetworkTab>(initialNetworkTabFromHash())
const state = ref<ViewState>('loading')
const snapshot = ref<NetworkSnapshot | null>(null)
const error = ref<BridgeError | null>(null)
const refreshing = ref(false)
let active = true
let requestGeneration = 0
let refreshTimer: number | null = null

const tabs = [
  { id: 'interfaces' as const, label: '接口', icon: Network },
  { id: 'applications' as const, label: '按应用', icon: AppWindow },
]

function initialNetworkTabFromHash(): NetworkTab {
  const query = window.location.hash.split('?', 2)[1]
  return query && new URLSearchParams(query).get('tab') === 'applications'
    ? 'applications'
    : 'interfaces'
}

const systemCapability = computed<NetworkCapabilityState>(() => snapshot.value?.systemTraffic ?? {
  status: error.value?.kind === 'transport' ? 'unreachable' : 'degraded',
  reason: error.value?.reason ?? 'network_snapshot_pending',
})

const applicationCapability = computed<NetworkCapabilityState>(() => snapshot.value?.perApplication ?? {
  status: error.value?.kind === 'transport' ? 'unreachable' : 'degraded',
  reason: error.value?.reason ?? 'network_snapshot_pending',
})

const coverageStatus = computed<BackendStatus>(() => {
  if (!snapshot.value) return error.value?.kind === 'transport' ? 'unreachable' : 'degraded'
  if (snapshot.value.coverage.reportedInterfaces === 0) return 'degraded'
  return snapshot.value.coverage.interfacesWithCounters === snapshot.value.coverage.reportedInterfaces
    ? 'healthy'
    : 'degraded'
})

const coverageValue = computed(() => {
  if (!snapshot.value || (
    snapshot.value.coverage.reportedInterfaces === 0
    && snapshot.value.coverage.reason === 'coverage_unknown'
  )) return 'unknown'
  return `${snapshot.value.coverage.interfacesWithCounters}/${snapshot.value.coverage.reportedInterfaces}`
})

function statusIcon(status: BackendStatus) {
  if (status === 'healthy') return CheckCircle2
  if (status === 'degraded') return CircleAlert
  if (status === 'unsupported') return CircleOff
  return Unplug
}

function formatBytes(value: number | null): string {
  if (value === null) return 'unknown'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let amount = value
  let unit = 0
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024
    unit += 1
  }
  return `${amount.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`
}

function formatRate(rate: NetworkRate, direction: 'rx' | 'tx'): string {
  if (rate.state !== 'known') return rate.state
  return `${formatBytes(direction === 'rx' ? rate.rxBytesPerSecond : rate.txBytesPerSecond)}/s`
}

function formatPercent(value: number | null): string {
  return value === null ? 'unknown' : `${value.toFixed(1)}%`
}

function formatTimestamp(value: number | null): string {
  if (value === null) return 'unknown'
  return new Intl.DateTimeFormat('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(new Date(value))
}

function interfaceKindLabel(kind: NetworkSnapshot['interfaces'][number]['kind']): string {
  return {
    physical: '物理',
    loopback: '回环',
    tunnel: '隧道',
    virtual: '虚拟',
  }[kind]
}

function selectTab(tab: NetworkTab): void {
  activeTab.value = tab
}

function onTabKeydown(event: KeyboardEvent): void {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const current = tabs.findIndex((tab) => tab.id === activeTab.value)
  const next = event.key === 'Home'
    ? 0
    : event.key === 'End'
      ? tabs.length - 1
      : (current + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length
  activeTab.value = tabs[next].id
  requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>(`[data-network-tab="${activeTab.value}"]`)?.focus()
  })
}

async function refresh(): Promise<void> {
  if (refreshing.value) return
  const generation = ++requestGeneration
  refreshing.value = true
  if (!snapshot.value) state.value = 'loading'
  const result = await getNetworkSnapshot()
  if (!active || generation !== requestGeneration) return
  refreshing.value = false
  if (result.kind === 'snapshot') {
    snapshot.value = result.snapshot
    error.value = null
    state.value = 'snapshot'
  } else {
    error.value = result.error
    state.value = snapshot.value ? 'snapshot' : 'error'
  }
}

onMounted(() => {
  void refresh()
  refreshTimer = window.setInterval(() => void refresh(), 2_000)
})

onBeforeUnmount(() => {
  active = false
  requestGeneration += 1
  if (refreshTimer !== null) window.clearInterval(refreshTimer)
})
</script>

<template>
  <section class="network-console" aria-labelledby="network-heading">
    <h1 id="network-heading" class="sr-only">网络</h1>

    <div class="network-tabs" role="tablist" aria-label="网络数据视图" @keydown="onTabKeydown">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        class="network-tab"
        :class="{ 'is-active': activeTab === tab.id }"
        type="button"
        role="tab"
        :data-network-tab="tab.id"
        :id="`network-tab-${tab.id}`"
        :aria-controls="`network-panel-${tab.id}`"
        :aria-selected="activeTab === tab.id"
        :tabindex="activeTab === tab.id ? 0 : -1"
        @click="selectTab(tab.id)"
      >
        <component :is="tab.icon" :size="18" aria-hidden="true" />
        <span>{{ tab.label }}</span>
      </button>
      <button
        class="icon-button network-refresh"
        type="button"
        aria-label="刷新网络事实"
        title="刷新网络事实"
        :disabled="refreshing"
        @click="refresh"
      >
        <RefreshCw :size="16" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
      </button>
    </div>

    <div class="network-layout">
      <main
        class="network-workspace"
        role="tabpanel"
        :id="`network-panel-${activeTab}`"
        :aria-labelledby="`network-tab-${activeTab}`"
        aria-live="polite"
      >
        <div v-if="state === 'error'" class="network-state is-error" role="status">
          <Unplug :size="40" aria-hidden="true" />
          <strong>网络快照不可用</strong>
          <code>{{ error?.reason }}</code>
          <button class="network-secondary-button" type="button" @click="refresh">
            <RefreshCw :size="15" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <div v-else-if="error" class="network-refresh-error" role="status">
          <CircleAlert :size="16" aria-hidden="true" />
          <span>刷新失败，正在显示上一次成功数据</span>
          <code>{{ error.reason }}</code>
        </div>

        <template v-if="state !== 'error' && activeTab === 'interfaces'">
          <div class="network-table-wrap">
            <table class="network-table">
              <thead>
                <tr>
                  <th scope="col">接口</th>
                  <th scope="col">类型</th>
                  <th scope="col">链路</th>
                  <th scope="col">接收</th>
                  <th scope="col">发送</th>
                  <th scope="col">状态</th>
                </tr>
              </thead>
              <tbody v-if="snapshot?.interfaces.length">
                <tr v-for="item in snapshot.interfaces" :key="item.index">
                  <th scope="row">
                    <span>{{ item.name }}</span>
                    <code>ifindex {{ item.index }}</code>
                  </th>
                  <td>
                    <span>{{ interfaceKindLabel(item.kind) }}</span>
                    <small>{{ item.kernelKind ?? item.kind }}</small>
                  </td>
                  <td>
                    <span>{{ item.isUp ? 'up' : 'down' }}</span>
                    <small>{{ item.carrierUp ? 'carrier' : 'no_carrier' }}</small>
                  </td>
                  <td>
                    <span>{{ formatRate(item.rate, 'rx') }}</span>
                    <small>{{ formatBytes(item.counters?.rxBytes ?? null) }}</small>
                  </td>
                  <td>
                    <span>{{ formatRate(item.rate, 'tx') }}</span>
                    <small>{{ formatBytes(item.counters?.txBytes ?? null) }}</small>
                  </td>
                  <td><code>{{ item.transition }}</code></td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-if="state === 'loading'" class="network-state">
            <LoaderCircle :size="38" class="is-spinning" aria-hidden="true" />
            <strong>正在读取网络快照</strong>
          </div>
          <div v-else-if="snapshot && snapshot.interfaces.length === 0" class="network-state is-warming">
            <Hourglass v-if="snapshot.aggregateRate.state === 'warming_up'" :size="42" aria-hidden="true" />
            <CircleHelp v-else :size="42" aria-hidden="true" />
            <strong>{{ snapshot.aggregateRate.state === 'warming_up' ? '等待首次网络采样' : '暂无接口数据' }}</strong>
            <code>{{ snapshot.aggregateRate.reason }}</code>
          </div>
        </template>

        <template v-else-if="state !== 'error'">
          <div class="network-table-wrap">
            <table class="network-table application-table">
              <thead>
                <tr>
                  <th scope="col">应用</th>
                  <th scope="col">接收总量</th>
                  <th scope="col">发送总量</th>
                  <th scope="col">接收占比</th>
                  <th scope="col">发送占比</th>
                </tr>
              </thead>
              <tbody v-if="snapshot?.applications.length">
                <tr v-for="application in snapshot.applications" :key="application.applicationKey">
                  <th scope="row"><code>{{ application.applicationKey }}</code></th>
                  <td>{{ formatBytes(application.rxBytes) }}</td>
                  <td>{{ formatBytes(application.txBytes) }}</td>
                  <td>{{ formatPercent(application.rxSharePercent) }}</td>
                  <td>{{ formatPercent(application.txSharePercent) }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-if="state === 'loading'" class="network-state">
            <LoaderCircle :size="38" class="is-spinning" aria-hidden="true" />
            <strong>正在读取按应用流量</strong>
          </div>
          <div v-else-if="snapshot?.perApplication.status === 'unsupported'" class="network-state is-unsupported">
            <CircleOff :size="42" aria-hidden="true" />
            <strong>按应用流量不可用</strong>
            <code>{{ snapshot.perApplication.reason }}</code>
          </div>
          <div v-else-if="snapshot && snapshot.applications.length === 0" class="network-state">
            <AppWindow :size="42" aria-hidden="true" />
            <strong>暂无按应用流量</strong>
            <code>{{ snapshot.perApplication.reason }}</code>
          </div>
        </template>
      </main>

      <aside class="network-inspector" aria-labelledby="network-inspector-heading">
        <h2 id="network-inspector-heading">网络能力</h2>
        <dl class="network-facts">
          <div class="network-fact-row">
            <dt><component :is="statusIcon(systemCapability.status)" :size="19" aria-hidden="true" />系统流量</dt>
            <dd :class="`is-${systemCapability.status}`">{{ systemCapability.status }}</dd>
            <code>{{ systemCapability.reason }}</code>
          </div>
          <div class="network-fact-row">
            <dt><component :is="statusIcon(applicationCapability.status)" :size="19" aria-hidden="true" />按应用流量</dt>
            <dd :class="`is-${applicationCapability.status}`">{{ applicationCapability.status }}</dd>
            <code>{{ applicationCapability.reason }}</code>
          </div>
          <div class="network-fact-row compact">
            <dt><Gauge :size="19" aria-hidden="true" />coverage</dt>
            <dd :class="`is-${coverageStatus}`">{{ coverageValue }}</dd>
            <code>{{ snapshot?.coverage.reason ?? 'coverage_unknown' }}</code>
          </div>
          <div class="network-fact-row compact">
            <dt><Hourglass :size="19" aria-hidden="true" />新鲜度</dt>
            <dd :class="`is-${snapshot?.freshness ?? 'degraded'}`">{{ snapshot?.freshness ?? 'unknown' }}</dd>
            <code>{{ formatTimestamp(snapshot?.lastSuccessAtUnixMs ?? null) }}</code>
          </div>
        </dl>
      </aside>
    </div>

    <footer class="network-status-strip" aria-label="网络能力摘要">
      <div class="network-status-item">
        <component :is="statusIcon(systemCapability.status)" :size="17" :class="`is-${systemCapability.status}`" aria-hidden="true" />
        <span>系统流量</span><strong :class="`is-${systemCapability.status}`">{{ systemCapability.status }}</strong>
        <code>{{ systemCapability.reason }}</code>
      </div>
      <div class="network-status-item">
        <component :is="statusIcon(applicationCapability.status)" :size="17" :class="`is-${applicationCapability.status}`" aria-hidden="true" />
        <span>按应用流量</span><strong :class="`is-${applicationCapability.status}`">{{ applicationCapability.status }}</strong>
        <code>{{ applicationCapability.reason }}</code>
      </div>
      <div class="network-status-item compact">
        <component :is="coverageStatus === 'healthy' ? CheckCircle2 : CircleHelp" :size="17" :class="`is-${coverageStatus}`" aria-hidden="true" />
        <span>coverage</span><strong :class="`is-${coverageStatus}`">{{ coverageValue }}</strong>
      </div>
    </footer>
  </section>
</template>
