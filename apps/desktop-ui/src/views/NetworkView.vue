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

import {
  cancelSpeedTest,
  getBackendCapabilityReport,
  getNetworkSnapshot,
  runSpeedTestBasic,
  runSpeedTestDeep,
} from '../backend'
import type {
  BackendCapability,
  BackendStatus,
  BridgeError,
  Iperf3Direction,
  Iperf3Result,
  LatencyTargetResult,
  LinssidLaunchResult,
  NetworkCapabilityState,
  NetworkRate,
  NetworkSnapshot,
  SpeedTestBasicEnd,
  SpeedTestDeepCommand,
  SpeedTestStage,
  SpeedTestStageData,
  WifiScanResult,
} from '../types'

type NetworkTab = 'interfaces' | 'applications' | 'speedtest'
type ViewState = 'loading' | 'snapshot' | 'error'
type SpeedTestPanel = 'basic' | 'deep' | 'purity'
type BasicState = 'idle' | 'running' | 'done' | 'error'
type IperfState = 'idle' | 'running' | 'done'
type WifiState = 'idle' | 'loading' | 'done'

const activeTab = ref<NetworkTab>(initialNetworkTabFromHash())
const state = ref<ViewState>('loading')
const snapshot = ref<NetworkSnapshot | null>(null)
const error = ref<BridgeError | null>(null)
const refreshing = ref(false)
let active = true
let requestGeneration = 0
let refreshTimer: number | null = null
let elapsedTimer: number | null = null
let basicStartedAt = 0
let iperfStartedAt = 0

const tabs = [
  { id: 'interfaces' as const, label: '接口', icon: Network },
  { id: 'applications' as const, label: '按应用', icon: AppWindow },
  { id: 'speedtest' as const, label: '测速', icon: Gauge },
]

const speedPanel = ref<SpeedTestPanel>('basic')

function initialNetworkTabFromHash(): NetworkTab {
  const query = window.location.hash.split('?', 2)[1]
  const tab = query && new URLSearchParams(query).get('tab')
  return tab === 'applications' || tab === 'speedtest' ? tab : 'interfaces'
}

// ---- capability states for the speedtest module ----
const speedtestCapability = ref<BackendCapability | null>(null)
const deeptestCapability = ref<BackendCapability | null>(null)
const capabilityLoading = ref(false)

async function loadCapabilities(): Promise<void> {
  if (capabilityLoading.value) return
  capabilityLoading.value = true
  const result = await getBackendCapabilityReport()
  if (!active) return
  capabilityLoading.value = false
  if (!result || result.kind !== 'report') return
  speedtestCapability.value =
    result.report.capabilities.find((capability) => capability.id === 'network.speedtest.v1') ?? null
  deeptestCapability.value =
    result.report.capabilities.find((capability) => capability.id === 'network.deeptest.v1') ?? null
}

function capabilityStatus(capability: BackendCapability | null): BackendStatus {
  return capability?.status ?? 'unreachable'
}

// ---- basic speed test ----
const basicState = ref<BasicState>('idle')
const basicError = ref<BridgeError | null>(null)
const basicEnd = ref<SpeedTestBasicEnd | null>(null)
const basicStages = ref<SpeedTestStageData[]>([])
const activeStage = ref<SpeedTestStage | null>(null)
const elapsedSeconds = ref(0)

const stageOrder: SpeedTestStage[] = ['latency', 'bandwidth', 'ip_purity']
const stageLabels: Record<SpeedTestStage, string> = {
  latency: '站点延迟',
  bandwidth: '网络带宽',
  ip_purity: 'IP 纯净度',
}

const activeStageIndex = computed(() =>
  activeStage.value === null ? 0 : stageOrder.indexOf(activeStage.value) + 1,
)

const requestedStages = ref<SpeedTestStage[]>([])

async function startBasicTest(stages: SpeedTestStage[]): Promise<void> {
  if (basicState.value === 'running') return
  basicError.value = null
  basicEnd.value = null
  basicStages.value = []
  activeStage.value = null
  requestedStages.value = stages
  basicState.value = 'running'
  basicStartedAt = Date.now()
  elapsedSeconds.value = 0
  const result = await runSpeedTestBasic(stages, (stage) => {
    basicStages.value = [...basicStages.value, stage]
    activeStage.value = stage.stage
  })
  if (!active) return
  if (result.kind === 'end') {
    basicEnd.value = result.end
    if (result.end.stages.length > 0) basicStages.value = result.end.stages
    basicState.value = result.end.error ? 'error' : 'done'
    if (result.end.error) {
      basicError.value = {
        kind: 'daemon',
        code: 'speedtest_failed',
        reason: result.end.error,
        retryable: true,
      }
    }
  } else {
    basicError.value = result.error
    basicState.value = 'error'
  }
}

const loadedStageCount = computed(() => basicStages.value.length)

function stageLoaded(stage: SpeedTestStage): boolean {
  return basicStages.value.some((item) => item.stage === stage)
}

async function cancelBasicTest(): Promise<void> {
  const result = await cancelSpeedTest()
  if (result.kind === 'error') {
    basicError.value = result.error
  }
}

function stageData(stage: SpeedTestStage): SpeedTestStageData | undefined {
  return basicStages.value.find((item) => item.stage === stage)
}

const latencyTargets = computed<LatencyTargetResult[]>(() => {
  const data = stageData('latency')
  return data && data.stage === 'latency' ? data.payload.targets : []
})

const internationalMeasurement = computed(() => {
  const data = stageData('bandwidth')
  if (!data || data.stage !== 'bandwidth') return null
  return data.payload.measurements.find((item) => item.kind === 'international') ?? null
})

const domesticMeasurements = computed(() => {
  const data = stageData('bandwidth')
  if (!data || data.stage !== 'bandwidth') return []
  return data.payload.measurements.filter((item) => item.kind === 'domestic')
})

const purity = computed(() => {
  const data = stageData('ip_purity')
  return data && data.stage === 'ip_purity' ? data.payload.purity : null
})

const purityVerdict = computed<{ label: string; status: BackendStatus; reason: string }>(() => {
  const result = purity.value
  if (!result) return { label: '未知', status: 'unreachable', reason: 'ip_purity_not_measured' }
  if (result.error) return { label: '未知', status: 'degraded', reason: result.error }
  if (result.proxy === true) return { label: '代理出口', status: 'degraded', reason: 'proxy_flag' }
  if (result.hosting === true) return { label: '机房 IP', status: 'degraded', reason: 'hosting_flag' }
  if (result.mobile === true) return { label: '移动网络', status: 'degraded', reason: 'mobile_flag' }
  if (result.riskScore !== null && result.riskScore >= 60) {
    return { label: '高风险', status: 'degraded', reason: `risk_score_${result.riskScore}` }
  }
  return { label: '未标记', status: 'healthy', reason: 'no_proxy_or_hosting_flags' }
})

const riskLabel = computed<{ label: string; tone: 'good' | 'warn' | 'bad' }>(() => {
  const score = purity.value?.riskScore
  if (score === null || score === undefined) return { label: '—', tone: 'warn' }
  if (score < 25) return { label: '低风险', tone: 'good' }
  if (score < 60) return { label: '中风险', tone: 'warn' }
  return { label: '高风险', tone: 'bad' }
})

const humanBot = computed<{ human: number | null; bot: number | null }>(() => {
  const score = purity.value?.riskScore
  if (score === null || score === undefined) return { human: null, bot: null }
  return { human: 100 - score, bot: score }
})

// ---- deep speed test ----
const iperfServer = ref('127.0.0.1')
const iperfPort = ref(5201)
const iperfDirection = ref<Iperf3Direction>('upload')
const iperfDuration = ref(10)
const iperfParallel = ref(1)
const iperfState = ref<IperfState>('idle')
const iperfError = ref<BridgeError | null>(null)
const iperfResult = ref<Iperf3Result | null>(null)

async function runIperf3(): Promise<void> {
  if (iperfState.value === 'running') return
  const server = iperfServer.value.trim()
  if (server.length === 0 || server.length > 253) return
  const port = Math.trunc(iperfPort.value)
  if (!Number.isInteger(port) || port < 1 || port > 65535) return
  const duration = Math.trunc(iperfDuration.value)
  if (!Number.isInteger(duration) || duration < 1 || duration > 60) return
  const parallel = Math.trunc(iperfParallel.value)
  if (!Number.isInteger(parallel) || parallel < 1 || parallel > 8) return
  const command: SpeedTestDeepCommand = {
    command: 'iperf3_start',
    params: {
      server,
      port,
      direction: iperfDirection.value,
      duration_secs: duration,
      parallel,
    },
  }
  iperfError.value = null
  iperfResult.value = null
  iperfState.value = 'running'
  iperfStartedAt = Date.now()
  elapsedSeconds.value = 0
  const result = await runSpeedTestDeep(command)
  if (!active) return
  iperfState.value = 'done'
  if (result.kind === 'output' && result.output.type === 'iperf3') {
    iperfResult.value = result.output.payload
  } else if (result.kind === 'output') {
    iperfError.value = {
      kind: 'protocol',
      code: 'invalid_iperf3_output',
      reason: 'invalid_iperf3_output',
      retryable: false,
    }
  } else {
    iperfError.value = result.error
  }
}

async function stopIperf3(): Promise<void> {
  const command: SpeedTestDeepCommand = { command: 'iperf3_stop', params: null }
  const result = await runSpeedTestDeep(command)
  if (!active) return
  if (result.kind === 'output' && result.output.type === 'iperf3') {
    if (result.output.payload.error) {
      iperfError.value = {
        kind: 'daemon',
        code: 'iperf3_stop',
        reason: result.output.payload.error,
        retryable: false,
      }
    }
  } else if (result.kind === 'error') {
    iperfError.value = result.error
  }
}

// ---- wifi scan ----
const wifiState = ref<WifiState>('idle')
const wifiError = ref<BridgeError | null>(null)
const wifiResult = ref<WifiScanResult | null>(null)

async function scanWifi(): Promise<void> {
  if (wifiState.value === 'loading') return
  wifiError.value = null
  wifiState.value = 'loading'
  const result = await runSpeedTestDeep({ command: 'wifi_scan', params: null })
  if (!active) return
  wifiState.value = 'done'
  if (result.kind === 'output' && result.output.type === 'wifi_scan') {
    wifiResult.value = result.output.payload
  } else if (result.kind === 'output') {
    wifiError.value = {
      kind: 'protocol',
      code: 'invalid_wifi_scan_output',
      reason: 'invalid_wifi_scan_output',
      retryable: false,
    }
  } else {
    wifiError.value = result.error
  }
}

// ---- linssid ----
const linssidResult = ref<LinssidLaunchResult | null>(null)
const linssidError = ref<BridgeError | null>(null)

async function launchLinssid(): Promise<void> {
  linssidError.value = null
  const result = await runSpeedTestDeep({ command: 'linssid_launch', params: null })
  if (!active) return
  if (result.kind === 'output' && result.output.type === 'linssid') {
    linssidResult.value = result.output.payload
  } else if (result.kind === 'output') {
    linssidError.value = {
      kind: 'protocol',
      code: 'invalid_linssid_output',
      reason: 'invalid_linssid_output',
      retryable: false,
    }
  } else {
    linssidError.value = result.error
  }
}

// ---- shared helpers ----
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

function formatClock(value: number | null): string {
  return formatTimestamp(value)
}

function formatMbps(bitsPerSecond: number | null): string {
  if (bitsPerSecond === null) return '—'
  return `${(bitsPerSecond / 1_000_000).toFixed(1)}`
}

function interfaceKindLabel(kind: NetworkSnapshot['interfaces'][number]['kind']): string {
  return {
    physical: '物理',
    loopback: '回环',
    tunnel: '隧道',
    virtual: '虚拟',
  }[kind]
}

function latencyVerdict(target: LatencyTargetResult): { label: string; tone: 'good' | 'slow' } {
  const avg = target.avgTtfbMs
  if (avg === null) return { label: '未知', tone: 'slow' }
  return avg < 500 ? { label: '正常', tone: 'good' } : { label: '慢', tone: 'slow' }
}

function probeText(probe: { ttfbMs: number | null; error: string | null }): string {
  if (probe.ttfbMs !== null) return `${probe.ttfbMs}`
  return probe.error ?? '—'
}

function selectTab(tab: NetworkTab): void {
  activeTab.value = tab
  if (tab === 'speedtest') {
    void loadCapabilities()
  }
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
  if (activeTab.value === 'speedtest') {
    void loadCapabilities()
  }
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
  elapsedTimer = window.setInterval(() => {
    if (basicState.value === 'running') {
      elapsedSeconds.value = Math.round((Date.now() - basicStartedAt) / 1000)
    } else if (iperfState.value === 'running') {
      elapsedSeconds.value = Math.round((Date.now() - iperfStartedAt) / 1000)
    }
  }, 1_000)
})

onBeforeUnmount(() => {
  active = false
  requestGeneration += 1
  if (refreshTimer !== null) window.clearInterval(refreshTimer)
  if (elapsedTimer !== null) window.clearInterval(elapsedTimer)
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
            <table class="network-table interface-table">
              <colgroup>
                <col class="interface-column" />
                <col class="kind-column" />
                <col class="link-column" />
                <col class="rate-column" />
                <col class="rate-column" />
                <col class="status-column" />
              </colgroup>
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

        <template v-else-if="state !== 'error' && activeTab === 'applications'">
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

        <template v-else-if="state !== 'error'">
          <div class="speed-panel">
            <div class="speed-segmented" role="group" aria-label="测速模式">
              <button
                class="speed-segment"
                :class="{ 'is-active': speedPanel === 'basic' }"
                type="button"
                @click="speedPanel = 'basic'"
              >
                基础测速
              </button>
              <button
                class="speed-segment"
                :class="{ 'is-active': speedPanel === 'deep' }"
                type="button"
                @click="speedPanel = 'deep'"
              >
                深度测速
              </button>
              <button
                class="speed-segment"
                :class="{ 'is-active': speedPanel === 'purity' }"
                type="button"
                @click="speedPanel = 'purity'"
              >
                IP 纯净度
              </button>
            </div>

            <!-- 基础测速 -->
            <div v-if="speedPanel === 'basic'" class="speed-basic">
              <div class="speed-toolbar">
                <span class="capability-token" :class="`is-${capabilityStatus(speedtestCapability)}`">
                  network.speedtest.v1
                </span>
                <code class="speed-capability-reason">{{ speedtestCapability?.reason ?? 'capability_unknown' }}</code>
                <span v-if="basicEnd" class="speed-last-run">上次测速 {{ formatClock(basicEnd.endedAtUnixMs) }}</span>
                <button
                  v-if="basicState !== 'running'"
                  class="speed-start"
                  type="button"
                  :disabled="capabilityStatus(speedtestCapability) === 'unsupported'"
                  @click="() => startBasicTest(['latency', 'bandwidth'])"
                >
                  开始测速
                </button>
                <button v-else class="speed-secondary" type="button" @click="cancelBasicTest">取消</button>
              </div>

              <div v-if="basicState === 'idle'" class="speed-state">
                <Hourglass :size="38" aria-hidden="true" />
                <strong>尚未测速</strong>
                <code>点击「开始测速」测量站点延迟、网络带宽与 IP 纯净度</code>
              </div>

              <div v-else-if="basicState === 'running'" class="speed-state" role="status">
                <LoaderCircle :size="38" class="is-spinning" aria-hidden="true" />
                <strong>正在测量：{{ stageLabels[activeStage ?? 'latency'] }}</strong>
                <code>阶段 {{ activeStageIndex }}/3 · 已运行 {{ elapsedSeconds }}s · 可在任一阶段间隙取消</code>
              </div>

              <template v-else>
                <div v-if="basicError" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>测速失败</span>
                  <code>{{ basicError.reason }}</code>
                  <button class="network-secondary-button" type="button" @click="() => startBasicTest(['latency', 'bandwidth'])">
                    <RefreshCw :size="15" aria-hidden="true" />
                    <span>重试</span>
                  </button>
                </div>
                <div v-else-if="basicEnd?.cancelled" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>测速已取消</span>
                  <code>cancelled_by_user</code>
                </div>

                <section class="speed-band" aria-label="站点延迟">
                  <h2 class="speed-band-heading">
                    站点延迟
                    <small>HTTP 首字节 TTFB · 3 次探测 · 经透明代理时连接时间为代理本地应答，以 TTFB 为准</small>
                  </h2>
                  <div v-if="!stageLoaded('latency') && basicState === 'running'" class="speed-stage-pending" role="status">
                    <LoaderCircle :size="16" class="is-spinning" aria-hidden="true" />
                    <span>正在测量：站点延迟…</span>
                  </div>
                  <div v-else-if="!stageLoaded('latency') && basicState !== 'running'" class="speed-stage-pending">
                    <CircleHelp :size="16" aria-hidden="true" />
                    <span>本组未测得</span>
                  </div>
                  <div v-if="stageLoaded('latency')" class="network-table-wrap">
                    <table class="network-table speed-latency-table">
                      <thead>
                        <tr>
                          <th scope="col">站点</th>
                          <th scope="col">均值</th>
                          <th scope="col">探测 1</th>
                          <th scope="col">探测 2</th>
                          <th scope="col">探测 3</th>
                          <th scope="col">判定</th>
                        </tr>
                      </thead>
                      <tbody>
                        <tr v-for="target in latencyTargets" :key="target.host">
                          <th scope="row">
                            <span>{{ target.host }}</span>
                            <small>https · 443</small>
                          </th>
                          <td class="speed-num">{{ target.avgTtfbMs === null ? '—' : `${target.avgTtfbMs} ms` }}</td>
                          <td v-for="(probe, index) in target.probes" :key="index" class="speed-num">
                            {{ probeText(probe) }}
                          </td>
                          <td>
                            <span class="latency-pill" :class="`is-${latencyVerdict(target).tone}`">
                              {{ latencyVerdict(target).label }}
                            </span>
                          </td>
                        </tr>
                      </tbody>
                    </table>
                  </div>
                </section>

                <section class="speed-band" aria-label="网络带宽">
                  <h2 class="speed-band-heading">网络带宽</h2>
                  <div v-if="!stageLoaded('bandwidth') && basicState === 'running'" class="speed-stage-pending" role="status">
                    <LoaderCircle :size="16" class="is-spinning" aria-hidden="true" />
                    <span>正在测量：网络带宽…</span>
                  </div>
                  <div v-else-if="!stageLoaded('bandwidth') && basicState !== 'running'" class="speed-stage-pending">
                    <CircleHelp :size="16" aria-hidden="true" />
                    <span>本组未测得</span>
                  </div>
                  <template v-if="stageLoaded('bandwidth')">
                  <div class="bw-subgroup">
                    <h3 class="bw-subgroup-heading">
                      国际线路
                      <code>speed.cloudflare.com/__down · __up</code>
                    </h3>
                    <div class="bw-metrics">
                      <div class="bw-metric">
                        <span class="bw-value">{{ formatMbps(internationalMeasurement?.downloadBitsPerSecond ?? null) }}</span>
                        <span class="bw-unit">Mbps</span>
                        <span class="bw-label">下载</span>
                      </div>
                      <div class="bw-metric">
                        <span class="bw-value">{{ formatMbps(internationalMeasurement?.uploadBitsPerSecond ?? null) }}</span>
                        <span class="bw-unit">Mbps</span>
                        <span class="bw-label">上传</span>
                      </div>
                      <span v-if="internationalMeasurement?.error" class="bw-error">
                        <code>{{ internationalMeasurement.error }}</code>
                      </span>
                    </div>
                  </div>
                  <div class="bw-subgroup">
                    <h3 class="bw-subgroup-heading">
                      国内镜像下载
                      <code>ubuntu-releases · 限时 12s/镜像</code>
                    </h3>
                    <div class="network-table-wrap">
                      <table class="network-table speed-mirror-table">
                        <thead>
                          <tr>
                            <th scope="col">镜像</th>
                            <th scope="col">速率</th>
                            <th scope="col">结果</th>
                            <th scope="col">原因</th>
                          </tr>
                        </thead>
                        <tbody>
                          <tr v-for="measurement in domesticMeasurements" :key="measurement.label">
                            <th scope="row"><span>{{ measurement.label }}</span><small>{{ measurement.source }}</small></th>
                            <td class="speed-num">{{ formatMbps(measurement.downloadBitsPerSecond) }} Mbps</td>
                            <td>
                              <span :class="measurement.error ? 'speed-fail' : 'speed-ok'">
                                {{ measurement.error ? '不可达' : '可达' }}
                              </span>
                            </td>
                            <td><code>{{ measurement.error ?? (measurement.httpCode === null ? '—' : `http_${measurement.httpCode}`) }}</code></td>
                          </tr>
                        </tbody>
                      </table>
                    </div>
                    <p v-if="domesticMeasurements.length > 0 && domesticMeasurements.every((item) => item.error)" class="speed-note">
                      国内镜像全部不可达：当前流量可能经代理线路，此组结果不代表国内带宽；镜像列表由 appd 配置。
                    </p>
                  </div>
                  </template>
                </section>
              </template>
            </div>

            <!-- 深度测速 -->
            <div v-else-if="speedPanel === 'deep'" class="speed-deep">
              <div class="speed-toolbar">
                <span class="capability-token" :class="`is-${capabilityStatus(deeptestCapability)}`">
                  network.deeptest.v1
                </span>
                <code class="speed-capability-reason">{{ deeptestCapability?.reason ?? 'capability_unknown' }}</code>
              </div>

              <section class="speed-band" aria-label="iperf3 测速">
                <h2 class="speed-band-heading">iperf3 测速 <small>手动指定服务器 · 结果来自 iperf3 --json</small></h2>
                <form class="iperf-form" @submit.prevent="runIperf3">
                  <div class="iperf-field">
                    <label for="iperf-server">服务器</label>
                    <input id="iperf-server" v-model="iperfServer" placeholder="host" required>
                  </div>
                  <div class="iperf-field">
                    <label for="iperf-port">端口</label>
                    <input id="iperf-port" v-model.number="iperfPort" type="number" min="1" max="65535" class="iperf-short" required>
                  </div>
                  <div class="iperf-field">
                    <label for="iperf-direction">方向</label>
                    <select id="iperf-direction" v-model="iperfDirection" class="iperf-short">
                      <option value="upload">上传</option>
                      <option value="download">下载</option>
                      <option value="bidirectional">双向</option>
                    </select>
                  </div>
                  <div class="iperf-field">
                    <label for="iperf-duration">时长 (s)</label>
                    <input id="iperf-duration" v-model.number="iperfDuration" type="number" min="1" max="60" class="iperf-short" required>
                  </div>
                  <div class="iperf-field">
                    <label for="iperf-parallel">并行</label>
                    <input id="iperf-parallel" v-model.number="iperfParallel" type="number" min="1" max="8" class="iperf-short" required>
                  </div>
                  <button class="speed-start" type="submit" :disabled="iperfState === 'running'">启动 iperf3</button>
                  <button v-if="iperfState === 'running'" class="speed-secondary" type="button" @click="stopIperf3">停止</button>
                </form>

                <div v-if="iperfState === 'running'" class="iperf-running" role="status">
                  <LoaderCircle :size="20" class="is-spinning" aria-hidden="true" />
                  <span>iperf3 运行中（已 {{ elapsedSeconds }}s）…</span>
                </div>
                <div v-else-if="iperfError" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>iperf3 失败</span>
                  <code>{{ iperfError.reason }}</code>
                </div>
                <div v-else-if="iperfResult" class="iperf-result" role="status">
                  <div class="iperf-result-grid">
                    <div class="iperf-result-item">
                      <span>下载</span>
                      <strong>{{ formatMbps(iperfResult.downloadBitsPerSecond) }} <small>Mbps</small></strong>
                    </div>
                    <div class="iperf-result-item">
                      <span>上传</span>
                      <strong>{{ formatMbps(iperfResult.uploadBitsPerSecond) }} <small>Mbps</small></strong>
                    </div>
                    <div class="iperf-result-item">
                      <span>重传</span>
                      <strong>{{ iperfResult.retransmits ?? '—' }}</strong>
                    </div>
                    <div class="iperf-result-item">
                      <span>抖动</span>
                      <strong>{{ iperfResult.jitterMs === null ? '—' : `${iperfResult.jitterMs.toFixed(2)} ms` }}</strong>
                    </div>
                  </div>
                  <code class="iperf-result-meta">
                    {{ iperfResult.server }}:{{ iperfResult.port }} · {{ iperfResult.direction }} · {{ iperfResult.durationSecs }}s · {{ iperfResult.parallel }} 并行
                    <template v-if="iperfResult.error"> · {{ iperfResult.error }}</template>
                  </code>
                </div>
                <div v-else class="iperf-empty">
                  尚未运行测速 · 输入自建或局域网 iperf3 服务器地址
                </div>
              </section>

              <section class="speed-band" aria-label="WiFi 扫描">
                <h2 class="speed-band-heading">WiFi 扫描 <code>nmcli dev wifi list</code></h2>
                <div class="speed-toolbar">
                  <button class="speed-secondary" type="button" :disabled="wifiState === 'loading'" @click="scanWifi">
                    <RefreshCw :size="15" :class="{ 'is-spinning': wifiState === 'loading' }" aria-hidden="true" />
                    <span>{{ wifiState === 'loading' ? '扫描中' : '扫描' }}</span>
                  </button>
                  <span v-if="wifiResult" class="speed-last-run">
                    上次扫描 {{ formatClock(wifiResult.scannedAtUnixMs) }} · 来源 {{ wifiResult.source }}
                  </span>
                </div>
                <div v-if="wifiError" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>WiFi 扫描失败</span>
                  <code>{{ wifiError.reason }}</code>
                </div>
                <div v-else-if="wifiResult" class="network-table-wrap">
                  <table class="network-table speed-wifi-table">
                    <thead>
                      <tr>
                        <th scope="col">SSID</th>
                        <th scope="col">信号</th>
                        <th scope="col">信道</th>
                        <th scope="col">频段</th>
                        <th scope="col">安全</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr v-for="network in wifiResult.networks" :key="network.ssid + network.channel">
                        <th scope="row"><span>{{ network.ssid }}</span></th>
                        <td class="speed-num">{{ network.signalPercent === null ? '—' : `${network.signalPercent}%` }}</td>
                        <td class="speed-num">{{ network.channel ?? '—' }}</td>
                        <td>{{ network.band ?? '—' }}</td>
                        <td><code>{{ network.security ?? '—' }}</code></td>
                      </tr>
                    </tbody>
                  </table>
                </div>
                <div v-else class="speed-state is-small">
                  <CircleHelp :size="32" aria-hidden="true" />
                  <strong>尚未扫描</strong>
                  <code>点击「扫描」读取当前 WiFi 环境（无需 root）</code>
                </div>
              </section>

              <section class="speed-band" aria-label="LinSSID">
                <h2 class="speed-band-heading">LinSSID <small>WiFi 分析工具（外部 GUI）</small></h2>
                <div class="tool-row">
                  <code class="tool-path">{{ linssidResult?.executable ?? '未检测到 linssid' }}</code>
                  <span class="tool-state">需要 root 授权 · 启动将弹出 polkit 窗口</span>
                  <button class="speed-secondary" type="button" @click="launchLinssid">启动 LinSSID</button>
                </div>
                <div v-if="linssidError" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>启动失败</span>
                  <code>{{ linssidError.reason }}</code>
                </div>
                <p v-else-if="linssidResult" class="speed-note">
                  <code>{{ linssidResult.reason }}</code>
                </p>
              </section>
            </div>

            <div v-else class="speed-purity">
              <div class="speed-toolbar">
                <span class="capability-token" :class="`is-${capabilityStatus(speedtestCapability)}`">
                  network.speedtest.v1
                </span>
                <code class="speed-capability-reason">{{ speedtestCapability?.reason ?? 'capability_unknown' }}</code>
                <span v-if="basicEnd" class="speed-last-run">上次检测 {{ formatClock(basicEnd.endedAtUnixMs) }}</span>
                <button
                  v-if="basicState !== 'running'"
                  class="speed-start"
                  type="button"
                  :disabled="capabilityStatus(speedtestCapability) === 'unsupported'"
                  @click="() => startBasicTest(['ip_purity'])"
                >
                  检测
                </button>
                <button v-else class="speed-secondary" type="button" @click="cancelBasicTest">取消</button>
              </div>

              <div v-if="basicState === 'idle'" class="speed-state">
                <Hourglass :size="38" aria-hidden="true" />
                <strong>尚未检测</strong>
                <code>点击「检测」查询出口 IP 基础事实与风险值（ip-api.com + ipok.io）</code>
              </div>

              <div v-else-if="basicState === 'running'" class="speed-state" role="status">
                <LoaderCircle :size="38" class="is-spinning" aria-hidden="true" />
                <strong>正在检测 IP 风险…</strong>
                <code>已运行 {{ elapsedSeconds }}s · 查询 ip-api.com 与 ipok.io</code>
              </div>

              <template v-else>
                <div v-if="basicError" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>检测失败</span>
                  <code>{{ basicError.reason }}</code>
                  <button class="network-secondary-button" type="button" @click="() => startBasicTest(['ip_purity'])">
                    <RefreshCw :size="15" aria-hidden="true" />
                    <span>重试</span>
                  </button>
                </div>
                <div v-else-if="basicEnd?.cancelled" class="speed-refresh-error" role="status">
                  <CircleAlert :size="16" aria-hidden="true" />
                  <span>检测已取消</span>
                  <code>cancelled_by_user</code>
                </div>

                <section class="speed-band" aria-label="IP 纯净度">
                  <h2 class="speed-band-heading">
                    IP 纯净度
                    <small>基础事实来自 ip-api.com · 风险值来自 ipok.io 公开接口（7 源加权）</small>
                  </h2>
                  <div class="network-table-wrap">
                    <table class="network-table speed-purity-table">
                      <thead>
                        <tr>
                          <th scope="col">项目</th>
                          <th scope="col">值</th>
                          <th scope="col">来源</th>
                        </tr>
                      </thead>
                      <tbody>
                        <tr>
                          <th scope="row"><span>出口 IP</span></th>
                          <td><code>{{ purity?.ip ?? '—' }}</code></td>
                          <td><code>ip-api.com</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>地理位置</span></th>
                          <td><span>{{ purity ? [purity.country, purity.region, purity.city].filter(Boolean).join(' · ') || '—' : '—' }}</span></td>
                          <td><code>ip-api.com</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>ISP</span></th>
                          <td><span>{{ purity?.isp ?? '—' }}</span></td>
                          <td><code>ip-api.com</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>ASN</span></th>
                          <td><code>{{ purity?.asn ?? '—' }}<template v-if="purity?.asname"> · {{ purity.asname }}</template></code></td>
                          <td><code>ip-api.com</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>IP 类型</span></th>
                          <td><span>{{ purity?.ipType ?? '—' }}</span></td>
                          <td><code>ipok.io · ipType</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>风险值</span></th>
                          <td>
                            <template v-if="purity?.riskScore !== null && purity?.riskScore !== undefined">
                              <span class="risk-pill" :class="`is-${riskLabel.tone}`">{{ riskLabel.label }}</span>
                              <code>{{ purity.riskScore }}/100<template v-if="purity.riskError"> · {{ purity.riskError }}</template></code>
                            </template>
                            <code v-else>{{ purity?.riskError ?? '—' }}</code>
                          </td>
                          <td><code>ipok.io</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>人机占比</span></th>
                          <td>
                            <template v-if="humanBot.human !== null">
                              <span>真人 {{ humanBot.human }}% · 机器人 {{ humanBot.bot }}%</span>
                            </template>
                            <code v-else>—</code>
                          </td>
                          <td><code>派生：机器人占比 = 风险值</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>标记</span></th>
                          <td>
                            <span>proxy {{ purity?.proxy ?? '—' }} · hosting {{ purity?.hosting ?? '—' }} · mobile {{ purity?.mobile ?? '—' }}</span>
                            <small v-if="purity?.signals.length">signals: {{ purity.signals.join(' · ') }}</small>
                          </td>
                          <td><code>ip-api.com + ipok.io</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>黑名单</span></th>
                          <td>
                            <template v-if="purity?.blocklistChecked !== null && purity?.blocklistChecked !== undefined">
                              <span :class="purity.blocklistListed.length > 0 ? 'speed-fail' : 'speed-ok'">
                                {{ purity.blocklistListed.length > 0 ? purity.blocklistListed.join(' · ') : '未命中' }}
                              </span>
                              <small>检查 {{ purity.blocklistChecked }} 个库</small>
                            </template>
                            <code v-else>—</code>
                          </td>
                          <td><code>ipok.io</code></td>
                        </tr>
                        <tr>
                          <th scope="row"><span>判定</span></th>
                          <td>
                            <span class="dd-verdict" :class="`is-${purityVerdict.status}`">{{ purityVerdict.label }}</span>
                            <small>{{ purityVerdict.reason }}</small>
                          </td>
                          <td><code>派生</code></td>
                        </tr>
                      </tbody>
                    </table>
                  </div>

                  <div v-if="purity?.riskSources.length" class="bw-subgroup">
                    <h3 class="bw-subgroup-heading">风控来源明细 <code>ipok.io riskBreakdown</code></h3>
                    <div class="network-table-wrap">
                      <table class="network-table speed-purity-table">
                        <thead>
                          <tr>
                            <th scope="col">来源</th>
                            <th scope="col">风险</th>
                            <th scope="col">权重</th>
                          </tr>
                        </thead>
                        <tbody>
                          <tr v-for="source in purity?.riskSources" :key="source.source">
                            <th scope="row"><span>{{ source.source }}</span></th>
                            <td><code>{{ source.risk ?? '—' }}</code></td>
                            <td><code>{{ source.weight ?? '—' }}</code></td>
                          </tr>
                        </tbody>
                      </table>
                    </div>
                  </div>

                  <p class="speed-note">
                    风险值与风控来源明细来自 <code>ipok.io/api/ip</code> 公开接口；「人机占比」为派生值
                    <code>（机器人占比 = 风险值，真人占比 = 100 − 风险值）</code>，非第三方直接返回。
                  </p>
                </section>
              </template>
            </div>
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
          <div v-if="activeTab === 'speedtest'" class="network-fact-row">
            <dt><component :is="statusIcon(capabilityStatus(speedtestCapability))" :size="19" aria-hidden="true" />基础测速</dt>
            <dd :class="`is-${capabilityStatus(speedtestCapability)}`">{{ capabilityStatus(speedtestCapability) }}</dd>
            <code>{{ speedtestCapability?.reason ?? 'capability_unknown' }}</code>
          </div>
          <div v-if="activeTab === 'speedtest'" class="network-fact-row">
            <dt><component :is="statusIcon(capabilityStatus(deeptestCapability))" :size="19" aria-hidden="true" />深度测速</dt>
            <dd :class="`is-${capabilityStatus(deeptestCapability)}`">{{ capabilityStatus(deeptestCapability) }}</dd>
            <code>{{ deeptestCapability?.reason ?? 'capability_unknown' }}</code>
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
      <template v-if="activeTab === 'speedtest'">
        <div class="network-status-item compact">
          <component :is="statusIcon(capabilityStatus(speedtestCapability))" :size="17" :class="`is-${capabilityStatus(speedtestCapability)}`" aria-hidden="true" />
          <span>基础测速</span><strong :class="`is-${capabilityStatus(speedtestCapability)}`">{{ capabilityStatus(speedtestCapability) }}</strong>
          <code>{{ speedtestCapability?.reason ?? 'capability_unknown' }}</code>
        </div>
        <div class="network-status-item compact">
          <component :is="statusIcon(capabilityStatus(deeptestCapability))" :size="17" :class="`is-${capabilityStatus(deeptestCapability)}`" aria-hidden="true" />
          <span>深度测速</span><strong :class="`is-${capabilityStatus(deeptestCapability)}`">{{ capabilityStatus(deeptestCapability) }}</strong>
          <code>{{ deeptestCapability?.reason ?? 'capability_unknown' }}</code>
        </div>
      </template>
      <div v-if="activeTab !== 'speedtest'" class="network-status-item compact">
        <component :is="coverageStatus === 'healthy' ? CheckCircle2 : CircleHelp" :size="17" :class="`is-${coverageStatus}`" aria-hidden="true" />
        <span>coverage</span><strong :class="`is-${coverageStatus}`">{{ coverageValue }}</strong>
      </div>
    </footer>
  </section>
</template>
