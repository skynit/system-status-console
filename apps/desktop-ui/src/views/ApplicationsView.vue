<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import {
  AppWindow,
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  BookOpenText,
  CalendarDays,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleHelp,
  CircleOff,
  Clock3,
  Inbox,
  LoaderCircle,
  Radio,
  RefreshCw,
  UserRoundCheck,
} from 'lucide-vue-next'

import { getBackendHealth, getTelemetrySnapshot, getUsageSummary } from '../backend'
import type {
  ApplicationTelemetry,
  BackendHealth,
  BackendStatus,
  BridgeError,
  MetricValue,
  TelemetrySnapshot,
  UsageApplicationDuration,
  UsagePeriod,
  UsageSummary,
} from '../types'

type ApplicationPanel = 'resources' | 'usage'
type ResourceState = 'loading' | 'snapshot' | 'error'
type UsageState = 'idle' | 'loading' | 'summary' | 'error'
type ResourceSortKey = 'application' | 'cpu' | 'memory' | 'processes' | 'fileDescriptors' | 'grouping'
type SortDirection = 'ascending' | 'descending'

const RESOURCE_REFRESH_INTERVAL_MS = 1_000
const USAGE_REFRESH_INTERVAL_MS = 10_000

const resourceSortColumns: Array<{ key: ResourceSortKey; label: string }> = [
  { key: 'application', label: '应用' },
  { key: 'cpu', label: 'CPU' },
  { key: 'memory', label: '内存' },
  { key: 'processes', label: '进程' },
  { key: 'fileDescriptors', label: '文件句柄' },
  { key: 'grouping', label: '归并' },
]

const route = useRoute()
const activePanel = ref<ApplicationPanel>(route.query.panel === 'usage' ? 'usage' : 'resources')
const resourceState = ref<ResourceState>('loading')
const snapshot = ref<TelemetrySnapshot | null>(null)
const resourceError = ref<BridgeError | null>(null)
const backendHealth = ref<BackendHealth>({
  status: 'unsupported',
  capabilityReason: 'health_not_requested',
})
const usagePeriod = ref<UsagePeriod>(route.query.period === 'weekly' ? 'weekly' : 'daily')
const currentDate = ref(startOfLocalDay(new Date()))
const selectedDate = ref(startOfLocalDay(currentDate.value))
const usageState = ref<UsageState>('idle')
const usageSummary = ref<UsageSummary | null>(null)
const usageError = ref<BridgeError | null>(null)
const resourceRefreshing = ref(false)
const refreshing = ref(false)
const resourceSortKey = ref<ResourceSortKey | null>(null)
const resourceSortDirection = ref<SortDirection>('ascending')
let resourceGeneration = 0
let usageGeneration = 0
let active = true
let resourceRefreshTimer: number | null = null
let usageRefreshTimer: number | null = null

const applications = computed(() => snapshot.value?.applications ?? [])
const sortedApplications = computed(() => {
  if (resourceSortKey.value === null) return applications.value
  const key = resourceSortKey.value
  const direction = resourceSortDirection.value
  return applications.value
    .map((application, index) => ({ application, index }))
    .sort((left, right) => {
      const comparison = compareSortValues(
        resourceSortValue(left.application, key),
        resourceSortValue(right.application, key),
        direction,
      )
      return comparison || left.index - right.index
    })
    .map(({ application }) => application)
})
const usageApplications = computed(() => usageSummary.value?.applications ?? [])
const activeRefreshing = computed(() => activePanel.value === 'resources'
  ? resourceRefreshing.value
  : refreshing.value)
const totalDurationNs = computed(() => usageApplications.value.reduce(
  (total, application) => total + application.durationNs,
  0,
))
const telemetryAvailability = computed(() => {
  if (resourceError.value?.kind === 'transport') return 'unreachable'
  if (resourceError.value) return 'degraded'
  return snapshot.value?.status === 'complete' && snapshot.value.freshness === 'fresh'
    ? 'healthy'
    : 'degraded'
})
const selectedBucketKey = computed(() => usagePeriod.value === 'daily'
  ? localDateKey(selectedDate.value)
  : isoWeekKey(selectedDate.value))
const selectedBucketLabel = computed(() => usagePeriod.value === 'daily'
  ? `${localDateKey(selectedDate.value)} ${new Intl.DateTimeFormat('zh-CN', { weekday: 'short' }).format(selectedDate.value)}`
  : selectedBucketKey.value)
const currentBucketKey = computed(() => usagePeriod.value === 'daily'
  ? localDateKey(currentDate.value)
  : isoWeekKey(currentDate.value))
const isCurrentBucket = computed(() => selectedBucketKey.value === currentBucketKey.value)
const usageAvailability = computed<BackendStatus>(() => {
  if (usageError.value?.kind === 'transport') return 'unreachable'
  if (usageError.value) return 'degraded'
  return usageSummary.value?.status ?? 'degraded'
})

function startOfLocalDay(value: Date): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate())
}

function localDateKey(value: Date): string {
  const year = value.getFullYear()
  const month = String(value.getMonth() + 1).padStart(2, '0')
  const day = String(value.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

function isoWeekKey(value: Date): string {
  const date = new Date(Date.UTC(value.getFullYear(), value.getMonth(), value.getDate()))
  const weekday = date.getUTCDay() || 7
  date.setUTCDate(date.getUTCDate() + 4 - weekday)
  const isoYear = date.getUTCFullYear()
  const yearStart = new Date(Date.UTC(isoYear, 0, 1))
  const week = Math.ceil((((date.getTime() - yearStart.getTime()) / 86_400_000) + 1) / 7)
  return `${isoYear}-W${String(week).padStart(2, '0')}`
}

function formatPercent(metric: MetricValue): string {
  return metric.state === 'known' && metric.value !== null
    ? `${metric.value.toFixed(1)}%`
    : metric.state
}

function formatBytes(metric: MetricValue): string {
  if (metric.state !== 'known' || metric.value === null) return metric.state
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let value = metric.value
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`
}

function formatFdUsed(application: ApplicationTelemetry): string {
  return `已用 ${formatCount(application.fdUsed)}`
}

function formatFdSoftLimit(application: ApplicationTelemetry): string {
  return `软限制 ${formatCount(application.fdSoftLimit)} · ${formatPercent(application.fdPercentOfSoftLimit)}`
}

function formatCount(metric: MetricValue): string {
  return metric.state === 'known' && metric.value !== null ? String(metric.value) : metric.state
}

function metricSortValue(metric: MetricValue): number | null {
  return metric.state === 'known' ? metric.value : null
}

function resourceSortValue(application: ApplicationTelemetry, key: ResourceSortKey): number | string | null {
  if (key === 'application') return application.displayLabel
  if (key === 'cpu') return metricSortValue(application.cpuPercentTotalCapacity)
  if (key === 'memory') return metricSortValue(application.rssBytes)
  if (key === 'processes') return application.processCount
  if (key === 'fileDescriptors') return metricSortValue(application.fdUsed)
  return application.groupingResolution === 'unknown' ? null : application.groupingResolution
}

function compareSortValues(
  left: number | string | null,
  right: number | string | null,
  direction: SortDirection,
): number {
  if (left === null && right === null) return 0
  if (left === null) return 1
  if (right === null) return -1
  const comparison = typeof left === 'number' && typeof right === 'number'
    ? left - right
    : String(left).localeCompare(String(right), 'zh-CN', { numeric: true, sensitivity: 'base' })
  return direction === 'ascending' ? comparison : -comparison
}

function toggleResourceSort(key: ResourceSortKey): void {
  if (resourceSortKey.value === key) {
    resourceSortDirection.value = resourceSortDirection.value === 'ascending' ? 'descending' : 'ascending'
    return
  }
  resourceSortKey.value = key
  resourceSortDirection.value = 'ascending'
}

function resourceAriaSort(key: ResourceSortKey): SortDirection | 'none' {
  return resourceSortKey.value === key ? resourceSortDirection.value : 'none'
}

function resourceSortButtonLabel(key: ResourceSortKey, label: string): string {
  if (resourceSortKey.value !== key) return `按${label}升序排序`
  return resourceSortDirection.value === 'ascending'
    ? `按${label}降序排序`
    : `按${label}升序排序`
}

function formatTimestamp(value: number | null | undefined, includeDate = false): string {
  if (value === null || value === undefined) return 'unknown'
  return new Intl.DateTimeFormat('zh-CN', includeDate
    ? { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }
    : { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(new Date(value))
}

function formatDuration(durationNs: number): string {
  const totalSeconds = Math.floor(durationNs / 1_000_000_000)
  const hours = Math.floor(totalSeconds / 3_600)
  const minutes = Math.floor((totalSeconds % 3_600) / 60)
  const seconds = totalSeconds % 60
  if (hours > 0) {
    return seconds > 0
      ? `${hours} 小时 ${minutes} 分钟 ${seconds} 秒`
      : `${hours} 小时 ${minutes} 分钟`
  }
  if (minutes > 0) return `${minutes} 分钟 ${seconds} 秒`
  return `${totalSeconds} 秒`
}

function formatUsageShare(application: UsageApplicationDuration): string {
  return totalDurationNs.value > 0
    ? `${((application.durationNs / totalDurationNs.value) * 100).toFixed(1)}%`
    : 'unknown'
}

function formatUsageDefinition(definition: UsageSummary['coverage']['definition'] | undefined): string {
  return definition === 'foreground_unlocked_input_active_300s_monotonic'
    ? '窗口处于前台聚焦、会话已解锁，且最近 5 分钟内有输入'
    : '定义未知'
}

function formatBucketCoverage(covered: boolean | undefined): string {
  if (covered === true) return '已覆盖完整周期起点'
  if (covered === false) return '仅包含统计开始后的记录'
  return '覆盖范围未知'
}

function statusIcon(status: BackendStatus) {
  if (status === 'healthy') return CheckCircle2
  if (status === 'degraded') return CircleAlert
  if (status === 'unsupported') return CircleOff
  return CircleHelp
}

async function refreshResources(): Promise<void> {
  if (resourceRefreshing.value) return
  const generation = ++resourceGeneration
  resourceRefreshing.value = true
  if (!snapshot.value) resourceState.value = 'loading'
  const [health, telemetry] = await Promise.all([
    getBackendHealth(),
    getTelemetrySnapshot(),
  ])
  if (!active || generation !== resourceGeneration) return
  backendHealth.value = health
  if (telemetry.kind === 'snapshot') {
    snapshot.value = telemetry.snapshot
    resourceError.value = null
    resourceState.value = 'snapshot'
  } else {
    resourceError.value = telemetry.error
    resourceState.value = snapshot.value ? 'snapshot' : 'error'
  }
  resourceRefreshing.value = false
}

async function refreshUsage(): Promise<void> {
  const generation = ++usageGeneration
  refreshing.value = true
  if (!usageSummary.value) usageState.value = 'loading'
  const result = await getUsageSummary({
    period: usagePeriod.value,
    bucketKey: selectedBucketKey.value,
  })
  if (!active || generation !== usageGeneration) return
  if (result.kind === 'summary') {
    usageSummary.value = result.summary
    usageError.value = null
    usageState.value = 'summary'
  } else {
    usageSummary.value = null
    usageError.value = result.error
    usageState.value = 'error'
  }
  refreshing.value = false
}

function selectPanel(panel: ApplicationPanel): void {
  activePanel.value = panel
  if (panel === 'usage' && usageState.value === 'idle') void refreshUsage()
}

function onPanelKeydown(event: KeyboardEvent): void {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const panels: ApplicationPanel[] = ['resources', 'usage']
  const current = panels.indexOf(activePanel.value)
  const next = event.key === 'Home'
    ? 0
    : event.key === 'End'
      ? panels.length - 1
      : (current + (event.key === 'ArrowRight' ? 1 : -1) + panels.length) % panels.length
  selectPanel(panels[next])
  requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>(`[data-application-panel="${panels[next]}"]`)?.focus()
  })
}

function selectUsagePeriod(period: UsagePeriod): void {
  if (usagePeriod.value === period) return
  usagePeriod.value = period
  usageSummary.value = null
  void refreshUsage()
}

function onUsagePeriodKeydown(event: KeyboardEvent): void {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const periods: UsagePeriod[] = ['daily', 'weekly']
  const current = periods.indexOf(usagePeriod.value)
  const next = event.key === 'Home'
    ? 0
    : event.key === 'End'
      ? periods.length - 1
      : (current + (event.key === 'ArrowRight' ? 1 : -1) + periods.length) % periods.length
  selectUsagePeriod(periods[next])
  requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>(`[data-usage-period="${periods[next]}"]`)?.focus()
  })
}

function moveUsageBucket(direction: -1 | 1): void {
  if (direction > 0 && isCurrentBucket.value) return
  const next = new Date(selectedDate.value)
  next.setDate(next.getDate() + direction * (usagePeriod.value === 'daily' ? 1 : 7))
  selectedDate.value = startOfLocalDay(next)
  usageSummary.value = null
  void refreshUsage()
}

function refreshActivePanel(): void {
  if (activePanel.value === 'resources') void refreshResources()
  else void refreshUsage()
}

function autoRefreshResources(): void {
  if (!active || activePanel.value !== 'resources' || resourceRefreshing.value) return
  void refreshResources()
}

function autoRefreshUsage(): void {
  if (!active) return
  const followedCurrentBucket = isCurrentBucket.value
  const nextCurrentDate = startOfLocalDay(new Date())
  if (nextCurrentDate.getTime() !== currentDate.value.getTime()) {
    currentDate.value = nextCurrentDate
    if (followedCurrentBucket) selectedDate.value = startOfLocalDay(nextCurrentDate)
  }
  if (activePanel.value !== 'usage' || !isCurrentBucket.value || refreshing.value) return
  void refreshUsage()
}

onMounted(() => {
  refreshActivePanel()
  resourceRefreshTimer = window.setInterval(autoRefreshResources, RESOURCE_REFRESH_INTERVAL_MS)
  usageRefreshTimer = window.setInterval(autoRefreshUsage, USAGE_REFRESH_INTERVAL_MS)
})
onBeforeUnmount(() => {
  active = false
  if (resourceRefreshTimer !== null) window.clearInterval(resourceRefreshTimer)
  if (usageRefreshTimer !== null) window.clearInterval(usageRefreshTimer)
  resourceGeneration += 1
  usageGeneration += 1
})
</script>

<template>
  <section class="applications-console" aria-labelledby="applications-heading">
    <h1 id="applications-heading" class="sr-only">应用</h1>

    <div class="applications-tabs" role="tablist" aria-label="应用视图" @keydown="onPanelKeydown">
      <button
        class="applications-tab"
        :class="{ 'is-active': activePanel === 'resources' }"
        type="button"
        role="tab"
        data-application-panel="resources"
        :aria-selected="activePanel === 'resources'"
        :tabindex="activePanel === 'resources' ? 0 : -1"
        @click="selectPanel('resources')"
      >
        <AppWindow :size="18" aria-hidden="true" /><span>资源</span>
      </button>
      <button
        class="applications-tab"
        :class="{ 'is-active': activePanel === 'usage' }"
        type="button"
        role="tab"
        data-application-panel="usage"
        :aria-selected="activePanel === 'usage'"
        :tabindex="activePanel === 'usage' ? 0 : -1"
        @click="selectPanel('usage')"
      >
        <Clock3 :size="18" aria-hidden="true" /><span>使用时间</span>
      </button>
      <button
        class="icon-button applications-refresh"
        type="button"
        :aria-label="activePanel === 'resources' ? '刷新应用资源' : '刷新使用时间'"
        :title="activePanel === 'resources' ? '刷新应用资源' : '刷新使用时间'"
        :disabled="activeRefreshing"
        @click="refreshActivePanel"
      >
        <RefreshCw :size="16" :class="{ 'is-spinning': activeRefreshing }" aria-hidden="true" />
      </button>
    </div>

    <div v-if="activePanel === 'resources'" class="telemetry-layout applications-resource-layout">
      <section class="telemetry-workspace" aria-labelledby="resource-table-heading">
        <div class="telemetry-section-heading">
          <div>
            <h2 id="resource-table-heading">应用聚合</h2>
            <p v-if="snapshot" class="telemetry-meta">
              {{ applications.length }} 项 · {{ snapshot.scope }} · {{ formatTimestamp(snapshot.capturedAtUnixMs) }}
            </p>
          </div>
          <button
            v-if="snapshot"
            class="freshness-label"
            :class="`is-${snapshot.freshness}`"
            type="button"
            :aria-label="`数据新鲜度 ${snapshot.freshness}，刷新应用资源`"
            title="刷新应用资源"
            :disabled="resourceRefreshing"
            @click="refreshResources"
          >
            <RefreshCw :size="13" :class="{ 'is-spinning': resourceRefreshing }" aria-hidden="true" />
            <span>{{ snapshot.freshness }}</span>
          </button>
        </div>

        <div v-if="resourceError && snapshot" class="telemetry-refresh-error" role="status">
          <CircleAlert :size="16" aria-hidden="true" />
          <span>刷新失败，正在显示上一次成功数据</span>
          <code>{{ resourceError.reason }}</code>
        </div>
        <div v-if="resourceState === 'loading'" class="telemetry-state" aria-live="polite">
          <RefreshCw :size="30" class="is-spinning" aria-hidden="true" /><strong>正在读取应用资源</strong>
        </div>
        <div v-else-if="resourceState === 'error'" class="telemetry-state is-error" role="status">
          <CircleAlert :size="32" aria-hidden="true" /><strong>应用资源不可用</strong><code>{{ resourceError?.reason }}</code>
          <button class="quiet-action" type="button" @click="refreshResources"><RefreshCw :size="15" aria-hidden="true" /><span>重试</span></button>
        </div>
        <div v-else-if="applications.length === 0" class="telemetry-state">
          <Inbox :size="34" aria-hidden="true" /><strong>暂无可用数据</strong><code>{{ snapshot?.reason }}</code>
        </div>
        <div v-else class="telemetry-table-wrap">
          <table class="telemetry-table">
            <colgroup>
              <col class="telemetry-column-application" />
              <col class="telemetry-column-cpu" />
              <col class="telemetry-column-memory" />
              <col class="telemetry-column-processes" />
              <col class="telemetry-column-file-descriptors" />
              <col class="telemetry-column-grouping" />
            </colgroup>
            <thead>
              <tr>
                <th
                  v-for="column in resourceSortColumns"
                  :key="column.key"
                  scope="col"
                  :aria-sort="resourceAriaSort(column.key)"
                >
                  <button
                    class="telemetry-sort-button"
                    :class="{ 'is-active': resourceSortKey === column.key }"
                    type="button"
                    :data-sort-key="column.key"
                    :aria-label="resourceSortButtonLabel(column.key, column.label)"
                    :title="resourceSortButtonLabel(column.key, column.label)"
                    @click="toggleResourceSort(column.key)"
                  >
                    <span>{{ column.label }}</span>
                    <ArrowUp v-if="resourceSortKey === column.key && resourceSortDirection === 'ascending'" :size="15" aria-hidden="true" />
                    <ArrowDown v-else-if="resourceSortKey === column.key" :size="15" aria-hidden="true" />
                    <ArrowUpDown v-else :size="15" aria-hidden="true" />
                  </button>
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="application in sortedApplications" :key="application.applicationKey">
                <th scope="row"><span class="application-label">{{ application.displayLabel }}</span><code :title="application.applicationKey">{{ application.applicationKey }}</code></th>
                <td><span class="metric-primary">proc {{ formatPercent(application.cpuPercentTotalCapacity) }}</span><small>cgroup {{ formatPercent(application.cgroupCpuPercentTotalCapacity) }}</small></td>
                <td><span class="metric-primary">RSS {{ formatBytes(application.rssBytes) }}</span><small>PSS {{ formatBytes(application.pssBytes) }}</small><small>cgroup {{ formatBytes(application.memoryCurrentBytes) }}</small></td>
                <td><span class="metric-primary">{{ application.processCount }} same-EUID</span><small>{{ formatCount(application.cgroupProcessCount) }} full-cgroup</small></td>
                <td>
                  <span class="metric-primary">{{ formatFdUsed(application) }}</span>
                  <small class="fd-soft-limit" :title="formatFdSoftLimit(application)">{{ formatFdSoftLimit(application) }}</small>
                  <small>归因 {{ formatPercent(application.fdPercentOfAttributed) }}</small>
                </td>
                <td><span class="grouping-label">{{ application.groupingResolution }}</span><small>{{ application.processScope }} / {{ application.cgroupScope }}</small></td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <aside class="capability-inspector" aria-labelledby="resource-inspector-heading">
        <div class="inspector-heading"><p class="eyebrow">请求范围</p><h2 id="resource-inspector-heading">能力检查器</h2></div>
        <dl class="inspector-list">
          <div class="inspector-row"><dt>appd.health.v1</dt><dd :class="`is-${backendHealth.status}`">{{ backendHealth.status }}</dd><code>{{ backendHealth.capabilityReason }}</code></div>
          <div class="inspector-row"><dt>telemetry.snapshot.v1</dt><dd :class="`is-${telemetryAvailability}`">{{ telemetryAvailability }}</dd><code>{{ resourceError?.reason ?? snapshot?.reason ?? 'pending' }}</code></div>
          <div class="inspector-row compact"><dt>状态</dt><dd>{{ snapshot?.status ?? resourceError?.kind ?? 'loading' }}</dd></div>
          <div class="inspector-row compact"><dt>新鲜度</dt><dd>{{ snapshot?.freshness ?? 'unknown' }}</dd></div>
          <div class="inspector-row compact"><dt>last success</dt><dd>{{ formatTimestamp(snapshot?.lastSuccessAtUnixMs) }}</dd></div>
          <div class="inspector-row compact"><dt>retryable</dt><dd>{{ resourceError?.retryable ?? snapshot?.retryable ?? false }}</dd></div>
          <div class="inspector-row compact"><dt>scope</dt><dd>{{ snapshot?.scope ?? 'unknown' }}</dd></div>
          <div class="inspector-row compact"><dt>系统 FD</dt><dd v-if="snapshot">{{ formatCount(snapshot.systemFd.fileNrAllocated) }} / {{ formatCount(snapshot.systemFd.fileMax) }}</dd><dd v-else>unknown</dd><code>{{ snapshot ? formatPercent(snapshot.systemFd.pressurePercent) : 'unknown' }}</code></div>
        </dl>
      </aside>
    </div>

    <div v-else class="usage-panel">
      <div class="usage-layout">
        <main class="usage-workspace" aria-live="polite">
          <div class="usage-controls">
            <div class="usage-period-control" role="tablist" aria-label="使用时间周期" @keydown="onUsagePeriodKeydown">
              <button type="button" role="tab" data-usage-period="daily" :class="{ 'is-active': usagePeriod === 'daily' }" :aria-selected="usagePeriod === 'daily'" :tabindex="usagePeriod === 'daily' ? 0 : -1" @click="selectUsagePeriod('daily')">每日</button>
              <button type="button" role="tab" data-usage-period="weekly" :class="{ 'is-active': usagePeriod === 'weekly' }" :aria-selected="usagePeriod === 'weekly'" :tabindex="usagePeriod === 'weekly' ? 0 : -1" @click="selectUsagePeriod('weekly')">每周</button>
            </div>
            <div class="usage-date-control">
              <button class="icon-button" type="button" aria-label="上一个时间段" title="上一个时间段" @click="moveUsageBucket(-1)"><ChevronLeft :size="18" aria-hidden="true" /></button>
              <span><CalendarDays :size="18" aria-hidden="true" /><strong>{{ selectedBucketLabel }}</strong></span>
              <button class="icon-button" type="button" aria-label="下一个时间段" title="下一个时间段" :disabled="isCurrentBucket" @click="moveUsageBucket(1)"><ChevronRight :size="18" aria-hidden="true" /></button>
            </div>
          </div>

          <div class="usage-table-wrap">
            <table class="usage-table">
              <thead><tr><th scope="col">应用</th><th scope="col">前台时长</th><th scope="col">已记录占比</th><th scope="col">最后前台</th><th scope="col">状态</th></tr></thead>
              <tbody v-if="usageApplications.length">
                <tr v-for="application in usageApplications" :key="`${application.appId}:${application.timezoneId}:${application.utcOffsetSeconds}`">
                  <th scope="row"><code :title="application.appId">{{ application.appId }}</code><small>{{ application.timezoneId }} · UTC{{ application.utcOffsetSeconds >= 0 ? '+' : '' }}{{ application.utcOffsetSeconds / 3600 }}</small></th>
                  <td>{{ formatDuration(application.durationNs) }}</td>
                  <td>{{ formatUsageShare(application) }}</td>
                  <td>{{ formatTimestamp(application.lastWallUtcMs, true) }}</td>
                  <td><span :class="`is-${usageSummary?.status ?? 'degraded'}`">{{ usageSummary?.status ?? 'unknown' }}</span></td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-if="usageState === 'loading'" class="usage-state"><LoaderCircle :size="42" class="is-spinning" aria-hidden="true" /><strong>正在读取使用时间</strong></div>
          <div v-else-if="usageState === 'error'" class="usage-state is-error" role="status"><CircleAlert :size="42" aria-hidden="true" /><strong>使用时间不可用</strong><code>{{ usageError?.reason }}</code><button class="quiet-action" type="button" @click="refreshUsage"><RefreshCw :size="15" aria-hidden="true" /><span>重试</span></button></div>
          <div v-else-if="usageApplications.length === 0" class="usage-state is-empty"><Clock3 :size="48" aria-hidden="true" /><strong>等待使用时间记录</strong><code>{{ usageSummary?.reason ?? 'usage_tracking_pending' }}</code></div>
        </main>

        <aside class="usage-inspector" aria-labelledby="usage-inspector-heading">
          <h2 id="usage-inspector-heading">使用时间能力</h2>
          <dl class="usage-facts">
            <div class="usage-fact-row"><dt><component :is="statusIcon(usageAvailability)" :size="19" aria-hidden="true" />tracking</dt><dd :class="`is-${usageAvailability}`">{{ usageAvailability }}</dd><code>{{ usageError?.reason ?? usageSummary?.reason ?? 'usage_tracking_pending' }}</code></div>
            <div class="usage-fact-row"><dt><Radio :size="19" aria-hidden="true" />niri event stream</dt><dd :class="`is-${usageSummary?.coverage.niriEventStreamConnected ? 'healthy' : (usageSummary?.coverage.status ?? 'degraded')}`">{{ usageSummary?.coverage.niriEventStreamConnected ? 'healthy' : (usageSummary?.coverage.status ?? 'unknown') }}</dd><code>{{ usageSummary?.coverage.reason ?? 'coverage_unknown' }}</code></div>
            <div class="usage-fact-row"><dt><UserRoundCheck :size="19" aria-hidden="true" />logind session</dt><dd :class="`is-${usageSummary?.coverage.logindSessionAvailable ? 'healthy' : (usageSummary?.coverage.status ?? 'degraded')}`">{{ usageSummary?.coverage.logindSessionAvailable ? 'healthy' : (usageSummary?.coverage.status ?? 'unknown') }}</dd><code>{{ usageSummary?.coverage.reason ?? 'coverage_unknown' }}</code></div>
            <div class="usage-fact-row"><dt><CircleHelp :size="19" aria-hidden="true" />coverage</dt><dd :class="`is-${usageSummary?.coverage.status ?? 'degraded'}`">{{ usageSummary?.coverage.status ?? 'unknown' }}</dd><code>{{ usageSummary?.coverage.reason ?? 'coverage_unknown' }}</code></div>
            <div class="usage-fact-row"><dt><CalendarDays :size="19" aria-hidden="true" />统计始于</dt><dd>{{ formatTimestamp(usageSummary?.coverage.trackingStartedUnixMs, true) }}</dd><code>{{ formatBucketCoverage(usageSummary?.coverage.bucketStartCovered) }}</code></div>
          </dl>
        </aside>
      </div>

      <footer class="usage-status-strip" aria-label="使用时间能力摘要">
        <div class="usage-status-item"><Clock3 :size="17" :class="`is-${usageAvailability}`" aria-hidden="true" /><span>tracking</span><strong :class="`is-${usageAvailability}`">{{ usageAvailability }}</strong><code>{{ usageError?.reason ?? usageSummary?.reason ?? 'usage_tracking_pending' }}</code></div>
        <div class="usage-status-item"><CircleHelp :size="17" :class="`is-${usageSummary?.coverage.status ?? 'degraded'}`" aria-hidden="true" /><span>coverage</span><strong :class="`is-${usageSummary?.coverage.status ?? 'degraded'}`">{{ usageSummary?.coverage.status ?? 'unknown' }}</strong><code>{{ usageSummary?.coverage.reason ?? 'coverage_unknown' }}</code></div>
        <div class="usage-status-item definition"><BookOpenText :size="17" aria-hidden="true" /><span>计时口径</span><code>{{ formatUsageDefinition(usageSummary?.coverage.definition) }}</code></div>
      </footer>
    </div>
  </section>
</template>
