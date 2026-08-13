<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RefreshCw } from 'lucide-vue-next'

import { getBackendCapabilityReport } from '../backend'
import CapabilityRow from '../components/CapabilityRow.vue'
import type { BackendCapability, BridgeError } from '../types'
import type { RouteLocationRaw } from 'vue-router'

const capabilityDefinitions: Record<string, { label: string; detail: string }> = {
  'appd.health.v1': { label: 'appd 服务', detail: '本地同 UID IPC 与运行状态' },
  'telemetry.snapshot.v1': { label: '应用资源', detail: 'CPU、内存与文件句柄' },
  'network.system.v1': { label: '系统网络', detail: '接口、计数器与实时速率' },
  'network.per_app.v1': { label: '按应用流量', detail: '各应用收发流量与占比' },
  'usage.foreground.v1': { label: '使用时间', detail: '每日与每周前台使用时长' },
  'remote.ssh.v1': { label: 'SSH', detail: 'OpenSSH 终端与主机信任' },
  'remote.sftp.v1': { label: 'SFTP', detail: '基于 SSH 的远程文件操作' },
  'remote.ftp.v1': { label: 'FTP / FTPS', detail: '进入 FTP；显式 TLS 可在远程连接中切换' },
  'remote.smb.v1': { label: 'SMB2/3', detail: 'SMB 文件能力与诊断边界' },
  'transfers.v1': { label: '传输队列', detail: '可恢复上传、下载与冲突处理' },
  'notes.v1': { label: '备忘录', detail: '日记和列表共享的本地实体' },
}

const capabilityRoutes: Record<string, RouteLocationRaw> = {
  'telemetry.snapshot.v1': { path: '/applications', query: { panel: 'resources' } },
  'network.system.v1': { path: '/network', query: { tab: 'interfaces' } },
  'network.per_app.v1': { path: '/network', query: { tab: 'applications' } },
  'usage.foreground.v1': { path: '/applications', query: { panel: 'usage' } },
  'remote.ssh.v1': { path: '/remote', query: { protocol: 'ssh' } },
  'remote.sftp.v1': { path: '/remote', query: { protocol: 'sftp' } },
  'remote.ftp.v1': { path: '/remote', query: { protocol: 'ftp' } },
  'remote.smb.v1': { path: '/remote', query: { protocol: 'smb' } },
  'transfers.v1': { path: '/transfers' },
  'notes.v1': { path: '/memos' },
}

const capabilities = ref<BackendCapability[]>([])
const catalogError = ref<BridgeError | null>(null)
const loadingCatalog = ref(false)

const catalogHeading = computed(() => {
  if (loadingCatalog.value) {
    return capabilities.value.length > 0 ? '正在刷新能力目录' : '正在读取后端能力'
  }
  if (catalogError.value && capabilities.value.length > 0) return '能力目录刷新失败'
  if (catalogError.value) return '能力目录不可用'
  return '实时能力目录'
})

function definition(capability: BackendCapability) {
  return capabilityDefinitions[capability.id] ?? {
    label: capability.id,
    detail: '后端声明的运行能力',
  }
}

async function refreshCatalog() {
  if (loadingCatalog.value) return
  loadingCatalog.value = true
  const result = await getBackendCapabilityReport()
  if (result.kind === 'report') {
    capabilities.value = result.report.capabilities
    catalogError.value = null
  } else {
    catalogError.value = result.error
  }
  loadingCatalog.value = false
}

onMounted(refreshCatalog)
</script>

<template>
  <section class="art-dashboard" aria-labelledby="dashboard-heading">
    <h1 id="dashboard-heading" class="sr-only">仪表盘</h1>

    <div class="dashboard-title" aria-label="CONPUTER STATUS CONSOLE">
      <span class="dashboard-title-block">CONPUTER</span>
      <span class="dashboard-title-block is-serif">STATUS</span>
      <span class="dashboard-title-block">CONSOLE</span>
    </div>
    <p class="dashboard-caption">ULTRAMARINE CONTROL FIELD</p>

    <div class="capability-heading">
      <div>
        <span>[ LIVE CAPABILITY DIRECTORY ]</span>
        <strong>{{ catalogHeading }}</strong>
      </div>
      <div class="capability-heading-actions">
        <span>{{ capabilities.length }} 项</span>
        <button
          class="dashboard-refresh"
          type="button"
          aria-label="刷新能力目录"
          title="刷新能力目录"
          :disabled="loadingCatalog"
          @click="refreshCatalog"
        >
          <RefreshCw :class="{ 'is-spinning': loadingCatalog }" :size="18" aria-hidden="true" />
        </button>
      </div>
    </div>

    <div
      class="capability-list"
      :class="{ 'has-many-capabilities': capabilities.length > 5 }"
      :aria-busy="loadingCatalog"
      aria-live="polite"
    >
      <CapabilityRow
        v-for="capability in capabilities"
        :key="capability.id"
        :label="definition(capability).label"
        :detail="definition(capability).detail"
        :status="capability.status"
        :reason="capability.reason"
        :to="capabilityRoutes[capability.id]"
      />
      <CapabilityRow
        v-if="catalogError"
        class="catalog-error-row"
        label="能力目录"
        detail="无法读取 appd 返回的 capability catalog"
        status="unreachable"
        :reason="catalogError.reason"
      />
    </div>
  </section>
</template>
