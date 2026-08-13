<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { Info, RefreshCw, SlidersHorizontal } from 'lucide-vue-next'

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
  { id: 'notes.v1', label: '备忘录', detail: '日记和列表共享的本地实体' },
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

const activeTab = ref<SettingsTab>('apps')

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
        class="settings-panel"
        role="tabpanel"
        aria-labelledby="settings-tab-apps"
      >
        <p class="settings-desc">
          查看本机控制台的运行事实与配置边界。当前版本提供状态查看；配置项接口尚未开放，未提供项如实标记。
        </p>

        <div v-if="groupState === 'error'" class="settings-error" role="status">
          <strong>设置事实不可用</strong>
          <code>{{ catalogError?.reason }}</code>
          <button class="settings-retry" type="button" @click="refresh">
            <RefreshCw :size="14" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <div v-else-if="catalogError && groupState === 'ready'" class="settings-error is-stale" role="status">
          <strong>能力目录刷新失败，正在显示上一次成功数据</strong>
          <code>{{ catalogError.reason }}</code>
          <button class="settings-retry" type="button" @click="refresh">
            <RefreshCw :size="14" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <section v-else class="settings-group" aria-label="系统状态">
          <div class="settings-group-title">
            <strong>系统状态</strong>
            <span>appd 与采集能力</span>
          </div>
          <div v-if="groupState === 'loading'" class="settings-loading" role="status">
            <RefreshCw :size="15" class="is-spinning" aria-hidden="true" />
            <span>正在读取后端能力</span>
          </div>
          <div v-else class="settings-rows">
            <div v-for="row in systemRows" :key="row.definition.id" class="settings-row">
              <span class="settings-row-name">{{ row.definition.label }}<small>{{ row.definition.detail }}</small></span>
              <span class="settings-token" :class="`is-${row.capability.status}`">{{ row.capability.status }}</span>
              <code class="settings-row-reason" :title="row.capability.reason">{{ row.capability.reason }}</code>
            </div>
          </div>
        </section>

        <section v-if="groupState === 'ready'" class="settings-group" aria-label="远程连接">
          <div class="settings-group-title">
            <strong>远程连接</strong>
            <span>SSH 终端与文件能力</span>
          </div>
          <div class="settings-rows">
            <div v-for="row in remoteRows" :key="row.definition.id" class="settings-row">
              <span class="settings-row-name">{{ row.definition.label }}<small>{{ row.definition.detail }}</small></span>
              <span class="settings-token" :class="`is-${row.capability.status}`">{{ row.capability.status }}</span>
              <code class="settings-row-reason" :title="row.capability.reason">{{ row.capability.reason }}</code>
            </div>
          </div>
        </section>

        <section v-if="groupState === 'ready'" class="settings-group" aria-label="数据与队列">
          <div class="settings-group-title">
            <strong>数据与队列</strong>
            <span>本地持久化服务</span>
          </div>
          <div class="settings-rows">
            <div v-for="row in dataRows" :key="row.definition.id" class="settings-row">
              <span class="settings-row-name">{{ row.definition.label }}<small>{{ row.definition.detail }}</small></span>
              <span class="settings-token" :class="`is-${row.capability.status}`">{{ row.capability.status }}</span>
              <code class="settings-row-reason" :title="row.capability.reason">{{ row.capability.reason }}</code>
            </div>
          </div>
          <div v-if="extraCapabilities.length > 0" class="settings-group-inner" aria-label="其他能力">
            <div v-for="capability in extraCapabilities" :key="capability.id" class="settings-row">
              <span class="settings-row-name">{{ capability.id }}<small>后端声明的运行能力</small></span>
              <span class="settings-token" :class="`is-${capability.status}`">{{ capability.status }}</span>
              <code class="settings-row-reason" :title="capability.reason">{{ capability.reason }}</code>
            </div>
          </div>
        </section>

        <section class="settings-group" aria-label="使用时间口径">
          <div class="settings-group-title">
            <strong>使用时间口径</strong>
            <span>统计事实</span>
          </div>
          <dl class="settings-facts">
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
        </section>

        <section class="settings-group" aria-label="配置项">
          <div class="settings-group-title">
            <strong>配置项</strong>
            <span>当前版本未提供配置界面</span>
          </div>
          <div class="settings-rows">
            <div v-for="item in pendingSettings" :key="item.label" class="settings-row">
              <span class="settings-row-name">{{ item.label }}<small>{{ item.detail }}</small></span>
              <span class="settings-token is-unsupported">unsupported</span>
              <code class="settings-row-reason">not_implemented</code>
            </div>
          </div>
        </section>

        <div class="settings-note">
          <strong>说明</strong>
          <span>本页所有状态与 reason 均来自后端实时事实。可配置项（采集、保留期、通知、快捷键、隐私、远程默认值）将在后端提供设置契约后分批开放，届时本页对应行转为可操作控件。</span>
        </div>
      </div>

      <div
        v-if="activeTab === 'system'"
        id="settings-panel-system"
        class="settings-panel"
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
