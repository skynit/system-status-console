<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import {
  ArrowDown,
  ArrowDownUp,
  ArrowUp,
  CheckCircle2,
  CircleAlert,
  CircleOff,
  Clock3,
  LoaderCircle,
  Plus,
  RefreshCw,
  ShieldCheck,
  Unplug,
  X,
} from 'lucide-vue-next'

import {
  cancelTransfer,
  enqueueTransfer,
  getBackendHealth,
  getRemoteProfiles,
  listTransfers,
  pickDownloadDestination,
  pickUploadSource,
  resolveTransferConflict,
  retryTransfer,
} from '../backend'
import type {
  BackendHealth,
  BackendStatus,
  BridgeError,
  ConflictPolicy,
  StoredRemoteProfile,
  TransferDirection,
  TransferDraft,
  TransferLocalHandleGrant,
  TransferMutationFetchResult,
  TransferPage,
  TransferState,
  TransferTask,
} from '../types'

type DirectionFilter = 'all' | TransferDirection
type PageState = 'loading' | 'page' | 'error'
type ProfileState = 'idle' | 'loading' | 'ready' | 'error'

function initialTransferRequest(): { direction: TransferDirection; profileId: string; path: string } | null {
  const query = window.location.hash.split('?', 2)[1]
  if (!query) return null
  const parameters = new URLSearchParams(query)
  const direction = parameters.get('direction')
  const profileId = parameters.get('profile')
  const path = parameters.get('path')
  if ((direction !== 'upload' && direction !== 'download') || !profileId || path === null) return null
  return { direction, profileId, path }
}

const initialRequest = initialTransferRequest()

const PAGE_LIMIT = 16
const TRANSFER_REFRESH_INTERVAL_MS = 1_000
const directionFilters: Array<{ id: DirectionFilter; label: string; icon: typeof ArrowDownUp }> = [
  { id: 'all', label: '全部', icon: ArrowDownUp },
  { id: 'upload', label: '上传', icon: ArrowUp },
  { id: 'download', label: '下载', icon: ArrowDown },
]

const health = ref<BackendHealth>({ status: 'unsupported', capabilityReason: 'health_not_requested' })
const pageState = ref<PageState>('loading')
const page = ref<TransferPage | null>(null)
const pageError = ref<BridgeError | null>(null)
const refreshing = ref(false)
const directionFilter = ref<DirectionFilter>('all')
const offsetHistory = ref<number[]>([])
const profiles = ref<StoredRemoteProfile[]>([])
const profileState = ref<ProfileState>('idle')
const profileError = ref<BridgeError | null>(null)
const mutationError = ref<BridgeError | null>(null)
const mutationNotice = ref<string | null>(null)
const busyTaskIds = ref<string[]>([])
const createOpen = ref(initialRequest !== null)
const createDirection = ref<TransferDirection>(initialRequest?.direction ?? 'upload')
const selectedProfileId = ref<string | null>(initialRequest?.profileId ?? null)
const remotePath = ref(initialRequest?.path ?? '')
const localGrant = ref<TransferLocalHandleGrant | null>(null)
const autoFilledRemotePath = ref<string | null>(null)
const formError = ref<string | null>(null)
const createBusy = ref(false)
const createDirty = ref(initialRequest !== null)
let active = true
let requestGeneration = 0
let createGeneration = 0
let mutatingCreateForm = false
let refreshTimer: number | null = null

const tasks = computed(() => page.value?.tasks ?? [])
const hasActiveTransfers = computed(() => tasks.value.some((task) =>
  ['queued', 'running', 'pausing', 'retry_scheduled', 'conflict'].includes(task.state.status),
))
const visibleTasks = computed(() => tasks.value)
const transferProfiles = computed(() => profiles.value.filter((stored) => (
  stored.profile.protocol === 'sftp'
    || stored.profile.protocol === 'ftp'
    || stored.profile.protocol === 'ftps_explicit'
    || stored.profile.protocol === 'smb'
)))
const selectedProfile = computed(() => transferProfiles.value.find((stored) => (
  stored.profile.id === selectedProfileId.value
)) ?? null)
const queueMutationBusy = computed(() => busyTaskIds.value.length > 0)
const runnerStatus = computed<BackendStatus>(() => {
  if (health.value.status === 'unsupported' || health.value.status === 'unreachable') return health.value.status
  if (pageError.value?.kind === 'transport') return 'unreachable'
  if (pageError.value) return 'degraded'
  return page.value ? 'healthy' : 'degraded'
})
const runnerReason = computed(() => pageError.value?.reason ?? health.value.capabilityReason)
const localHandleStatus = computed<BackendStatus>(() => {
  if (health.value.status === 'unsupported' || health.value.status === 'unreachable') return health.value.status
  return 'degraded'
})
const localHandleReason = computed(() => health.value.status === 'healthy' || health.value.status === 'degraded'
  ? 'portal_selection_required'
  : health.value.capabilityReason)
const endpointStatus = computed<BackendStatus>(() => {
  if (profileError.value?.kind === 'transport') return 'unreachable'
  if (profileError.value) return 'degraded'
  return transferProfiles.value.length > 0 ? 'degraded' : 'unsupported'
})
const endpointReason = computed(() => {
  if (profileError.value) return profileError.value.reason
  if (profileState.value === 'idle' || profileState.value === 'loading') return 'transfer_profiles_pending'
  return transferProfiles.value.length > 0
    ? 'profiles_available_endpoints_unverified'
    : 'transfer_profile_unavailable'
})
const transferProtocolSummary = computed(() => {
  const protocols = [...new Set(transferProfiles.value.map((stored) => stored.profile.protocol))]
  return protocols.length > 0 ? protocols.map(transferProtocolLabel).join(' / ') : 'none'
})

watch([createDirection, selectedProfileId, remotePath, localGrant], () => {
  if (createOpen.value && !mutatingCreateForm) createDirty.value = true
}, { flush: 'sync' })

function statusIcon(status: BackendStatus) {
  if (status === 'healthy') return CheckCircle2
  if (status === 'degraded') return CircleAlert
  if (status === 'unsupported') return CircleOff
  return Unplug
}

function transferProtocolLabel(protocol: StoredRemoteProfile['profile']['protocol']): string {
  return protocol === 'ftps_explicit' ? 'FTPS' : protocol.toUpperCase()
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

function formatTimestamp(value: number): string {
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(value))
}

function formatProgress(task: TransferTask): string {
  const transferred = formatBytes(task.progress.bytesTransferred)
  if (task.progress.totalBytes === null) return `${transferred} / unknown`
  const percent = task.progress.totalBytes === 0
    ? 100
    : Math.min(100, (task.progress.bytesTransferred / task.progress.totalBytes) * 100)
  return `${percent.toFixed(1)}% · ${transferred} / ${formatBytes(task.progress.totalBytes)}`
}

function formatSpeed(task: TransferTask): string {
  return task.progress.bytesPerSecond === null
    ? 'rate unknown'
    : `${formatBytes(task.progress.bytesPerSecond)}/s`
}

function stateLabel(state: TransferState): string {
  return {
    queued: '排队中',
    running: '运行中',
    pausing: '暂停中',
    paused: '已暂停',
    cancelling: '取消中',
    retry_scheduled: '等待重试',
    conflict: '冲突',
    completed: '已完成',
    failed: '失败',
    cancelled: '已取消',
  }[state.status]
}

function stateClass(state: TransferState): string {
  if (state.status === 'completed') return 'is-healthy'
  if (state.status === 'failed' || state.status === 'conflict') return 'is-danger'
  if (state.status === 'cancelled') return 'is-muted'
  return 'is-warning'
}

function stateDetail(task: TransferTask): string {
  switch (task.state.status) {
    case 'failed':
      return task.state.failure.reason
    case 'retry_scheduled':
      return `${task.state.failure.reason} · ${formatTimestamp(task.state.notBeforeUnixMs)}`
    case 'conflict':
      return task.state.conflict.reason
    default:
      return `revision ${task.revision}`
  }
}

function endpointLabel(task: TransferTask, endpoint: 'source' | 'destination'): string {
  const value = task[endpoint]
  if (value.kind === 'remote') return `${value.protocol}:${value.path}`
  return `本机句柄 ${value.handle?.slice(0, 8) ?? 'unknown'}`
}

function canCancel(task: TransferTask): boolean {
  return ['queued', 'running', 'pausing', 'paused', 'retry_scheduled', 'conflict'].includes(task.state.status)
}

function canRetry(task: TransferTask): boolean {
  return task.state.status === 'failed'
    && task.state.failure.retry !== 'never'
    && task.completedAttempts < task.retryPolicy.maxAttempts
}

function retryUnavailableLabel(task: TransferTask): string | null {
  if (task.state.status !== 'failed') return null
  if (task.completedAttempts >= task.retryPolicy.maxAttempts) return '重试次数已用完'
  if (task.state.failure.retry === 'never') return '不可重试'
  return null
}

function canResumeConflict(task: TransferTask): boolean {
  return task.state.status === 'conflict' && task.features.resume.status === 'supported'
}

function selectDirectionFilter(filter: DirectionFilter): void {
  if (refreshing.value || createBusy.value || queueMutationBusy.value || directionFilter.value === filter) return
  directionFilter.value = filter
  offsetHistory.value = []
  void showPage(0)
}

function onFilterKeydown(event: KeyboardEvent): void {
  if (refreshing.value || createBusy.value || queueMutationBusy.value) return
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const current = directionFilters.findIndex((filter) => filter.id === directionFilter.value)
  const next = event.key === 'Home'
    ? 0
    : event.key === 'End'
      ? directionFilters.length - 1
      : (current + (event.key === 'ArrowRight' ? 1 : -1) + directionFilters.length) % directionFilters.length
  selectDirectionFilter(directionFilters[next].id)
  requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>(`[data-transfer-filter="${directionFilter.value}"]`)?.focus()
  })
}

function autoRefreshTransfers(): void {
  if (!active || !hasActiveTransfers.value || refreshing.value) return
  void refresh()
}

async function refresh(): Promise<void> {
  if (refreshing.value || createBusy.value || queueMutationBusy.value) return
  const generation = ++requestGeneration
  refreshing.value = true
  if (!page.value) pageState.value = 'loading'
  const [nextHealth, result] = await Promise.all([
    getBackendHealth(),
    listTransfers({
      limit: PAGE_LIMIT,
      offset: page.value?.query.offset ?? 0,
      states: [],
      direction: directionFilter.value === 'all' ? null : directionFilter.value,
      profileId: null,
    }),
  ])
  if (!active || generation !== requestGeneration) return
  health.value = nextHealth
  refreshing.value = false
  if (result.kind === 'page') {
    page.value = result.page
    pageError.value = null
    pageState.value = 'page'
  } else {
    pageError.value = result.error
    if (!page.value) pageState.value = 'error'
  }
}

async function loadProfiles(): Promise<void> {
  if (profileState.value === 'loading' || profileState.value === 'ready') return
  profileState.value = 'loading'
  const result = await getRemoteProfiles()
  if (!active) return
  if (result.kind === 'data') {
    profiles.value = result.data.profiles
    profileError.value = null
    profileState.value = 'ready'
    mutatingCreateForm = true
    selectedProfileId.value ??= transferProfiles.value[0]?.profile.id ?? null
    mutatingCreateForm = false
  } else {
    profiles.value = []
    profileError.value = result.error
    profileState.value = 'error'
  }
}

async function showPage(offset: number): Promise<boolean> {
  if (createBusy.value || queueMutationBusy.value) return false
  const generation = ++requestGeneration
  const direction = directionFilter.value === 'all' ? null : directionFilter.value
  const canRetainPage = page.value !== null && page.value.query.direction === direction
  refreshing.value = true
  if (!canRetainPage) {
    page.value = null
    pageState.value = 'loading'
  }
  const result = await listTransfers({
    limit: PAGE_LIMIT,
    offset,
    states: [],
    direction,
    profileId: null,
  })
  if (!active || generation !== requestGeneration) return false
  refreshing.value = false
  if (result.kind === 'page') {
    page.value = result.page
    pageError.value = null
    pageState.value = 'page'
    return true
  } else {
    pageError.value = result.error
    pageState.value = canRetainPage ? 'page' : 'error'
    return false
  }
}

async function nextPage(): Promise<void> {
  if (refreshing.value || createBusy.value || queueMutationBusy.value || !page.value?.hasMore || page.value.nextOffset === null) return
  const currentOffset = page.value.query.offset
  if (await showPage(page.value.nextOffset)) offsetHistory.value.push(currentOffset)
}

async function previousPage(): Promise<void> {
  if (refreshing.value || createBusy.value || queueMutationBusy.value) return
  const previous = offsetHistory.value.at(-1)
  if (previous !== undefined && await showPage(previous)) offsetHistory.value.pop()
}

function replaceTask(task: TransferTask): void {
  if (!page.value) return
  page.value = {
    ...page.value,
    tasks: page.value.tasks.map((item) => item.id === task.id ? task : item),
  }
}

async function runMutation(task: TransferTask, action: () => Promise<TransferMutationFetchResult>): Promise<void> {
  if (refreshing.value || createBusy.value || busyTaskIds.value.includes(task.id)) return
  busyTaskIds.value = [...busyTaskIds.value, task.id]
  mutationError.value = null
  mutationNotice.value = null
  const result = await action()
  busyTaskIds.value = busyTaskIds.value.filter((id) => id !== task.id)
  if (!active) return
  if (result.kind === 'error') {
    mutationError.value = result.error
    return
  }
  if (result.result.result === 'updated') {
    replaceTask(result.result.task)
    mutationNotice.value = 'transfer_updated'
  } else {
    mutationError.value = {
      kind: 'daemon',
      code: 'transfer_revision_conflict',
      reason: 'transfer_revision_conflict',
      retryable: true,
    }
    replaceTask(result.result.current)
  }
}

function cancelTask(task: TransferTask): void {
  void runMutation(task, () => cancelTransfer(task.id, task.revision))
}

function retryTask(task: TransferTask): void {
  void runMutation(task, () => retryTransfer(task.id, task.revision))
}

function resolveConflict(task: TransferTask, policy: ConflictPolicy): void {
  if (
    policy === 'overwrite'
    && !window.confirm(`覆盖会替换目标“${endpointLabel(task, 'destination')}”。确定继续吗？`)
  ) return
  void runMutation(task, () => resolveTransferConflict(task.id, task.revision, policy))
}

function openCreate(): void {
  if (refreshing.value || queueMutationBusy.value || createOpen.value) return
  createGeneration += 1
  createBusy.value = false
  createOpen.value = true
  createDirty.value = false
  formError.value = null
  void loadProfiles()
}

function resetCreateDraft(): void {
  mutatingCreateForm = true
  createDirection.value = 'upload'
  selectedProfileId.value = transferProfiles.value[0]?.profile.id ?? null
  remotePath.value = ''
  localGrant.value = null
  autoFilledRemotePath.value = null
  createDirty.value = false
  mutatingCreateForm = false
}

function closeCreate(): void {
  if (!confirmDiscardCreate()) return
  createGeneration += 1
  createBusy.value = false
  createOpen.value = false
  resetCreateDraft()
  formError.value = null
}

function selectCreateDirection(direction: TransferDirection, select?: HTMLSelectElement): void {
  if (createDirection.value === direction) return
  if (localGrant.value && !window.confirm('切换传输方向会清除已选择的本机文件，确定继续吗？')) {
    if (select) select.value = createDirection.value
    return
  }
  createDirection.value = direction
  localGrant.value = null
  autoFilledRemotePath.value = null
  formError.value = null
}

function markRemotePathEdited(): void {
  autoFilledRemotePath.value = null
}

function confirmDiscardCreate(): boolean {
  if (createBusy.value) return false
  return !createDirty.value || window.confirm('当前传输任务还有未提交的内容，确定放弃吗？')
}

function handleBeforeUnload(event: BeforeUnloadEvent): void {
  if (!createDirty.value) return
  event.preventDefault()
  event.returnValue = ''
}

function createTransferId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = bytes[6] & 0x0f | 0x40
  bytes[8] = bytes[8] & 0x3f | 0x80
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, '0'))
  return `${hex.slice(0, 4).join('')}-${hex.slice(4, 6).join('')}-${hex.slice(6, 8).join('')}-${hex.slice(8, 10).join('')}-${hex.slice(10, 16).join('')}`
}

async function chooseLocalHandle(): Promise<void> {
  const generation = createGeneration
  const direction = createDirection.value
  createBusy.value = true
  formError.value = null
  const result = direction === 'upload'
    ? await pickUploadSource()
    : await pickDownloadDestination()
  if (!active || generation !== createGeneration || !createOpen.value || createDirection.value !== direction) return
  createBusy.value = false
  if (result.kind === 'error') {
    formError.value = result.error.reason
    return
  }
  if (result.grant === null) return
  const previousAutoFilledPath = autoFilledRemotePath.value
  localGrant.value = result.grant
  if (direction === 'upload') {
    if (remotePath.value.endsWith('/')) {
      remotePath.value = `${remotePath.value}${result.grant.displayName}`
      autoFilledRemotePath.value = remotePath.value
    } else if (previousAutoFilledPath !== null && remotePath.value === previousAutoFilledPath) {
      const separator = remotePath.value.lastIndexOf('/')
      remotePath.value = `${remotePath.value.slice(0, separator + 1)}${result.grant.displayName}`
      autoFilledRemotePath.value = remotePath.value
    }
  }
}

function validRemotePathInput(value: string): boolean {
  const trimmed = value.trim()
  return trimmed.length > 0
    && !trimmed.includes('\0')
    && new TextEncoder().encode(trimmed).length <= 8 * 1024
}

function transferFormErrorMessage(reason: string): string {
  return reason === 'local_file_permission_denied'
    ? '无权限：无法读取所选本机文件'
    : reason
}

async function submitCreate(): Promise<void> {
  if (refreshing.value || createBusy.value || queueMutationBusy.value) return
  if (!selectedProfile.value) {
    formError.value = 'transfer_profile_required'
    return
  }
  if (!validRemotePathInput(remotePath.value)) {
    formError.value = 'transfer_remote_path_invalid'
    return
  }
  if (!localGrant.value) {
    formError.value = 'transfer_local_handle_required'
    return
  }
  createBusy.value = true
  formError.value = null
  const path = remotePath.value.trim()
  const profileId = selectedProfile.value.profile.id
  const grant = localGrant.value
  const expectedSource = createDirection.value === 'upload' && grant.sizeBytes !== null
    ? { sizeBytes: grant.sizeBytes, modifiedAtUnixMs: null, etag: null }
    : null
  const draft: TransferDraft = {
    id: createTransferId(),
    source: createDirection.value === 'upload'
      ? { kind: 'local', handle: grant.handle }
      : { kind: 'remote', profileId, path },
    destination: createDirection.value === 'upload'
      ? { kind: 'remote', profileId, path }
      : { kind: 'local', handle: grant.handle },
    direction: createDirection.value,
    expectedSource,
    expectedDestination: null,
    retryPolicy: { maxAttempts: 3, initialBackoffMs: 1_000, maxBackoffMs: 30_000 },
    bandwidthLimit: null,
    conflictPolicy: 'fail',
  }
  const result = await enqueueTransfer(draft)
  createBusy.value = false
  if (!active) return
  if (result.kind === 'error') {
    formError.value = result.error.reason
    return
  }
  mutationNotice.value = 'transfer_enqueued'
  mutationError.value = null
  mutatingCreateForm = true
  createOpen.value = false
  remotePath.value = ''
  localGrant.value = null
  autoFilledRemotePath.value = null
  createDirty.value = false
  mutatingCreateForm = false
  if (await showPage(0)) offsetHistory.value = []
}

onBeforeRouteLeave(() => confirmDiscardCreate())
onMounted(() => {
  window.addEventListener('beforeunload', handleBeforeUnload)
  void refresh()
  void loadProfiles()
  refreshTimer = window.setInterval(autoRefreshTransfers, TRANSFER_REFRESH_INTERVAL_MS)
})
onBeforeUnmount(() => {
  active = false
  requestGeneration += 1
  createGeneration += 1
  if (refreshTimer !== null) window.clearInterval(refreshTimer)
  window.removeEventListener('beforeunload', handleBeforeUnload)
})
</script>

<template>
  <section class="transfers-console" aria-labelledby="transfers-heading">
    <h1 id="transfers-heading" class="sr-only">传输队列</h1>

    <div class="transfers-layout">
      <main class="transfers-workspace" aria-live="polite">
        <div class="transfers-toolbar">
          <div class="transfer-direction-filter" role="tablist" aria-label="传输方向筛选" @keydown="onFilterKeydown">
            <button
              v-for="filter in directionFilters"
              :key="filter.id"
              type="button"
              role="tab"
              class="transfer-filter-button"
              :class="{ 'is-active': directionFilter === filter.id }"
              :data-transfer-filter="filter.id"
              :aria-selected="directionFilter === filter.id"
              :tabindex="directionFilter === filter.id ? 0 : -1"
              :disabled="refreshing || createBusy || queueMutationBusy"
              @click="selectDirectionFilter(filter.id)"
            >
              <component :is="filter.icon" :size="16" aria-hidden="true" />
              <span>{{ filter.label }}</span>
            </button>
          </div>
          <div class="transfer-toolbar-actions">
            <button class="transfer-primary-button" type="button" :disabled="pageState === 'error' || refreshing || createOpen || createBusy || queueMutationBusy" @click="openCreate">
              <Plus :size="16" aria-hidden="true" />
              <span>新建传输</span>
            </button>
            <button class="transfer-secondary-button" type="button" :disabled="refreshing || createBusy || queueMutationBusy" @click="refresh">
              <RefreshCw :size="16" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
              <span>刷新</span>
            </button>
          </div>
        </div>

        <form v-if="createOpen" class="transfer-create-form" aria-labelledby="transfer-create-heading" @submit.prevent="submitCreate">
          <div class="transfer-create-heading">
            <div>
              <strong id="transfer-create-heading">新建传输</strong>
              <span>本机文件只通过系统选择器授予 opaque handle</span>
            </div>
            <button class="icon-button compact-button" type="button" aria-label="关闭新建传输" title="关闭" :disabled="createBusy" @click="closeCreate">
              <X :size="15" aria-hidden="true" />
            </button>
          </div>

          <div class="transfer-create-grid">
            <label>
              <span>方向</span>
              <select :value="createDirection" :disabled="createBusy" @change="selectCreateDirection(($event.target as HTMLSelectElement).value as TransferDirection, $event.target as HTMLSelectElement)">
                <option value="upload">上传</option>
                <option value="download">下载</option>
              </select>
            </label>
            <label>
              <span>远端配置</span>
              <select v-model="selectedProfileId" :disabled="createBusy || profileState === 'loading' || transferProfiles.length === 0">
                <option v-if="transferProfiles.length === 0" :value="null">暂无可用远程文件配置</option>
                <option v-for="stored in transferProfiles" :key="stored.profile.id" :value="stored.profile.id">
                  {{ transferProtocolLabel(stored.profile.protocol) }} · {{ stored.profile.label }} · {{ stored.profile.endpoint.host }}:{{ stored.profile.endpoint.port }}
                </option>
              </select>
            </label>
            <label>
              <span>远端路径</span>
              <input v-model="remotePath" type="text" autocomplete="off" spellcheck="false" placeholder="/remote/path/file" :disabled="createBusy" @input="markRemotePathEdited">
            </label>
            <div class="transfer-local-picker">
              <span>本机{{ createDirection === 'upload' ? '源文件' : '目标文件' }}</span>
              <button class="transfer-secondary-button" type="button" :disabled="createBusy || health.status === 'unsupported'" @click="chooseLocalHandle">
                <ShieldCheck :size="16" aria-hidden="true" />
                <span>{{ localGrant ? localGrant.displayName : '通过系统选择器选择' }}</span>
              </button>
            </div>
          </div>

          <p v-if="profileError" class="transfer-form-error"><code>{{ profileError.reason }}</code></p>
          <p v-if="formError" class="transfer-form-error" role="alert">
            <span>{{ transferFormErrorMessage(formError) }}</span>
            <code v-if="transferFormErrorMessage(formError) !== formError">{{ formError }}</code>
          </p>
          <div class="transfer-create-actions">
            <button class="transfer-primary-button" type="submit" :disabled="refreshing || createBusy || queueMutationBusy || profileState === 'loading'">
              <LoaderCircle v-if="createBusy" :size="16" class="is-spinning" aria-hidden="true" />
              <Plus v-else :size="16" aria-hidden="true" />
              <span>加入队列</span>
            </button>
          </div>
        </form>

        <div v-if="pageState === 'error'" class="transfer-state is-error" role="status">
          <Unplug :size="42" aria-hidden="true" />
          <strong>传输队列不可用</strong>
          <code>{{ pageError?.reason }}</code>
          <button class="transfer-secondary-button" type="button" :disabled="refreshing || createBusy || queueMutationBusy" @click="refresh">
            <RefreshCw :size="15" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <template v-else>
          <div v-if="pageError && page" class="transfer-refresh-error" role="status">
            <CircleAlert :size="16" aria-hidden="true" />
            <span>请求失败，正在显示上一次成功结果</span>
            <code>{{ pageError.reason }}</code>
          </div>
          <div class="transfer-table-wrap">
            <table class="transfer-table">
              <thead>
                <tr>
                  <th scope="col">方向</th>
                  <th scope="col">状态</th>
                  <th scope="col">进度</th>
                  <th scope="col">源</th>
                  <th scope="col">目标</th>
                  <th scope="col">更新时间</th>
                  <th scope="col">操作</th>
                </tr>
              </thead>
              <tbody v-if="visibleTasks.length">
                <tr v-for="task in visibleTasks" :key="task.id">
                  <td>
                    <span class="transfer-direction" :class="`is-${task.direction}`">
                      <component :is="task.direction === 'upload' ? ArrowUp : ArrowDown" :size="15" aria-hidden="true" />
                      {{ task.direction === 'upload' ? '上传' : '下载' }}
                    </span>
                  </td>
                  <td>
                    <span class="transfer-state-label" :class="stateClass(task.state)">{{ stateLabel(task.state) }}</span>
                    <small>{{ stateDetail(task) }}</small>
                  </td>
                  <td>
                    <span>{{ formatProgress(task) }}</span>
                    <small>{{ formatSpeed(task) }}</small>
                  </td>
                  <td><code :title="endpointLabel(task, 'source')">{{ endpointLabel(task, 'source') }}</code></td>
                  <td><code :title="endpointLabel(task, 'destination')">{{ endpointLabel(task, 'destination') }}</code></td>
                  <td>{{ formatTimestamp(task.updatedAtMs) }}</td>
                  <td>
                    <div class="transfer-row-actions">
                      <button v-if="canCancel(task)" class="transfer-secondary-button compact-action" type="button" :disabled="refreshing || busyTaskIds.includes(task.id)" @click="cancelTask(task)">取消</button>
                      <button v-if="canRetry(task)" class="transfer-secondary-button compact-action" type="button" :disabled="refreshing || busyTaskIds.includes(task.id)" @click="retryTask(task)">重试</button>
                      <small v-else-if="retryUnavailableLabel(task)" class="transfer-action-status">{{ retryUnavailableLabel(task) }}</small>
                      <template v-if="task.state.status === 'conflict'">
                        <button v-if="canResumeConflict(task)" class="transfer-secondary-button compact-action" type="button" :disabled="refreshing || busyTaskIds.includes(task.id)" @click="resolveConflict(task, 'resume')">继续</button>
                        <button class="transfer-secondary-button compact-action" type="button" :disabled="refreshing || busyTaskIds.includes(task.id)" @click="resolveConflict(task, 'rename')">重命名</button>
                        <button class="transfer-secondary-button compact-action danger-action" type="button" :disabled="refreshing || busyTaskIds.includes(task.id)" @click="resolveConflict(task, 'overwrite')">覆盖</button>
                      </template>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-if="pageState === 'loading'" class="transfer-state">
            <LoaderCircle :size="42" class="is-spinning" aria-hidden="true" />
            <strong>正在读取传输队列</strong>
          </div>
          <div v-else-if="visibleTasks.length === 0" class="transfer-state is-empty">
            <ArrowDownUp :size="42" aria-hidden="true" />
            <strong>{{ directionFilter === 'all' ? '暂无传输任务' : '当前方向暂无任务' }}</strong>
            <code>transfer_queue_empty</code>
          </div>

          <div v-if="page" class="transfer-pagination">
            <button class="transfer-secondary-button compact-action" type="button" :disabled="offsetHistory.length === 0 || refreshing || createBusy || queueMutationBusy" @click="previousPage">上一页</button>
            <span>{{ page.query.offset + 1 }} - {{ page.query.offset + page.tasks.length }}</span>
            <button class="transfer-secondary-button compact-action" type="button" :disabled="!page.hasMore || refreshing || createBusy || queueMutationBusy" @click="nextPage">下一页</button>
          </div>
        </template>

        <div v-if="mutationError || mutationNotice" class="transfer-operation-message" :class="{ 'is-error': mutationError }" role="status">
          <CircleAlert v-if="mutationError" :size="16" aria-hidden="true" />
          <CheckCircle2 v-else :size="16" aria-hidden="true" />
          <code>{{ mutationError?.reason ?? mutationNotice }}</code>
          <button class="icon-button compact-button" type="button" aria-label="关闭传输消息" title="关闭" @click="mutationError = null; mutationNotice = null">
            <X :size="14" aria-hidden="true" />
          </button>
        </div>
      </main>

      <aside class="transfers-inspector" aria-labelledby="transfers-inspector-heading">
        <h2 id="transfers-inspector-heading">传输能力</h2>
        <dl class="transfer-facts">
          <div class="transfer-fact-row">
            <dt><component :is="statusIcon(runnerStatus)" :size="18" aria-hidden="true" />runner</dt>
            <dd :class="`is-${runnerStatus}`">{{ runnerStatus }}</dd>
            <code>{{ runnerReason }}</code>
          </div>
          <div class="transfer-fact-row">
            <dt><component :is="statusIcon(endpointStatus)" :size="18" aria-hidden="true" />controlled I/O</dt>
            <dd :class="`is-${endpointStatus}`">{{ endpointStatus }}</dd>
            <code>{{ endpointReason }}</code>
          </div>
          <div class="transfer-fact-row">
            <dt><component :is="statusIcon(localHandleStatus)" :size="18" aria-hidden="true" />local handles</dt>
            <dd :class="`is-${localHandleStatus}`">{{ localHandleStatus }}</dd>
            <code>{{ localHandleReason }}</code>
          </div>
          <div class="transfer-fact-row">
            <dt><component :is="statusIcon(endpointStatus)" :size="18" aria-hidden="true" />profiles</dt>
            <dd :class="`is-${endpointStatus}`">{{ transferProfiles.length }}</dd>
            <code>{{ transferProtocolSummary }}</code>
          </div>
        </dl>
      </aside>
    </div>

    <footer class="transfers-status-strip" aria-label="传输能力摘要">
      <div class="transfer-status-item">
        <component :is="statusIcon(runnerStatus)" :size="16" :class="`is-${runnerStatus}`" aria-hidden="true" />
        <span>runner</span>
        <strong :class="`is-${runnerStatus}`">{{ runnerStatus }}</strong>
        <code>{{ runnerReason }}</code>
      </div>
      <div class="transfer-status-item">
        <component :is="statusIcon(localHandleStatus)" :size="16" :class="`is-${localHandleStatus}`" aria-hidden="true" />
        <span>handles</span>
        <strong :class="`is-${localHandleStatus}`">{{ localHandleStatus }}</strong>
        <code>{{ localHandleReason }}</code>
      </div>
      <div class="transfer-status-item compact">
        <Clock3 :size="16" aria-hidden="true" />
        <span>schema</span>
        <code>transfer_v11</code>
      </div>
    </footer>
  </section>
</template>
