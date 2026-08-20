<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  ChevronRight,
  CircleOff,
  Clock3,
  Database,
  Gauge,
  Info,
  RefreshCw,
  SlidersHorizontal,
  SquareTerminal,
} from 'lucide-vue-next'

import { getBackendCapabilityReport, getSystemInfo, getUsageSummary } from '../backend'
import type {
  BackendCapability,
  BackendStatus,
  BridgeError,
  SystemInfoReport,
  UsageSummary,
} from '../types'

type GroupState = 'loading' | 'ready' | 'error'
type SettingsTab = 'apps' | 'system'
type AppsSettingsSection = 'system' | 'remote' | 'data' | 'usage' | 'configuration'

interface CapabilityDefinition {
  id: string
  label: string
  detail: string
}

const systemCapabilities: CapabilityDefinition[] = [
  { id: 'appd.health.v1', label: 'appd 服务', detail: '本地同 UID IPC 与运行状态' },
  { id: 'telemetry.snapshot.v1', label: '应用资源采集', detail: 'CPU、内存与文件句柄' },
  { id: 'network.system.v1', label: '系统网络', detail: '接口、计数器与实时速率' },
  { id: 'network.per_app.v1', label: '按应用流量', detail: '各应用收发流量与占比' },
  { id: 'usage.foreground.v1', label: '使用时间', detail: '每日与每周前台使用时长' },
]

const remoteCapabilities: CapabilityDefinition[] = [
  { id: 'remote.ssh.v1', label: 'SSH', detail: 'OpenSSH 终端与主机信任' },
  { id: 'remote.sftp.v1', label: 'SFTP', detail: '基于 SSH 的远程文件操作' },
  { id: 'remote.ftp.v1', label: 'FTP / FTPS', detail: '明文 FTP 需显式确认' },
  { id: 'remote.smb.v1', label: 'SMB2/3', detail: 'SMB 文件能力与诊断边界' },
]

const dataCapabilities: CapabilityDefinition[] = [
  { id: 'transfers.v1', label: '传输队列', detail: '可恢复上传、下载与冲突处理' },
  { id: 'notes.v1', label: '日志', detail: '日历和列表共享的本地日志实体' },
]

const pendingSettings = [
  { label: '采集周期', detail: '采样间隔为版本固定值' },
  { label: '数据保留期', detail: '各存储使用版本固定策略' },
  { label: '通知', detail: '应用内状态为事实来源' },
  { label: '快捷键', detail: 'Wayland 会话不提供全局快捷键' },
  { label: '隐私偏好', detail: '数据边界为版本固定事实' },
  { label: '远程连接默认值', detail: '各连接在表单中独立配置' },
]

const sectionLabels: Record<string, string> = {
  OS: '操作系统',
  Host: '主机',
  Kernel: '内核',
  Uptime: '运行时间',
  Packages: '软件包',
  Shell: 'Shell',
  WM: '窗口管理器',
  Display: '显示器',
  CPU: '处理器',
  GPU: '显卡',
  Memory: '内存',
  Swap: '交换空间',
  Disk: '磁盘',
  LocalIp: '本机地址',
  Battery: '电池',
  Locale: '区域设置',
}

const entryLabels: Record<string, string> = {
  os_name: '名称',
  os_version: '版本',
  vendor: '厂商',
  model: '型号',
  kernel: '内核',
  build: '构建',
  uptime: '运行时长',
  boot_time: '启动时间',
  packages: '软件包',
  shell: 'Shell',
  wm: 'WM',
  display: '显示器',
  cpu_name: '型号',
  cores: '核心',
  frequency: '频率',
  codename: '代号',
  device: '设备',
  mounts: '挂载',
  usage: '用量',
  created: '创建时间',
  gpu: '显卡',
  driver: '驱动',
  memory: '内存',
  swap: '交换',
  address: '地址',
  capacity: '电量',
  battery: '电池',
  locale: '区域',
}

const appsSections: Array<{
  id: AppsSettingsSection
  label: string
  detail: string
  description: string
}> = [
  { id: 'system', label: '系统状态', detail: 'appd 与采集能力', description: '查看 appd、资源采集、网络与使用时间能力。状态和 reason 直接来自本机后端。' },
  { id: 'remote', label: '远程连接', detail: 'SSH 与文件协议', description: '查看 SSH、SFTP、FTP / FTPS 与 SMB2/3 的协议能力和诊断边界。' },
  { id: 'data', label: '数据与队列', detail: '本地持久化服务', description: '查看传输队列、日志以及后端额外声明的本地数据能力。' },
  { id: 'usage', label: '使用时间口径', detail: '覆盖与计时定义', description: '查看使用时间统计的起点覆盖、桌面事件流和会话可用性。' },
  { id: 'configuration', label: '配置边界', detail: '当前未开放项', description: '以下配置项尚无后端设置契约，因此保持只读并如实标记为 unsupported。' },
]

const activeTab = ref<SettingsTab>('apps')
const activeAppsSection = ref<AppsSettingsSection>('system')

const capabilities = ref<BackendCapability[]>([])
const catalogError = ref<BridgeError | null>(null)
const groupState = ref<GroupState>('loading')

const usageSummary = ref<UsageSummary | null>(null)
const usageError = ref<BridgeError | null>(null)
const usageLoading = ref(false)

const sysReport = ref<SystemInfoReport | null>(null)
const sysError = ref<BridgeError | null>(null)
const sysLoading = ref(false)

let active = true
let catalogGeneration = 0
let usageGeneration = 0
let sysGeneration = 0

function localDateKey(value: Date): string {
  const year = value.getFullYear()
  const month = String(value.getMonth() + 1).padStart(2, '0')
  const day = String(value.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

function capabilityFor(id: string): BackendCapability | undefined {
  return capabilities.value.find((capability) => capability.id === id)
}

function groupedRows(definitions: CapabilityDefinition[]): Array<{
  definition: CapabilityDefinition
  capability: BackendCapability
}> {
  return definitions.map((definition) => ({
    definition,
    capability: capabilityFor(definition.id) ?? {
      id: definition.id,
      status: 'unsupported',
      reason: 'unknown_capability',
    },
  }))
}

const systemRows = computed(() => groupedRows(systemCapabilities))
const remoteRows = computed(() => groupedRows(remoteCapabilities))
const dataRows = computed(() => groupedRows(dataCapabilities))

const extraCapabilities = computed(() => {
  const defined = new Set([
    ...systemCapabilities,
    ...remoteCapabilities,
    ...dataCapabilities,
  ].map((definition) => definition.id))
  return capabilities.value.filter((capability) => !defined.has(capability.id))
})

const activeAppsSectionMeta = computed(() => (
  appsSections.find((section) => section.id === activeAppsSection.value) ?? appsSections[0]
))

const activeCapabilityRows = computed(() => {
  if (activeAppsSection.value === 'system') return systemRows.value
  if (activeAppsSection.value === 'remote') return remoteRows.value
  if (activeAppsSection.value === 'data') {
    return [
      ...dataRows.value,
      ...extraCapabilities.value.map((capability) => ({
        definition: {
          id: capability.id,
          label: capability.id,
          detail: '后端声明的运行能力',
        },
        capability,
      })),
    ]
  }
  return []
})

const activeSectionUsesCatalog = computed(() => (
  activeAppsSection.value === 'system'
  || activeAppsSection.value === 'remote'
  || activeAppsSection.value === 'data'
))

const usageAvailability = computed<BackendStatus>(() => {
  if (usageError.value?.kind === 'transport') return 'unreachable'
  if (usageError.value) return 'degraded'
  return usageSummary.value?.status ?? 'degraded'
})

const usageReason = computed(() => (
  usageError.value?.reason ?? usageSummary.value?.reason ?? 'usage_tracking_pending'
))

const trackingStartedLabel = computed(() => {
  const value = usageSummary.value?.coverage.trackingStartedUnixMs ?? null
  return value === null ? 'unknown' : formatTimestamp(value, true)
})

const coverageStatus = computed<BackendStatus>(() => (
  usageSummary.value?.coverage.status ?? 'degraded'
))

const sysAvailability = computed<BackendStatus>(() => {
  if (sysError.value?.kind === 'transport') return 'unreachable'
  if (sysError.value) return 'degraded'
  return sysReport.value?.status ?? 'degraded'
})

const sysReason = computed(() => (
  sysError.value?.reason ?? sysReport.value?.reason ?? 'system_info_pending'
))

const refreshing = computed(() => (
  activeTab.value === 'apps'
    ? groupState.value === 'loading' || usageLoading.value
    : sysLoading.value
))

const refreshLabel = computed(() => (
  refreshing.value
    ? (activeTab.value === 'apps' ? '正在刷新设置事实' : '正在刷新系统信息')
    : (activeTab.value === 'apps' ? '刷新设置事实' : '刷新系统信息')
))

function sectionLabel(id: string): string {
  return sectionLabels[id] ?? id
}

function entryLabel(key: string): string {
  return entryLabels[key] ?? key
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

function formatTimestamp(value: number, includeDate = false): string {
  return new Intl.DateTimeFormat('zh-CN', includeDate
    ? { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' }
    : { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(new Date(value)).replaceAll('/', '-')
}

async function refreshCatalog(): Promise<void> {
  const generation = ++catalogGeneration
  groupState.value = 'loading'
  const result = await getBackendCapabilityReport()
  if (!active || generation !== catalogGeneration) return
  if (result.kind === 'report') {
    capabilities.value = result.report.capabilities
    catalogError.value = null
    groupState.value = 'ready'
  } else {
    catalogError.value = result.error
    groupState.value = capabilities.value.length > 0 ? 'ready' : 'error'
  }
}

async function refreshUsage(): Promise<void> {
  const generation = ++usageGeneration
  usageLoading.value = true
  usageError.value = null
  const result = await getUsageSummary({
    period: 'daily',
    bucketKey: localDateKey(new Date()),
  })
  if (!active || generation !== usageGeneration) return
  usageLoading.value = false
  if (result.kind === 'summary') {
    usageSummary.value = result.summary
    usageError.value = null
  } else {
    usageSummary.value = null
    usageError.value = result.error
  }
}

async function refreshSystemInfo(): Promise<void> {
  const generation = ++sysGeneration
  sysLoading.value = true
  const result = await getSystemInfo()
  if (!active || generation !== sysGeneration) return
  sysLoading.value = false
  if (result.kind === 'systemInfo') {
    sysReport.value = result.report
    sysError.value = null
  } else {
    // Keep the last successful report for stale display, like the catalog.
    sysError.value = result.error
  }
}

function refresh(): void {
  if (activeTab.value === 'system') {
    void refreshSystemInfo()
  } else {
    void refreshCatalog()
    void refreshUsage()
  }
}

function selectTab(tab: SettingsTab): void {
  activeTab.value = tab
  if (tab === 'system' && sysReport.value === null && !sysLoading.value) {
    void refreshSystemInfo()
  }
}

function onTabKeydown(event: KeyboardEvent): void {
  const tabs: SettingsTab[] = ['apps', 'system']
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  const current = tabs.indexOf(activeTab.value)
  let next: number
  if (event.key === 'Home') {
    next = 0
  } else if (event.key === 'End') {
    next = tabs.length - 1
  } else if (event.key === 'ArrowRight') {
    next = current + 1 >= tabs.length ? 0 : current + 1
  } else {
    next = current - 1 < 0 ? tabs.length - 1 : current - 1
  }
  event.preventDefault()
  selectTab(tabs[next])
  document.querySelector<HTMLButtonElement>(`[data-settings-tab="${tabs[next]}"]`)?.focus()
}

function selectAppsSection(section: AppsSettingsSection): void {
  activeAppsSection.value = section
}

function onAppsSectionKeydown(event: KeyboardEvent): void {
  if (!['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  const sections = appsSections.map((section) => section.id)
  const current = sections.indexOf(activeAppsSection.value)
  let next: number
  if (event.key === 'Home') {
    next = 0
  } else if (event.key === 'End') {
    next = sections.length - 1
  } else if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
    next = current + 1 >= sections.length ? 0 : current + 1
  } else {
    next = current - 1 < 0 ? sections.length - 1 : current - 1
  }
  event.preventDefault()
  selectAppsSection(sections[next])
  document.querySelector<HTMLButtonElement>(`[data-settings-section="${sections[next]}"]`)?.focus()
}

onMounted(() => {
  void refreshCatalog()
  void refreshUsage()
})

onBeforeUnmount(() => {
  active = false
  catalogGeneration += 1
  usageGeneration += 1
  sysGeneration += 1
})
</script>

<template>
  <section class="settings-console" aria-labelledby="settings-heading">
    <h1 id="settings-heading" class="sr-only">设置</h1>

    <div class="settings-toolbar">
      <div class="settings-tabs" role="tablist" aria-label="设置视图" @keydown="onTabKeydown">
        <button
          id="settings-tab-apps"
          class="settings-tab"
          :class="{ 'is-active': activeTab === 'apps' }"
          type="button"
          role="tab"
          data-settings-tab="apps"
          :aria-selected="activeTab === 'apps'"
          aria-controls="settings-panel-apps"
          :tabindex="activeTab === 'apps' ? 0 : -1"
          @click="selectTab('apps')"
        >
          <SlidersHorizontal :size="18" aria-hidden="true" /><span>应用设置</span>
        </button>
        <button
          id="settings-tab-system"
          class="settings-tab"
          :class="{ 'is-active': activeTab === 'system' }"
          type="button"
          role="tab"
          data-settings-tab="system"
          :aria-selected="activeTab === 'system'"
          aria-controls="settings-panel-system"
          :tabindex="activeTab === 'system' ? 0 : -1"
          @click="selectTab('system')"
        >
          <Info :size="18" aria-hidden="true" /><span>系统信息</span>
        </button>
      </div>
      <button
        class="icon-button settings-refresh"
        type="button"
        :aria-label="refreshLabel"
        :title="refreshLabel"
        :disabled="refreshing"
        @click="refresh"
      >
        <RefreshCw :size="16" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
      </button>
    </div>

    <div class="settings-workspace">
      <div
        v-if="activeTab === 'apps'"
        id="settings-panel-apps"
        class="settings-panel settings-app-layout"
        role="tabpanel"
        aria-labelledby="settings-tab-apps"
      >
        <aside class="settings-index" aria-label="应用设置分区">
          <span class="settings-index-kicker">Settings index</span>
          <h2>应用设置</h2>
          <p>运行事实、统计口径与当前版本的配置边界。</p>

          <div
            class="settings-section-tabs"
            role="tablist"
            aria-label="应用设置分区"
            aria-orientation="vertical"
            @keydown="onAppsSectionKeydown"
          >
            <button
              v-for="(section, index) in appsSections"
              :id="`settings-section-tab-${section.id}`"
              :key="section.id"
              class="settings-section-tab"
              :class="{ 'is-active': activeAppsSection === section.id }"
              type="button"
              role="tab"
              :data-settings-section="section.id"
              :aria-selected="activeAppsSection === section.id"
              aria-controls="settings-apps-section-panel"
              :tabindex="activeAppsSection === section.id ? 0 : -1"
              @click="selectAppsSection(section.id)"
            >
              <Gauge v-if="section.id === 'system'" :size="18" aria-hidden="true" />
              <SquareTerminal v-else-if="section.id === 'remote'" :size="18" aria-hidden="true" />
              <Database v-else-if="section.id === 'data'" :size="18" aria-hidden="true" />
              <Clock3 v-else-if="section.id === 'usage'" :size="18" aria-hidden="true" />
              <CircleOff v-else :size="18" aria-hidden="true" />
              <span class="settings-section-tab-copy">
                <strong>{{ section.label }}</strong>
                <small>{{ section.detail }}</small>
              </span>
              <span class="settings-section-index" aria-hidden="true">0{{ index + 1 }}</span>
              <ChevronRight :size="16" class="settings-section-chevron" aria-hidden="true" />
            </button>
          </div>

          <div class="settings-index-note">
            <strong>只读事实</strong>
            <span>设置契约未开放前，本页不提供伪交互控件。</span>
          </div>
        </aside>

        <section
          id="settings-apps-section-panel"
          class="settings-fact-workspace"
          role="tabpanel"
          :aria-labelledby="`settings-section-tab-${activeAppsSection}`"
        >
          <header class="settings-section-heading">
            <div>
              <span class="settings-section-kicker">Settings facts</span>
              <h2>{{ activeAppsSectionMeta.label }}</h2>
              <p>{{ activeAppsSectionMeta.description }}</p>
            </div>
            <div v-if="activeSectionUsesCatalog" class="settings-status-legend" aria-label="能力状态图例">
              <span class="settings-token is-healthy">healthy</span>
              <span class="settings-token is-degraded">degraded</span>
              <span class="settings-token is-unsupported">unsupported</span>
            </div>
          </header>

          <template v-if="activeSectionUsesCatalog">
            <div v-if="groupState === 'error'" class="settings-error" role="status">
              <strong>设置事实不可用</strong>
              <code>{{ catalogError?.reason }}</code>
              <button class="settings-retry" type="button" @click="refresh">
                <RefreshCw :size="14" aria-hidden="true" />
                <span>重试</span>
              </button>
            </div>

            <template v-else>
              <div v-if="catalogError && groupState === 'ready'" class="settings-error is-stale" role="status">
                <strong>能力目录刷新失败，正在显示上一次成功数据</strong>
                <code>{{ catalogError.reason }}</code>
                <button class="settings-retry" type="button" @click="refresh">
                  <RefreshCw :size="14" aria-hidden="true" />
                  <span>重试</span>
                </button>
              </div>

              <div v-if="groupState === 'loading'" class="settings-loading" role="status">
                <RefreshCw :size="15" class="is-spinning" aria-hidden="true" />
                <span>正在读取后端能力</span>
              </div>

              <div v-else class="settings-rows" role="table" :aria-label="`${activeAppsSectionMeta.label}能力状态`">
                <div class="settings-table-head" role="row">
                  <span role="columnheader">能力</span>
                  <span role="columnheader">状态</span>
                  <span role="columnheader">Capability reason</span>
                </div>
                <div v-for="row in activeCapabilityRows" :key="row.definition.id" class="settings-row" role="row">
                  <span class="settings-row-name" role="cell">{{ row.definition.label }}<small>{{ row.definition.detail }}</small></span>
                  <span class="settings-token" :class="`is-${row.capability.status}`" role="cell">{{ row.capability.status }}</span>
                  <code class="settings-row-reason" :title="row.capability.reason" role="cell">{{ row.capability.reason }}</code>
                </div>
              </div>
            </template>

            <div class="settings-note">
              <Info :size="18" aria-hidden="true" />
              <span><strong>状态说明</strong>degraded 表示部分事实可用；进入对应功能页可查看完整 reason 与恢复路径。</span>
            </div>
          </template>

          <template v-else-if="activeAppsSection === 'usage'">
            <div v-if="usageLoading && !usageSummary" class="settings-loading" role="status">
              <RefreshCw :size="15" class="is-spinning" aria-hidden="true" />
              <span>正在读取使用时间口径</span>
            </div>
            <dl v-else class="settings-facts">
              <div class="settings-fact">
                <dt>统计始于</dt>
                <dd>{{ trackingStartedLabel }}</dd>
              </div>
              <div class="settings-fact">
                <dt>当日覆盖</dt>
                <dd>
                  <span class="settings-token" :class="`is-${coverageStatus}`">{{ coverageStatus }}</span>
                  <code :title="usageReason">{{ usageReason }}</code>
                </dd>
              </div>
              <div class="settings-fact">
                <dt>周期起点覆盖</dt>
                <dd>{{ formatBucketCoverage(usageSummary?.coverage.bucketStartCovered) }}</dd>
              </div>
              <div class="settings-fact">
                <dt>niri 事件流</dt>
                <dd>
                  <span class="settings-token" :class="usageSummary?.coverage.niriEventStreamConnected ? 'is-healthy' : 'is-degraded'">
                    {{ usageSummary?.coverage.niriEventStreamConnected ? 'connected' : 'disconnected' }}
                  </span>
                </dd>
              </div>
              <div class="settings-fact">
                <dt>logind 会话</dt>
                <dd>
                  <span class="settings-token" :class="usageSummary?.coverage.logindSessionAvailable ? 'is-healthy' : 'is-degraded'">
                    {{ usageSummary?.coverage.logindSessionAvailable ? 'available' : 'unavailable' }}
                  </span>
                </dd>
              </div>
              <div class="settings-fact settings-fact-wide">
                <dt>计时口径</dt>
                <dd>{{ formatUsageDefinition(usageSummary?.coverage.definition) }}</dd>
              </div>
              <div v-if="usageError" class="settings-fact settings-fact-wide">
                <dt>使用时间</dt>
                <dd>
                  <span class="settings-token" :class="`is-${usageAvailability}`">{{ usageAvailability }}</span>
                  <code :title="usageReason">{{ usageReason }}</code>
                </dd>
              </div>
            </dl>
            <div class="settings-note">
              <Info :size="18" aria-hidden="true" />
              <span><strong>统计边界</strong>使用时间只记录后端声明的前台、解锁与输入活跃条件，不补算统计开始前的数据。</span>
            </div>
          </template>

          <template v-else>
            <div class="settings-rows" role="table" aria-label="未开放配置项">
              <div class="settings-table-head" role="row">
                <span role="columnheader">配置项</span>
                <span role="columnheader">状态</span>
                <span role="columnheader">Capability reason</span>
              </div>
              <div v-for="item in pendingSettings" :key="item.label" class="settings-row" role="row">
                <span class="settings-row-name" role="cell">{{ item.label }}<small>{{ item.detail }}</small></span>
                <span class="settings-token is-unsupported" role="cell">unsupported</span>
                <code class="settings-row-reason" role="cell">not_implemented</code>
              </div>
            </div>
            <div class="settings-note">
              <Info :size="18" aria-hidden="true" />
              <span><strong>配置说明</strong>后端提供设置契约后，对应项目才会转为可操作控件；当前不新增权限或本地旁路配置。</span>
            </div>
          </template>
        </section>
      </div>

      <div
        v-if="activeTab === 'system'"
        id="settings-panel-system"
        class="settings-panel settings-system-workspace"
        role="tabpanel"
        aria-labelledby="settings-tab-system"
      >
        <div v-if="sysLoading && !sysReport" class="settings-loading" role="status">
          <RefreshCw :size="15" class="is-spinning" aria-hidden="true" />
          <span>正在采集系统信息</span>
        </div>

        <div v-else-if="sysError && !sysReport" class="settings-error" role="status">
          <strong>系统信息不可用</strong>
          <code>{{ sysError.reason }}</code>
          <button class="settings-retry" type="button" @click="refreshSystemInfo">
            <RefreshCw :size="14" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <template v-else>
          <div v-if="sysError && sysReport" class="settings-error is-stale" role="status">
            <strong>系统信息刷新失败，正在显示上一次成功数据</strong>
            <code>{{ sysError.reason }}</code>
            <button class="settings-retry" type="button" @click="refreshSystemInfo">
              <RefreshCw :size="14" aria-hidden="true" />
              <span>重试</span>
            </button>
          </div>

          <div v-if="sysReport && sysReport.status !== 'healthy'" class="settings-error" role="status">
            <strong>
              系统信息不可用
              <span class="settings-token" :class="`is-${sysAvailability}`">{{ sysAvailability }}</span>
            </strong>
            <code>{{ sysReason }}</code>
            <button class="settings-retry" type="button" @click="refreshSystemInfo">
              <RefreshCw :size="14" aria-hidden="true" />
              <span>重试</span>
            </button>
          </div>

          <template v-if="sysReport && sysReport.status === 'healthy'">
            <div v-if="sysReport.sections.length > 0" class="sysinfo-meta">
              <span>{{ sysReport.toolVersion ?? 'fastfetch' }}</span>
              <span>·</span>
              <span>采集于 {{ sysReport.capturedAtUnixMs === null ? 'unknown' : formatTimestamp(sysReport.capturedAtUnixMs, true) }}</span>
              <span class="settings-token is-healthy">healthy</span>
              <code>{{ sysReason }}</code>
            </div>

            <div v-if="sysReport.sections.length > 0" class="sys-pairs">
              <section v-for="section in sysReport.sections" :key="section.id" class="sys-block" :aria-label="sectionLabel(section.id)">
                <div class="sys-title">
                  <strong>{{ sectionLabel(section.id) }}</strong>
                  <small>{{ section.id }}</small>
                </div>
                <template v-for="(group, groupIndex) in section.groups" :key="groupIndex">
                  <p v-if="group.title" class="sys-group-title">{{ group.title }}</p>
                  <div v-for="entry in group.entries" :key="`${groupIndex}-${entry.key}`" class="srow">
                    <span class="srow-label">{{ entryLabel(entry.key) }}</span>
                    <span class="srow-value">{{ entry.value }}</span>
                  </div>
                </template>
              </section>
            </div>

            <div v-else class="settings-loading" role="status">
              <span>fastfetch 未返回可展示的系统事实</span>
            </div>
          </template>
        </template>
      </div>
    </div>
  </section>
</template>
