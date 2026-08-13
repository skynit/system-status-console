import { Channel, invoke } from '@tauri-apps/api/core'

import {
  backendStatuses,
  bridgeErrorKinds,
  groupingResolutionValues,
  metricStateValues,
  telemetryFreshnessValues,
  telemetryStatusValues,
  type ApplicationTelemetry,
  type BackendHealth,
  type BackendCapability,
  type BackendCapabilityFetchResult,
  type BackendCapabilityReport,
  type BackendStatus,
  type BridgeError,
  type GroupingResolution,
  type IssueCount,
  type MetricState,
  type MetricValue,
  type NetworkApplicationTraffic,
  type NetworkByteTotals,
  type NetworkCapabilityState,
  type NetworkCoverage,
  type NetworkFetchResult,
  type NetworkFreshness,
  type NetworkInterfaceKind,
  type NetworkInterfaceSample,
  type NetworkInterfaceTransition,
  type NetworkLayeredAccounting,
  type NetworkRate,
  type NetworkRateState,
  type NetworkSnapshot,
  type NetworkTrafficTotals,
  type ConflictPolicy,
  type LocalFileHandle,
  type ObjectIdentity,
  type RemoteErrorKind,
  type RemoteOperation,
  type RetryDisposition,
  type NoteDeletedFilter,
  type NoteDocument,
  type NoteDraftMeta,
  type NoteExport,
  type NoteExportFormat,
  type NoteExportResult,
  type NoteGetResult,
  type NoteListResult,
  type NoteMutationFetchResult,
  type NoteMutationResult,
  type NotePage,
  type NoteQuery,
  type NoteSort,
  type NoteStatus,
  type NoteSummary,
  type NoteWriteInput,
  type OpenedTerminal,
  type TransferCheckpoint,
  type TransferCompletion,
  type TransferConflict,
  type TransferDirection,
  type TransferDraft,
  type TransferEndpoint,
  type TransferFailure,
  type TransferFeatureSet,
  type TransferFeatureSupport,
  type TransferId,
  type TransferListResult,
  type TransferLocalHandleGrant,
  type TransferLocalHandlePurpose,
  type TransferMutationFetchResult,
  type TransferMutationResult,
  type TransferPage,
  type TransferPickResult,
  type TransferProgress,
  type TransferQuery,
  type TransferRetryPolicy,
  type TransferState,
  type TransferStateKind,
  type TransferTask,
  type TransferTaskResult,
  type VerificationLevel,
  type RemoteAdapterCatalog,
  type RemoteAdapterDescriptor,
  type RemoteAuthentication,
  type RemoteCapabilityStatus,
  type RemoteConnectionProfile,
  type RemoteDirectoryPage,
  type RemoteEntry,
  type RemoteFetchResult,
  type RemoteOperationCapability,
  type RemoteProfileOptions,
  type RemoteProfilePage,
  type RemoteProtocol,
  type RemoteSecretKind,
  type RemoteSecretReference,
  type RemoteSession,
  type RemoteTrustPolicy,
  type StoredRemoteProfile,
  type SystemFdTelemetry,
  type TerminalCapabilities,
  type TerminalRead,
  type TerminalSize,
  type TerminalStatus,
  type TerminalStreamEvent,
  type TelemetryFetchResult,
  type TelemetryFreshness,
  type TelemetrySnapshot,
  type TelemetryStatus,
  type UsageApplicationDuration,
  type UsageCoverage,
  type UsageFetchResult,
  type UsagePeriod,
  type UsageSummary,
  type UsageSummaryQuery,
  fileOperations,
  networkFreshnessValues,
  networkInterfaceKindValues,
  networkInterfaceTransitionValues,
  networkLayeredAccountingValues,
  networkRateStateValues,
  conflictPolicies,
  localHandlePurposes,
  noteDeletedFilters,
  noteExportFormats,
  noteSorts,
  noteStatuses,
  remoteErrorKinds,
  remoteOperations,
  resumeValidationValues,
  retryDispositions,
  transferDirections,
  transferStateKinds,
  verificationLevels,
  remoteProtocols,
  remoteSecretKinds,
  usagePeriods,
} from './types'

const MAX_REMOTE_SECRET_BYTES = 8 * 1024
const APPD_HEALTH_CAPABILITY_ID = 'appd.health.v1'

function isDesktopBridgeAvailable(): boolean {
  return Boolean(window.__TAURI_INTERNALS__ || window.__TAURI__)
}

function isBackendStatus(value: unknown): value is BackendStatus {
  return typeof value === 'string' && backendStatuses.includes(value as BackendStatus)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isEnumValue<T extends readonly string[]>(values: T, value: unknown): value is T[number] {
  return typeof value === 'string' && values.includes(value as T[number])
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value)
}

function nullableNumber(value: unknown): number | null | undefined {
  return value === null || isFiniteNumber(value) ? value : undefined
}

function nullableNonNegativeInteger(value: unknown): number | null | undefined {
  return value === null || isNonNegativeInteger(value) ? value : undefined
}

function nullableSafeInteger(value: unknown): number | null | undefined {
  return value === null || isSafeInteger(value) ? value : undefined
}

function nullableString(value: unknown): string | null | undefined {
  return value === null || typeof value === 'string' ? value : undefined
}

function normalizeHealth(payload: unknown): BackendHealth {
  const report = normalizeBackendCapabilityReport(payload)
  if (!report) {
    return { status: 'degraded', capabilityReason: 'invalid_health_response' }
  }
  const appd = report.capabilities.find((capability) => capability.id === APPD_HEALTH_CAPABILITY_ID)
  if (!appd) return { status: 'degraded', capabilityReason: 'invalid_health_response' }
  return {
    status: appd.status,
    capabilityReason: appd.reason,
  }
}

export async function getBackendHealth(): Promise<BackendHealth> {
  if (!isDesktopBridgeAvailable()) {
    return { status: 'unsupported', capabilityReason: 'desktop_bridge_unavailable' }
  }

  try {
    const payload = await invoke<unknown>('appd_health')
    return normalizeHealth(payload)
  } catch (error) {
    const bridgeError = normalizeBridgeError(error, 'appd_health_unreachable')
    return { status: 'unreachable', capabilityReason: bridgeError.reason }
  }
}

function normalizeBackendCapability(value: unknown): BackendCapability | null {
  if (
    !isRecord(value)
    || typeof value.id !== 'string'
    || value.id.length === 0
    || !isBackendStatus(value.status)
    || typeof value.reason !== 'string'
    || value.reason.length === 0
  ) return null
  return { id: value.id, status: value.status, reason: value.reason }
}

function normalizeBackendCapabilityReport(value: unknown): BackendCapabilityReport | null {
  if (
    !isRecord(value)
    || typeof value.daemon_version !== 'string'
    || value.daemon_version.length === 0
    || !isBackendStatus(value.health)
    || typeof value.reason !== 'string'
    || value.reason.length === 0
    || !Array.isArray(value.capabilities)
    || value.capabilities.length === 0
    || value.capabilities.length > 32
  ) return null

  const capabilities = value.capabilities.map(normalizeBackendCapability)
  if (!capabilities.every((capability): capability is BackendCapability => capability !== null)) {
    return null
  }
  if (new Set(capabilities.map((capability) => capability.id)).size !== capabilities.length) {
    return null
  }

  return {
    daemonVersion: value.daemon_version,
    health: { status: value.health, capabilityReason: value.reason },
    capabilities,
  }
}

export async function getBackendCapabilityReport(): Promise<BackendCapabilityFetchResult> {
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }

  try {
    const report = normalizeBackendCapabilityReport(await invoke<unknown>('appd_health'))
    return report
      ? { kind: 'report', report }
      : {
          kind: 'error',
          error: {
            kind: 'protocol',
            code: 'invalid_capability_report',
            reason: 'invalid_capability_report',
            retryable: false,
          },
        }
  } catch (error) {
    return {
      kind: 'error',
      error: normalizeBridgeError(error, 'appd_health_unreachable'),
    }
  }
}

function normalizeBridgeError(value: unknown, fallbackCode: string): BridgeError {
  if (isRecord(value)) {
    const { kind, code, reason, retryable } = value
    if (
      isEnumValue(bridgeErrorKinds, kind) &&
      typeof code === 'string' &&
      code.length > 0 &&
      typeof reason === 'string' &&
      reason.length > 0 &&
      typeof retryable === 'boolean'
    ) {
      return { kind, code, reason, retryable }
    }
  }
  if (typeof value === 'string' && value.trim().length > 0) {
    return {
      kind: 'transport',
      code: fallbackCode,
      reason: value,
      retryable: true,
    }
  }
  return {
    kind: 'transport',
    code: fallbackCode,
    reason: fallbackCode,
    retryable: true,
  }
}

function normalizeMetric(value: unknown): MetricValue | null {
  if (!isRecord(value) || !isEnumValue(metricStateValues, value.state)) return null
  const state = value.state as MetricState
  const metricValue = nullableNumber(value.value)
  const reason = value.reason === undefined ? null : nullableString(value.reason)
  if (metricValue === undefined || reason === undefined) return null
  if ((state === 'known') !== (metricValue !== null) || (metricValue ?? 0) < 0) return null
  return { value: metricValue, state, reason }
}

function normalizeIssue(value: unknown): IssueCount | null {
  if (!isRecord(value) || typeof value.code !== 'string' || value.code.length === 0) return null
  if (!isNonNegativeInteger(value.count)) return null
  return { code: value.code, count: value.count }
}

function normalizeIssueList(value: unknown): IssueCount[] | null {
  if (!Array.isArray(value)) return null
  const issues = value.map(normalizeIssue)
  return issues.every((issue): issue is IssueCount => issue !== null) ? issues : null
}

function normalizeApplication(value: unknown): ApplicationTelemetry | null {
  if (!isRecord(value)) return null
  const desktopEntryId =
    value.desktop_entry_id === undefined ? null : nullableString(value.desktop_entry_id)
  const groupingResolution = isEnumValue(groupingResolutionValues, value.grouping_resolution)
    ? (value.grouping_resolution as GroupingResolution)
    : null
  const cpu = normalizeMetric(value.cpu_percent_total_capacity_sum)
  const cgroupCpu = normalizeMetric(value.cgroup_cpu_percent_total_capacity)
  const rss = normalizeMetric(value.rss_sum_bytes)
  const pss = normalizeMetric(value.pss_sum_bytes)
  const memoryCurrent = normalizeMetric(value.memory_current_bytes)
  const cgroupProcessCount = normalizeMetric(value.cgroup_process_count)
  const fdUsed = normalizeMetric(value.fd_used_sum)
  const fdLimit = normalizeMetric(value.fd_soft_limit_sum)
  const fdAttributed = normalizeMetric(value.fd_percent_of_attributed_sum)
  const fdPercent = normalizeMetric(value.fd_percent_of_soft_limit_sum)
  const fdMaxProcessPercent = normalizeMetric(value.fd_max_process_percent_of_soft_limit)
  if (
    typeof value.application_key !== 'string' ||
    value.application_key.length === 0 ||
    desktopEntryId === undefined ||
    typeof value.display_label !== 'string' ||
    value.display_label.length === 0 ||
    groupingResolution === null ||
    !isNonNegativeInteger(value.process_count) ||
    typeof value.process_scope !== 'string' ||
    value.process_scope.length === 0 ||
    typeof value.cgroup_scope !== 'string' ||
    value.cgroup_scope.length === 0 ||
    !cpu ||
    !cgroupCpu ||
    !rss ||
    !pss ||
    !memoryCurrent ||
    !cgroupProcessCount ||
    !fdUsed ||
    !fdLimit ||
    !fdAttributed ||
    !fdPercent ||
    !fdMaxProcessPercent
  ) {
    return null
  }
  return {
    applicationKey: value.application_key,
    desktopEntryId,
    displayLabel: value.display_label,
    groupingResolution,
    processCount: value.process_count,
    processScope: value.process_scope,
    cgroupScope: value.cgroup_scope,
    cpuPercentTotalCapacity: cpu,
    cgroupCpuPercentTotalCapacity: cgroupCpu,
    rssBytes: rss,
    pssBytes: pss,
    memoryCurrentBytes: memoryCurrent,
    cgroupProcessCount,
    fdUsed,
    fdSoftLimit: fdLimit,
    fdPercentOfAttributed: fdAttributed,
    fdPercentOfSoftLimit: fdPercent,
    fdMaxProcessPercentOfSoftLimit: fdMaxProcessPercent,
  }
}

function normalizeSystemFd(value: unknown): SystemFdTelemetry | null {
  if (!isRecord(value) || typeof value.scope !== 'string' || value.scope.length === 0) return null
  const fileNrAllocated = normalizeMetric(value.file_nr_allocated)
  const fileNrMax = normalizeMetric(value.file_nr_max)
  const fileMax = normalizeMetric(value.file_max)
  const pressurePercent = normalizeMetric(value.pressure_percent)
  if (!fileNrAllocated || !fileNrMax || !fileMax || !pressurePercent) return null
  return { scope: value.scope, fileNrAllocated, fileNrMax, fileMax, pressurePercent }
}

function normalizeTelemetrySnapshot(value: unknown): TelemetrySnapshot | null {
  if (!isRecord(value) || value.schema_version !== 4) return null
  const capturedAtUnixMs = nullableNumber(value.captured_at_unix_ms)
  const sampleIntervalMs = nullableNonNegativeInteger(value.sample_interval_ms)
  const logicalCpuCount = nullableNonNegativeInteger(value.logical_cpu_count)
  const lastSuccessAtUnixMs = nullableNumber(value.last_success_at_unix_ms)
  const freshness = isEnumValue(telemetryFreshnessValues, value.freshness)
    ? (value.freshness as TelemetryFreshness)
    : null
  const status = isEnumValue(telemetryStatusValues, value.status)
    ? (value.status as TelemetryStatus)
    : null
  const permissionDeniedCounts = normalizeIssueList(value.permission_denied_counts)
  const issues = normalizeIssueList(value.issues)
  const systemFd = normalizeSystemFd(value.system_fd)
  const applications = Array.isArray(value.applications)
    ? value.applications.map(normalizeApplication)
    : null
  if (
    typeof value.snapshot_id !== 'string' ||
    value.snapshot_id.length === 0 ||
    capturedAtUnixMs === undefined ||
    sampleIntervalMs === undefined ||
    logicalCpuCount === undefined ||
    freshness === null ||
    status === null ||
    typeof value.reason !== 'string' ||
    value.reason.length === 0 ||
    typeof value.retryable !== 'boolean' ||
    typeof value.scope !== 'string' ||
    value.scope.length === 0 ||
    lastSuccessAtUnixMs === undefined ||
    permissionDeniedCounts === null ||
    issues === null ||
    systemFd === null ||
    applications === null ||
    !applications.every((application): application is ApplicationTelemetry => application !== null)
  ) {
    return null
  }
  return {
    schemaVersion: 4,
    snapshotId: value.snapshot_id,
    capturedAtUnixMs,
    sampleIntervalMs,
    logicalCpuCount,
    freshness,
    status,
    reason: value.reason,
    retryable: value.retryable,
    scope: value.scope,
    lastSuccessAtUnixMs,
    permissionDeniedCounts,
    issues,
    systemFd,
    applications,
  }
}

export async function getTelemetrySnapshot(): Promise<TelemetryFetchResult> {
  if (!isDesktopBridgeAvailable()) {
    return {
      kind: 'error',
      error: normalizeBridgeError(null, 'desktop_bridge_unavailable'),
    }
  }

  try {
    const payload = await invoke<unknown>('telemetry_snapshot')
    const snapshot = normalizeTelemetrySnapshot(payload)
    return snapshot
      ? { kind: 'snapshot', snapshot }
      : {
          kind: 'error',
          error: {
            kind: 'protocol',
            code: 'invalid_telemetry_response',
            reason: 'invalid_telemetry_response',
            retryable: false,
          },
        }
  } catch (error) {
    return {
      kind: 'error',
      error: normalizeBridgeError(error, 'telemetry_snapshot_unreachable'),
    }
  }
}

function isUuid(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)
}

function normalizeNetworkCapability(value: unknown): NetworkCapabilityState | null {
  if (!isRecord(value) || !isEnumValue(backendStatuses, value.status)) return null
  return typeof value.reason === 'string' && value.reason.length > 0
    ? { status: value.status, reason: value.reason }
    : null
}

function normalizeNetworkByteTotals(value: unknown): NetworkByteTotals | null {
  return isRecord(value)
    && isNonNegativeInteger(value.rx_bytes)
    && isNonNegativeInteger(value.tx_bytes)
    ? { rxBytes: value.rx_bytes, txBytes: value.tx_bytes }
    : null
}

function normalizeNetworkRate(value: unknown): NetworkRate | null {
  if (!isRecord(value) || !isEnumValue(networkRateStateValues, value.state)) return null
  const rxBytesPerSecond = nullableNumber(value.rx_bytes_per_second)
  const txBytesPerSecond = nullableNumber(value.tx_bytes_per_second)
  if (
    rxBytesPerSecond === undefined
    || txBytesPerSecond === undefined
    || typeof value.reason !== 'string'
    || value.reason.length === 0
    || (rxBytesPerSecond !== null && rxBytesPerSecond < 0)
    || (txBytesPerSecond !== null && txBytesPerSecond < 0)
  ) return null
  const state = value.state as NetworkRateState
  const knownShape = state === 'known'
    ? rxBytesPerSecond !== null && txBytesPerSecond !== null
    : rxBytesPerSecond === null && txBytesPerSecond === null
  return knownShape ? { rxBytesPerSecond, txBytesPerSecond, state, reason: value.reason } : null
}

function normalizeNetworkInterface(value: unknown): NetworkInterfaceSample | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.index)
    || typeof value.name !== 'string'
    || value.name.length === 0
    || new TextEncoder().encode(value.name).length > 256
    || value.name.includes('\0')
    || !isEnumValue(networkInterfaceKindValues, value.kind)
    || typeof value.is_up !== 'boolean'
    || typeof value.carrier_up !== 'boolean'
    || !isEnumValue(networkInterfaceTransitionValues, value.transition)
  ) return null
  const kernelKind = nullableString(value.kernel_kind)
  const counters = value.counters === null ? null : normalizeNetworkByteTotals(value.counters)
  const rate = normalizeNetworkRate(value.rate)
  if (kernelKind === undefined || (value.counters !== null && !counters) || !rate) return null
  return {
    index: value.index,
    name: value.name,
    kind: value.kind as NetworkInterfaceKind,
    kernelKind,
    isUp: value.is_up,
    carrierUp: value.carrier_up,
    counters,
    rate,
    transition: value.transition as NetworkInterfaceTransition,
  }
}

function normalizeNetworkApplication(value: unknown): NetworkApplicationTraffic | null {
  if (
    !isRecord(value)
    || typeof value.application_key !== 'string'
    || value.application_key.length === 0
    || new TextEncoder().encode(value.application_key).length > 512
    || value.application_key.includes('\0')
    || !isNonNegativeInteger(value.rx_bytes)
    || !isNonNegativeInteger(value.tx_bytes)
  ) return null
  const rxSharePercent = nullableNumber(value.rx_share_percent)
  const txSharePercent = nullableNumber(value.tx_share_percent)
  if (
    rxSharePercent === undefined
    || txSharePercent === undefined
    || (rxSharePercent !== null && (rxSharePercent < 0 || rxSharePercent > 100))
    || (txSharePercent !== null && (txSharePercent < 0 || txSharePercent > 100))
  ) return null
  return {
    applicationKey: value.application_key,
    rxBytes: value.rx_bytes,
    txBytes: value.tx_bytes,
    rxSharePercent,
    txSharePercent,
  }
}

function normalizeNetworkCoverage(value: unknown): NetworkCoverage | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.reported_interfaces)
    || !isNonNegativeInteger(value.interfaces_with_counters)
    || value.interfaces_with_counters > value.reported_interfaces
    || typeof value.includes_loopback !== 'boolean'
    || typeof value.includes_tunnels !== 'boolean'
    || !isEnumValue(networkLayeredAccountingValues, value.layered_accounting)
    || typeof value.reason !== 'string'
    || value.reason.length === 0
  ) return null
  return {
    reportedInterfaces: value.reported_interfaces,
    interfacesWithCounters: value.interfaces_with_counters,
    includesLoopback: value.includes_loopback,
    includesTunnels: value.includes_tunnels,
    layeredAccounting: value.layered_accounting as NetworkLayeredAccounting,
    reason: value.reason,
  }
}

function normalizeNetworkTotals(value: unknown): NetworkTrafficTotals | null {
  if (!isRecord(value) || value.scope !== 'inclusive_interfaces') return null
  const allInterfaces = normalizeNetworkByteTotals(value.all_interfaces)
  const physical = normalizeNetworkByteTotals(value.physical)
  const loopback = normalizeNetworkByteTotals(value.loopback)
  const tunnel = normalizeNetworkByteTotals(value.tunnel)
  const otherVirtual = normalizeNetworkByteTotals(value.other_virtual)
  return allInterfaces && physical && loopback && tunnel && otherVirtual
    ? { scope: value.scope, allInterfaces, physical, loopback, tunnel, otherVirtual }
    : null
}

function normalizeNetworkSnapshot(value: unknown): NetworkSnapshot | null {
  if (!isRecord(value) || value.schema_version !== 1 || !isUuid(value.snapshot_id)) return null
  const capturedAtUnixMs = nullableNonNegativeInteger(value.captured_at_unix_ms)
  const observedBoottimeMs = nullableNonNegativeInteger(value.observed_boottime_ms)
  const sampleIntervalMs = nullableNonNegativeInteger(value.sample_interval_ms)
  const lastSuccessAtUnixMs = nullableNonNegativeInteger(value.last_success_at_unix_ms)
  const freshness = isEnumValue(networkFreshnessValues, value.freshness)
    ? (value.freshness as NetworkFreshness)
    : null
  const systemTraffic = normalizeNetworkCapability(value.system_traffic)
  const perApplication = normalizeNetworkCapability(value.per_application)
  const coverage = normalizeNetworkCoverage(value.coverage)
  const totals = value.totals === null ? null : normalizeNetworkTotals(value.totals)
  const aggregateRate = normalizeNetworkRate(value.aggregate_rate)
  const interfaces = Array.isArray(value.interfaces) && value.interfaces.length <= 256
    ? value.interfaces.map(normalizeNetworkInterface)
    : null
  const applications = Array.isArray(value.applications) && value.applications.length <= 1_024
    ? value.applications.map(normalizeNetworkApplication)
    : null
  if (
    capturedAtUnixMs === undefined
    || observedBoottimeMs === undefined
    || sampleIntervalMs === undefined
    || lastSuccessAtUnixMs === undefined
    || freshness === null
    || typeof value.retryable !== 'boolean'
    || !systemTraffic
    || !perApplication
    || !coverage
    || (value.totals !== null && !totals)
    || !aggregateRate
    || !interfaces
    || !applications
    || !interfaces.every((item): item is NetworkInterfaceSample => item !== null)
    || !applications.every((item): item is NetworkApplicationTraffic => item !== null)
    || coverage.reportedInterfaces !== interfaces.length
    || (perApplication.status === 'unsupported' && applications.length !== 0)
    || (systemTraffic.status === 'healthy' && (
      capturedAtUnixMs === null
      || observedBoottimeMs === null
      || lastSuccessAtUnixMs === null
      || !totals
      || freshness !== 'fresh'
    ))
  ) return null
  return {
    schemaVersion: 1,
    snapshotId: value.snapshot_id,
    capturedAtUnixMs,
    observedBoottimeMs,
    sampleIntervalMs,
    lastSuccessAtUnixMs,
    freshness,
    retryable: value.retryable,
    systemTraffic,
    perApplication,
    coverage,
    totals,
    aggregateRate,
    interfaces,
    applications,
  }
}

export async function getNetworkSnapshot(): Promise<NetworkFetchResult> {
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const snapshot = normalizeNetworkSnapshot(await invoke<unknown>('network_snapshot'))
    return snapshot
      ? { kind: 'snapshot', snapshot }
      : {
          kind: 'error',
          error: {
            kind: 'protocol',
            code: 'invalid_network_response',
            reason: 'invalid_network_response',
            retryable: false,
          },
        }
  } catch (error) {
    return {
      kind: 'error',
      error: normalizeBridgeError(error, 'network_snapshot_unreachable'),
    }
  }
}

function validUsageBucketKey(period: UsagePeriod, bucketKey: string): boolean {
  if (bucketKey.includes('\0')) return false
  if (period === 'daily') {
    const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(bucketKey)
    if (!match) return false
    const month = Number(match[2])
    const day = Number(match[3])
    return month >= 1 && month <= 12 && day >= 1 && day <= 31
  }
  const match = /^(\d{4})-W(\d{2})$/.exec(bucketKey)
  if (!match) return false
  const week = Number(match[2])
  return week >= 1 && week <= 53
}

function normalizeUsageQuery(value: unknown): UsageSummaryQuery | null {
  if (!isRecord(value) || !isEnumValue(usagePeriods, value.period)) return null
  if (typeof value.bucket_key !== 'string' || !validUsageBucketKey(value.period, value.bucket_key)) {
    return null
  }
  return { period: value.period, bucketKey: value.bucket_key }
}

function normalizeUsageCoverage(value: unknown): UsageCoverage | null {
  if (
    !isRecord(value)
    || !isEnumValue(backendStatuses, value.status)
    || typeof value.reason !== 'string'
    || value.reason.length === 0
    || typeof value.niri_event_stream_connected !== 'boolean'
    || typeof value.logind_session_available !== 'boolean'
    || !isNonNegativeInteger(value.event_gap_count)
    || value.definition !== 'foreground_unlocked_input_active_300s_monotonic'
  ) return null
  const lastCheckpointUnixMs = nullableSafeInteger(value.last_checkpoint_unix_ms)
  const trackingStartedUnixMs = nullableSafeInteger(value.tracking_started_unix_ms)
  if (
    lastCheckpointUnixMs === undefined
    || trackingStartedUnixMs === undefined
    || typeof value.bucket_start_covered !== 'boolean'
  ) return null
  return {
    status: value.status,
    reason: value.reason,
    niriEventStreamConnected: value.niri_event_stream_connected,
    logindSessionAvailable: value.logind_session_available,
    eventGapCount: value.event_gap_count,
    lastCheckpointUnixMs,
    trackingStartedUnixMs,
    bucketStartCovered: value.bucket_start_covered,
    definition: value.definition,
  }
}

function normalizeUsageApplication(
  value: unknown,
  query: UsageSummaryQuery,
): UsageApplicationDuration | null {
  if (
    !isRecord(value)
    || typeof value.app_id !== 'string'
    || value.app_id.length === 0
    || new TextEncoder().encode(value.app_id).length > 512
    || value.app_id.includes('\0')
    || value.bucket_key !== query.bucketKey
    || typeof value.timezone_id !== 'string'
    || value.timezone_id.length === 0
    || new TextEncoder().encode(value.timezone_id).length > 128
    || value.timezone_id.includes('\0')
    || !isSafeInteger(value.utc_offset_seconds)
    || value.utc_offset_seconds < -2_147_483_648
    || value.utc_offset_seconds > 2_147_483_647
    || !isNonNegativeInteger(value.duration_ns)
    || !isSafeInteger(value.last_wall_utc_ms)
  ) return null
  return {
    appId: value.app_id,
    bucketKey: value.bucket_key,
    timezoneId: value.timezone_id,
    utcOffsetSeconds: value.utc_offset_seconds,
    durationNs: value.duration_ns,
    lastWallUtcMs: value.last_wall_utc_ms,
  }
}

function normalizeUsageSummary(value: unknown): UsageSummary | null {
  if (!isRecord(value) || value.schema_version !== 3 || !isUuid(value.snapshot_id)) return null
  const capturedAtUnixMs = nullableSafeInteger(value.captured_at_unix_ms)
  const query = normalizeUsageQuery(value.query)
  const coverage = normalizeUsageCoverage(value.coverage)
  const applications = query && Array.isArray(value.applications) && value.applications.length <= 1_024
    ? value.applications.map((application) => normalizeUsageApplication(application, query))
    : null
  if (
    capturedAtUnixMs === undefined
    || !query
    || !isEnumValue(backendStatuses, value.status)
    || typeof value.reason !== 'string'
    || value.reason.length === 0
    || typeof value.retryable !== 'boolean'
    || !coverage
    || !applications
    || !applications.every((application): application is UsageApplicationDuration => application !== null)
    || (value.status === 'healthy' && (
      !coverage.niriEventStreamConnected
      || !coverage.logindSessionAvailable
      || coverage.status !== 'healthy'
      || capturedAtUnixMs === null
      || coverage.lastCheckpointUnixMs === null
      || coverage.trackingStartedUnixMs === null
      || !coverage.bucketStartCovered
    ))
  ) return null
  return {
    schemaVersion: 3,
    snapshotId: value.snapshot_id,
    capturedAtUnixMs,
    query,
    status: value.status,
    reason: value.reason,
    retryable: value.retryable,
    coverage,
    applications,
  }
}

export async function getUsageSummary(query: UsageSummaryQuery): Promise<UsageFetchResult> {
  if (!usagePeriods.includes(query.period) || !validUsageBucketKey(query.period, query.bucketKey)) {
    return {
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'invalid_usage_query',
        reason: 'usage_bucket_key_invalid',
        retryable: false,
      },
    }
  }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const summary = normalizeUsageSummary(await invoke<unknown>('usage_summary', {
      query: { period: query.period, bucket_key: query.bucketKey },
    }))
    return summary
      ? { kind: 'summary', summary }
      : {
          kind: 'error',
          error: {
            kind: 'protocol',
            code: 'invalid_usage_response',
            reason: 'invalid_usage_response',
            retryable: false,
          },
        }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'usage_summary_unreachable') }
  }
}

function normalizeRemoteCapability(value: unknown): RemoteCapabilityStatus | null {
  if (!isRecord(value) || !isEnumValue(['supported', 'unsupported'] as const, value.status)) {
    return null
  }
  if (value.status === 'supported') return { status: 'supported', reason: null }
  return typeof value.reason === 'string' && value.reason.length > 0
    ? { status: 'unsupported', reason: value.reason }
    : null
}

function normalizeOperationCapabilities(value: unknown): RemoteOperationCapability[] | null {
  if (!Array.isArray(value) || value.length !== fileOperations.length) return null
  const capabilities = value.map((item) => {
    if (!isRecord(item) || !isEnumValue(fileOperations, item.operation)) return null
    const status = normalizeRemoteCapability(item.status)
    return status ? { operation: item.operation, ...status } : null
  })
  if (!capabilities.every((item): item is RemoteOperationCapability => item !== null)) return null
  const operations = new Set(capabilities.map((item) => item.operation))
  return operations.size === fileOperations.length ? capabilities : null
}

function normalizeAdapterAvailability(value: unknown): BackendHealth | null {
  if (!isRecord(value) || !isEnumValue(backendStatuses, value.status)) return null
  if (value.status === 'healthy') return { status: 'healthy', capabilityReason: 'available' }
  return typeof value.reason === 'string' && value.reason.length > 0
    ? { status: value.status, capabilityReason: value.reason }
    : null
}

function normalizeRemoteAdapter(value: unknown): RemoteAdapterDescriptor | null {
  if (!isRecord(value) || !isEnumValue(remoteProtocols, value.protocol)) return null
  const availability = normalizeAdapterAvailability(value.availability)
  const terminal = normalizeRemoteCapability(value.terminal)
  const fileCapabilities = normalizeOperationCapabilities(value.file_operations)
  return availability && terminal && fileCapabilities
    ? {
        protocol: value.protocol,
        availability,
        terminal,
        fileOperations: fileCapabilities,
      }
    : null
}

function normalizeRemoteCatalog(value: unknown): RemoteAdapterCatalog | null {
  if (!isRecord(value) || value.schema_version !== 1 || !isUuid(value.snapshot_id)) return null
  if (!isNonNegativeInteger(value.captured_at_unix_ms) || !Array.isArray(value.adapters)) return null
  const adapters = value.adapters.map(normalizeRemoteAdapter)
  if (!adapters.every((adapter): adapter is RemoteAdapterDescriptor => adapter !== null)) return null
  const protocols = new Set(adapters.map((adapter) => adapter.protocol))
  if (protocols.size !== remoteProtocols.length || remoteProtocols.some((protocol) => !protocols.has(protocol))) {
    return null
  }
  return {
    schemaVersion: 1,
    snapshotId: value.snapshot_id,
    capturedAtUnixMs: value.captured_at_unix_ms,
    adapters,
  }
}

function normalizeAuthentication(value: unknown): RemoteAuthentication | null {
  if (!isRecord(value) || typeof value.method !== 'string') return null
  if (value.method === 'anonymous' || value.method === 'ssh_agent' || value.method === 'kerberos') {
    return { method: value.method }
  }
  const normalizeSecretRef = (candidate: unknown) => {
    if (!isRecord(candidate) || candidate.backend !== 'secret_service' || !isUuid(candidate.item_id)) return null
    return { backend: 'secret_service' as const, item_id: candidate.item_id }
  }
  if (value.method === 'password') {
    const secret = normalizeSecretRef(value.secret)
    return secret ? { method: 'password', secret } : null
  }
  if (value.method === 'ssh_key') {
    const privateKey = normalizeSecretRef(value.private_key)
    const passphrase = value.passphrase === null ? null : normalizeSecretRef(value.passphrase)
    return privateKey && (value.passphrase === null || passphrase)
      ? { method: 'ssh_key', private_key: privateKey, passphrase }
      : null
  }
  return null
}

function normalizeTrustPolicy(value: unknown): RemoteTrustPolicy | null {
  if (!isRecord(value) || typeof value.kind !== 'string') return null
  if (value.kind === 'ssh_known_hosts' && isEnumValue(['reject', 'ask_user'] as const, value.first_use)) {
    return { kind: value.kind, first_use: value.first_use }
  }
  if (
    value.kind === 'pinned_tls_certificate'
    && typeof value.certificate_pem === 'string'
    && value.certificate_pem.length > 0
    && new TextEncoder().encode(value.certificate_pem).byteLength <= 16 * 1024
  ) {
    return { kind: value.kind, certificate_pem: value.certificate_pem }
  }
  if (value.kind === 'system_tls' || value.kind === 'plaintext_acknowledged' || value.kind === 'smb_negotiated') {
    return { kind: value.kind }
  }
  return null
}

function normalizeProfileOptions(value: unknown): RemoteProfileOptions | null {
  if (!isRecord(value) || !isEnumValue(remoteProtocols, value.protocol)) return null
  if (value.protocol === 'ssh') {
    return Array.isArray(value.jump_profiles) && value.jump_profiles.every(isUuid) && typeof value.agent_forwarding === 'boolean'
      ? { protocol: 'ssh', jump_profiles: value.jump_profiles, agent_forwarding: value.agent_forwarding }
      : null
  }
  if (value.protocol === 'sftp') {
    return Array.isArray(value.jump_profiles) && value.jump_profiles.every(isUuid)
      ? { protocol: 'sftp', jump_profiles: value.jump_profiles }
      : null
  }
  if (value.protocol === 'ftp') {
    return isEnumValue(['passive', 'active_restricted'] as const, value.data_connection)
      ? { protocol: 'ftp', data_connection: value.data_connection }
      : null
  }
  if (value.protocol === 'ftps_explicit') {
    return isEnumValue(['passive', 'active_restricted'] as const, value.data_connection)
      && typeof value.require_protected_data_channel === 'boolean'
      ? {
          protocol: 'ftps_explicit',
          data_connection: value.data_connection,
          require_protected_data_channel: value.require_protected_data_channel,
        }
      : null
  }
  return (value.share === null || typeof value.share === 'string')
    && isEnumValue(['smb2', 'smb3'] as const, value.minimum_dialect)
    && typeof value.require_signing === 'boolean'
    && typeof value.require_encryption === 'boolean'
    ? {
        protocol: 'smb',
        share: value.share,
        minimum_dialect: value.minimum_dialect,
        require_signing: value.require_signing,
        require_encryption: value.require_encryption,
      }
    : null
}

function normalizeRemoteProfile(value: unknown): RemoteConnectionProfile | null {
  if (!isRecord(value) || !isUuid(value.id) || !isEnumValue(remoteProtocols, value.protocol)) return null
  if (!isRecord(value.endpoint) || typeof value.endpoint.host !== 'string' || value.endpoint.host.length === 0) return null
  if (!Number.isSafeInteger(value.endpoint.port) || (value.endpoint.port as number) <= 0 || (value.endpoint.port as number) > 65535) return null
  const username = nullableString(value.username)
  const domain = nullableString(value.domain)
  const authentication = normalizeAuthentication(value.authentication)
  const trust = normalizeTrustPolicy(value.trust)
  const options = normalizeProfileOptions(value.options)
  if (typeof value.label !== 'string' || value.label.length === 0 || username === undefined || domain === undefined || !authentication || !trust || !options) return null
  if (options.protocol !== value.protocol) return null
  return {
    id: value.id,
    label: value.label,
    protocol: value.protocol,
    endpoint: { host: value.endpoint.host, port: value.endpoint.port as number },
    username,
    domain,
    authentication,
    trust,
    options,
  }
}

function normalizeStoredRemoteProfile(value: unknown): StoredRemoteProfile | null {
  if (!isRecord(value)) return null
  const profile = normalizeRemoteProfile(value.profile)
  if (!profile || !isNonNegativeInteger(value.revision) || !isNonNegativeInteger(value.created_at_unix_ms) || !isNonNegativeInteger(value.updated_at_unix_ms)) return null
  if (value.updated_at_unix_ms < value.created_at_unix_ms) return null
  return {
    profile,
    revision: value.revision,
    createdAtUnixMs: value.created_at_unix_ms,
    updatedAtUnixMs: value.updated_at_unix_ms,
  }
}

function normalizeProfilePageResult(value: unknown): RemoteProfilePage | null {
  if (!isRecord(value) || value.result !== 'page' || !isRecord(value.value) || !Array.isArray(value.value.profiles)) return null
  const profiles = value.value.profiles.map(normalizeStoredRemoteProfile)
  const nextAfter = nullableString(value.value.next_after)
  return profiles.every((profile): profile is StoredRemoteProfile => profile !== null) && nextAfter !== undefined
    ? { profiles, nextAfter }
    : null
}

function remoteProfilePageMatches(page: RemoteProfilePage, after: string | null): boolean {
  if (page.profiles.length > 16) return false
  if (after !== null && page.profiles.some((stored) => stored.profile.id <= after)) return false
  if (page.profiles.some((stored, index) => index > 0 && page.profiles[index - 1].profile.id >= stored.profile.id)) {
    return false
  }
  return page.nextAfter === null
    || page.profiles.length > 0 && page.nextAfter === page.profiles[page.profiles.length - 1].profile.id
}

function normalizeStoredProfileResult(value: unknown): StoredRemoteProfile | null {
  return isRecord(value) && value.result === 'stored' ? normalizeStoredRemoteProfile(value.value) : null
}

async function invokeRemote<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  normalize: (value: unknown) => T | null,
  fallbackCode: string,
): Promise<RemoteFetchResult<T>> {
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const payload = await invoke<unknown>(command, args)
    const data = normalize(payload)
    return data === null
      ? {
          kind: 'error',
          error: { kind: 'protocol', code: fallbackCode, reason: fallbackCode, retryable: false },
        }
      : { kind: 'data', data }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, `${command}_unreachable`) }
  }
}

export function getRemoteAdapterCatalog(): Promise<RemoteFetchResult<RemoteAdapterCatalog>> {
  return invokeRemote('remote_capabilities', undefined, normalizeRemoteCatalog, 'invalid_remote_catalog_response')
}

export function getRemoteProfiles(after: string | null = null): Promise<RemoteFetchResult<RemoteProfilePage>> {
  return invokeRemote(
    'remote_profile',
    { command: { operation: 'list', query: { after, limit: 16 } } },
    (value) => {
      const page = normalizeProfilePageResult(value)
      return page && remoteProfilePageMatches(page, after) ? page : null
    },
    'invalid_remote_profile_page_response',
  )
}

export function upsertRemoteProfile(profile: RemoteConnectionProfile, expectedRevision: number | null = null): Promise<RemoteFetchResult<StoredRemoteProfile>> {
  return invokeRemote(
    'remote_profile',
    { command: { operation: 'upsert', profile, expected_revision: expectedRevision } },
    (value) => {
      const stored = normalizeStoredProfileResult(value)
      const expectedNext = expectedRevision === null ? 0 : expectedRevision + 1
      return stored && stored.profile.id === profile.id && stored.revision === expectedNext
        ? stored
        : null
    },
    'invalid_remote_profile_response',
  )
}

function normalizeSecretReference(value: unknown): RemoteSecretReference | null {
  if (!isRecord(value) || value.backend !== 'secret_service' || !isUuid(value.item_id)) return null
  return { backend: 'secret_service', item_id: value.item_id }
}

function normalizeStoredSecretResult(value: unknown): RemoteSecretReference | null {
  if (!isRecord(value) || value.result !== 'stored' || !isRecord(value.value)) return null
  return normalizeSecretReference(value.value.reference)
}

export async function storeRemoteSecret(kind: RemoteSecretKind, value: Uint8Array): Promise<RemoteFetchResult<RemoteSecretReference>> {
  if (!remoteSecretKinds.includes(kind) || value.byteLength === 0 || value.byteLength > MAX_REMOTE_SECRET_BYTES) {
    return {
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'remote_secret_input_invalid',
        reason: 'remote_secret_input_invalid',
        retryable: false,
      },
    }
  }
  const transient = Array.from(value)
  try {
    return await invokeRemote(
      'secret',
      { command: { operation: 'store', kind, value: transient } },
      normalizeStoredSecretResult,
      'invalid_remote_secret_response',
    )
  } finally {
    transient.fill(0)
  }
}

export function deleteRemoteSecret(reference: RemoteSecretReference): Promise<RemoteFetchResult<string>> {
  const normalized = normalizeSecretReference(reference)
  if (!normalized) {
    return Promise.resolve({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'remote_secret_reference_invalid',
        reason: 'remote_secret_reference_invalid',
        retryable: false,
      },
    })
  }
  return invokeRemote(
    'secret',
    { command: { operation: 'delete', reference: normalized } },
    (value) => isRecord(value) && value.result === 'deleted' ? normalized.item_id : null,
    'invalid_remote_secret_delete_response',
  )
}

export async function deleteRemoteProfile(profileId: string, expectedRevision: number): Promise<RemoteFetchResult<string>> {
  return invokeRemote(
    'remote_profile',
    { command: { operation: 'delete', profile_id: profileId, expected_revision: expectedRevision } },
    (value) => isRecord(value) && value.result === 'deleted' && isRecord(value.value) && value.value.profile_id === profileId ? profileId : null,
    'invalid_remote_profile_delete_response',
  )
}

function normalizeConnectionState(value: unknown): { state: string; reason: string | null } | null {
  if (!isRecord(value) || typeof value.status !== 'string') return null
  const reason = value.reason === undefined ? null : nullableString(value.reason)
  if (reason === undefined) return null
  return { state: value.status, reason }
}

function normalizeRemoteSession(value: unknown): RemoteSession | null {
  if (!isRecord(value) || !isUuid(value.id) || !isUuid(value.profile_id) || !isEnumValue(remoteProtocols, value.protocol)) return null
  const state = normalizeConnectionState(value.state)
  const capabilities = normalizeOperationCapabilities(value.capabilities)
  if (!state || !capabilities || !isNonNegativeInteger(value.opened_at_unix_ms) || !isNonNegativeInteger(value.updated_at_unix_ms)) return null
  return {
    id: value.id,
    profileId: value.profile_id,
    protocol: value.protocol,
    state: state.state,
    stateReason: state.reason,
    capabilities,
    openedAtUnixMs: value.opened_at_unix_ms,
    updatedAtUnixMs: value.updated_at_unix_ms,
  }
}

function normalizeSessionResult(value: unknown): RemoteSession | null {
  return isRecord(value) && value.result === 'session' ? normalizeRemoteSession(value.value) : null
}

function normalizeRemoteEntry(value: unknown): RemoteEntry | null {
  if (!isRecord(value) || typeof value.name !== 'string' || typeof value.path !== 'string') return null
  if (!isEnumValue(['file', 'directory', 'symlink', 'other'] as const, value.kind) || !isRecord(value.identity)) return null
  const sizeBytes = nullableNonNegativeInteger(value.identity.size_bytes)
  const modifiedAtUnixMs = nullableNumber(value.identity.modified_at_unix_ms)
  const unixMode = nullableNonNegativeInteger(value.unix_mode)
  const capabilities = normalizeOperationCapabilities(value.capabilities)
  return sizeBytes !== undefined && modifiedAtUnixMs !== undefined && unixMode !== undefined && capabilities
    ? { name: value.name, path: value.path, kind: value.kind, sizeBytes, modifiedAtUnixMs, unixMode, capabilities }
    : null
}

function normalizeDirectoryResult(value: unknown): RemoteDirectoryPage | null {
  if (!isRecord(value) || value.result !== 'directory_page' || !isRecord(value.value)) return null
  const page = value.value
  if (!isUuid(page.session_id) || typeof page.path !== 'string' || !isNonNegativeInteger(page.offset) || !Array.isArray(page.entries)) return null
  const entries = page.entries.map(normalizeRemoteEntry)
  const nextOffset = nullableNonNegativeInteger(page.next_offset)
  return entries.every((entry): entry is RemoteEntry => entry !== null) && nextOffset !== undefined
    ? { sessionId: page.session_id, path: page.path, offset: page.offset, entries, nextOffset }
    : null
}

export function connectRemoteSession(profileId: string): Promise<RemoteFetchResult<RemoteSession>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'connect', profile_id: profileId } },
    (value) => {
      const session = normalizeSessionResult(value)
      return session?.profileId === profileId ? session : null
    },
    'invalid_remote_session_response',
  )
}

export function listRemoteDirectory(sessionId: string, path: string, offset = 0): Promise<RemoteFetchResult<RemoteDirectoryPage>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'list', query: { session_id: sessionId, path, offset, limit: 2 } } },
    (value) => {
      const page = normalizeDirectoryResult(value)
      if (!page || page.sessionId !== sessionId || page.path !== path || page.offset !== offset || page.entries.length > 2) {
        return null
      }
      const expectedNext = offset + page.entries.length
      return page.nextOffset === null || page.nextOffset === expectedNext ? page : null
    },
    'invalid_remote_directory_response',
  )
}

export function createRemoteDirectory(sessionId: string, path: string): Promise<RemoteFetchResult<RemoteEntry>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'create_directory', session_id: sessionId, path } },
    (value) => {
      if (!isRecord(value) || value.result !== 'entry') return null
      const entry = normalizeRemoteEntry(value.value)
      return entry?.path === path && entry.kind === 'directory' ? entry : null
    },
    'invalid_remote_create_directory_response',
  )
}

export function renameRemoteEntry(sessionId: string, from: string, to: string): Promise<RemoteFetchResult<RemoteEntry>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'rename', session_id: sessionId, from, to } },
    (value) => {
      if (!isRecord(value) || value.result !== 'entry') return null
      const entry = normalizeRemoteEntry(value.value)
      return entry?.path === to ? entry : null
    },
    'invalid_remote_rename_response',
  )
}

export function deleteRemoteEntry(sessionId: string, path: string): Promise<RemoteFetchResult<string>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'delete', session_id: sessionId, path } },
    (value) => isRecord(value)
      && value.result === 'deleted'
      && isRecord(value.value)
      && value.value.session_id === sessionId
      ? sessionId
      : null,
    'invalid_remote_delete_response',
  )
}

export function disconnectRemoteSession(sessionId: string): Promise<RemoteFetchResult<string>> {
  return invokeRemote(
    'remote_session',
    { command: { operation: 'disconnect', session_id: sessionId } },
    (value) => isRecord(value) && value.result === 'disconnected' && isRecord(value.value) && value.value.session_id === sessionId ? sessionId : null,
    'invalid_remote_disconnect_response',
  )
}

function normalizeTerminalStatus(value: unknown): TerminalStatus | null {
  if (!isRecord(value) || !isRecord(value.state) || typeof value.state.state !== 'string') return null
  const state = value.state.state
  if (!isEnumValue(['running', 'exited', 'disconnected', 'closed_by_client'] as const, state)) return null
  let detail: string | null = null
  if (state === 'exited' && isRecord(value.state.value)) detail = value.state.value.code === null ? 'unknown' : String(value.state.value.code)
  if (state === 'disconnected' && isRecord(value.state.value) && typeof value.state.value.reason === 'string') detail = value.state.value.reason
  if (!isNonNegativeInteger(value.transcript_retained_bytes) || !isNonNegativeInteger(value.transcript_dropped_bytes)) return null
  return {
    state,
    detail,
    transcriptRetainedBytes: value.transcript_retained_bytes,
    transcriptDroppedBytes: value.transcript_dropped_bytes,
  }
}

function normalizeTerminalCapabilities(value: unknown): TerminalCapabilities | null {
  if (!isRecord(value)) return null
  if (
    !isNonNegativeInteger(value.max_output_chunk_bytes)
    || value.max_output_chunk_bytes === 0
    || value.max_output_chunk_bytes > 45_056
    || !isNonNegativeInteger(value.max_input_chunk_bytes)
    || value.max_input_chunk_bytes === 0
    || value.max_input_chunk_bytes > 45_056
    || !isNonNegativeInteger(value.max_transcript_bytes)
    || value.max_transcript_bytes === 0
    || value.max_transcript_bytes > 65_536
    || !isNonNegativeInteger(value.max_rows)
    || value.max_rows === 0
    || value.max_rows > 1_000
    || !isNonNegativeInteger(value.max_columns)
    || value.max_columns === 0
    || value.max_columns > 1_000
    || !isNonNegativeInteger(value.max_pixel_dimension)
    || value.max_pixel_dimension > 32_767
    || value.nonblocking_output !== true
    || value.fixed_openssh_program !== true
  ) return null
  return {
    maxOutputChunkBytes: value.max_output_chunk_bytes,
    maxInputChunkBytes: value.max_input_chunk_bytes,
    maxTranscriptBytes: value.max_transcript_bytes,
    maxRows: value.max_rows,
    maxColumns: value.max_columns,
    maxPixelDimension: value.max_pixel_dimension,
    nonblockingOutput: true,
    fixedOpenSshProgram: true,
  }
}

function normalizeOpenedTerminal(value: unknown): OpenedTerminal | null {
  if (!isRecord(value) || value.result !== 'opened' || !isRecord(value.value) || !isUuid(value.value.session_id)) return null
  const capabilities = normalizeTerminalCapabilities(value.value.capabilities)
  const status = normalizeTerminalStatus(value.value.status)
  return capabilities && status ? { sessionId: value.value.session_id, capabilities, status } : null
}

function normalizeTerminalReadResult(value: unknown): TerminalRead | null {
  if (!isRecord(value) || value.result !== 'read' || !isRecord(value.value) || !isUuid(value.value.session_id) || !isRecord(value.value.output)) return null
  const output = value.value.output
  if (output.status === 'pending' || output.status === 'end_of_stream') return { status: output.status }
  return output.status === 'data' && typeof output.data === 'string' && output.data.length > 0
    ? { status: 'data', encodedData: output.data }
    : null
}

function decodedBase64ByteLength(value: string): number | null {
  try {
    return atob(value).length
  } catch {
    return null
  }
}

function normalizeTerminalStatusResult(value: unknown): TerminalStatus | null {
  return isRecord(value) && (value.result === 'status' || value.result === 'closed') && isRecord(value.value)
    ? normalizeTerminalStatus(value.value.status)
    : null
}

function validTerminalSize(size: TerminalSize): boolean {
  return Number.isSafeInteger(size.rows)
    && size.rows > 0
    && size.rows <= 1_000
    && Number.isSafeInteger(size.columns)
    && size.columns > 0
    && size.columns <= 1_000
    && Number.isSafeInteger(size.pixelWidth)
    && size.pixelWidth >= 0
    && size.pixelWidth <= 32_767
    && Number.isSafeInteger(size.pixelHeight)
    && size.pixelHeight >= 0
    && size.pixelHeight <= 32_767
}

function invalidTerminalSize(): Promise<RemoteFetchResult<never>> {
  return Promise.resolve({
    kind: 'error',
    error: {
      kind: 'protocol',
      code: 'remote_terminal_size_invalid',
      reason: 'remote_terminal_size_invalid',
      retryable: false,
    },
  })
}

function terminalSizeCommand(size: TerminalSize) {
  return {
    rows: size.rows,
    columns: size.columns,
    pixel_width: size.pixelWidth,
    pixel_height: size.pixelHeight,
  }
}

export function openRemoteTerminal(
  profileId: string,
  size: TerminalSize,
  acceptNewHostKey = false,
): Promise<RemoteFetchResult<OpenedTerminal>> {
  if (!validTerminalSize(size)) return invalidTerminalSize()
  return invokeRemote(
    'remote_terminal',
    {
      command: {
        operation: 'open',
        profile_id: profileId,
        size: terminalSizeCommand(size),
        accept_new_host_key: acceptNewHostKey,
      },
    },
    normalizeOpenedTerminal,
    'invalid_remote_terminal_response',
  )
}

export function resizeRemoteTerminal(sessionId: string, size: TerminalSize): Promise<RemoteFetchResult<string>> {
  if (!validTerminalSize(size)) return invalidTerminalSize()
  return invokeRemote(
    'remote_terminal',
    { command: { operation: 'resize', session_id: sessionId, size: terminalSizeCommand(size) } },
    (value) => isRecord(value) && value.result === 'resized' && isRecord(value.value) && value.value.session_id === sessionId ? sessionId : null,
    'invalid_remote_terminal_resize_response',
  )
}

const MAX_TERMINAL_READ_BYTES = 45_056

export function readRemoteTerminal(
  sessionId: string,
  maxBytes = MAX_TERMINAL_READ_BYTES,
): Promise<RemoteFetchResult<TerminalRead>> {
  if (!Number.isInteger(maxBytes) || maxBytes < 1 || maxBytes > MAX_TERMINAL_READ_BYTES) {
    return Promise.resolve({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'remote_terminal_read_limit_invalid',
        reason: 'remote_terminal_read_limit_invalid',
        retryable: false,
      },
    })
  }
  return invokeRemote(
    'remote_terminal',
    { command: { operation: 'read', session_id: sessionId, max_bytes: maxBytes } },
    (value) => {
      if (!isRecord(value) || !isRecord(value.value) || value.value.session_id !== sessionId) return null
      const output = normalizeTerminalReadResult(value)
      if (!output || output.status !== 'data') return output
      const bytes = decodedBase64ByteLength(output.encodedData)
      return bytes !== null && bytes <= maxBytes ? output : null
    },
    'invalid_remote_terminal_read_response',
  )
}

export function pollRemoteTerminal(sessionId: string): Promise<RemoteFetchResult<TerminalStatus>> {
  return invokeRemote(
    'remote_terminal',
    { command: { operation: 'poll', session_id: sessionId } },
    (value) => isRecord(value) && isRecord(value.value) && value.value.session_id === sessionId
      ? normalizeTerminalStatusResult(value)
      : null,
    'invalid_remote_terminal_status_response',
  )
}

export function writeRemoteTerminal(sessionId: string, encodedData: string): Promise<RemoteFetchResult<number>> {
  const inputBytes = decodedBase64ByteLength(encodedData)
  if (inputBytes === null || inputBytes === 0 || inputBytes > MAX_TERMINAL_READ_BYTES) {
    return Promise.resolve({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'remote_terminal_data_invalid',
        reason: 'remote_terminal_data_invalid',
        retryable: false,
      },
    })
  }
  return invokeRemote(
    'remote_terminal',
    { command: { operation: 'write', session_id: sessionId, data: encodedData } },
    (value) => isRecord(value)
      && value.result === 'wrote'
      && isRecord(value.value)
      && value.value.session_id === sessionId
      && value.value.accepted_bytes === inputBytes
      ? inputBytes
      : null,
    'invalid_remote_terminal_write_response',
  )
}

export function closeRemoteTerminal(sessionId: string): Promise<RemoteFetchResult<TerminalStatus>> {
  return invokeRemote(
    'remote_terminal',
    { command: { operation: 'close', session_id: sessionId } },
    (value) => {
      if (!isRecord(value) || value.result !== 'closed' || !isRecord(value.value) || value.value.session_id !== sessionId) {
        return null
      }
      const status = normalizeTerminalStatusResult(value)
      return status?.state === 'closed_by_client' ? status : null
    },
    'invalid_remote_terminal_close_response',
  )
}

export async function streamRemoteTerminal(
  sessionId: string,
  maxBytes: number,
  onEvent: (event: TerminalStreamEvent) => void,
): Promise<RemoteFetchResult<void>> {
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  if (!Number.isInteger(maxBytes) || maxBytes < 1 || maxBytes > MAX_TERMINAL_READ_BYTES) {
    return {
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'remote_terminal_read_limit_invalid',
        reason: 'remote_terminal_read_limit_invalid',
        retryable: false,
      },
    }
  }
  let invalidEvent = false
  const channel = new Channel<unknown>()
  channel.onmessage = (value) => {
    const event = normalizeTerminalStreamEvent(value, sessionId, maxBytes)
    if (event) onEvent(event)
    else invalidEvent = true
  }
  try {
    await invoke<void>('remote_terminal_stream', {
      sessionId,
      maxBytes,
      onEvent: channel,
    })
    return invalidEvent
      ? {
          kind: 'error',
          error: {
            kind: 'protocol',
            code: 'invalid_remote_terminal_stream_event',
            reason: 'invalid_remote_terminal_stream_event',
            retryable: false,
          },
        }
      : { kind: 'data', data: undefined }
  } catch (error) {
    return {
      kind: 'error',
      error: normalizeBridgeError(error, 'remote_terminal_stream_unreachable'),
    }
  }
}

function normalizeTerminalStreamEvent(
  value: unknown,
  sessionId: string,
  maxBytes: number,
): TerminalStreamEvent | null {
  if (!isRecord(value) || value.session_id !== sessionId || typeof value.event !== 'string') return null
  if (value.event === 'data') {
    if (typeof value.data !== 'string') return null
    const bytes = decodedBase64ByteLength(value.data)
    return bytes !== null && bytes > 0 && bytes <= maxBytes
      ? { event: 'data', sessionId, encodedData: value.data }
      : null
  }
  const status = normalizeTerminalStatus(value.status)
  if (!status) return null
  if (value.event === 'started') {
    return value.max_bytes === maxBytes
      ? { event: 'started', sessionId, maxBytes, status }
      : null
  }
  if (value.event === 'status') return { event: 'status', sessionId, status }
  if (value.event === 'ended' && status.state !== 'running') {
    return { event: 'ended', sessionId, status }
  }
  return null
}

const MAX_NOTE_TITLE_CHARS = 512
const MAX_NOTE_TAGS = 64
const MAX_NOTE_TAG_CHARS = 64
const MAX_NOTE_QUERY_LIMIT = 64
const MAX_NOTE_QUERY_OFFSET = 100_000
const MAX_NOTE_SEARCH_CHARS = 512
const MAX_NOTE_BODY_BYTES = 4 * 1024 * 1024
const MAX_NOTE_EXPORT_BYTES = 45_056 * 598

function validCalendarDate(value: string): boolean {
  if (value.length !== 10 || value[4] !== '-' || value[7] !== '-') return false
  const year = Number(value.slice(0, 4))
  const month = Number(value.slice(5, 7))
  const day = Number(value.slice(8, 10))
  if (!Number.isInteger(year) || !Number.isInteger(month) || !Number.isInteger(day)) return false
  if (month < 1 || month > 12 || day < 1) return false
  const maxDay = month === 2
    ? (year % 400 === 0 || (year % 4 === 0 && year % 100 !== 0) ? 29 : 28)
    : [4, 6, 9, 11].includes(month) ? 30 : 31
  return day <= maxDay
}

function isSha256Hex(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value)
}

function validNoteMeta(meta: NoteDraftMeta): string | null {
  if (meta.title.includes('\0') || [...meta.title].length > MAX_NOTE_TITLE_CHARS) {
    return 'note_title_invalid'
  }
  if (meta.diaryDate !== null && !validCalendarDate(meta.diaryDate)) {
    return 'note_diary_date_invalid'
  }
  if (
    meta.tags.length > MAX_NOTE_TAGS
    || meta.tags.some((tag) => tag.includes('\0') || tag.length === 0 || [...tag].length > MAX_NOTE_TAG_CHARS)
  ) {
    return 'note_tags_invalid'
  }
  if (!noteStatuses.includes(meta.status)) return 'note_status_invalid'
  return null
}

function validNoteQuery(query: NoteQuery): string | null {
  if (
    query.search !== null
    && (query.search.includes('\0') || [...query.search].length > MAX_NOTE_SEARCH_CHARS)
  ) {
    return 'note_search_invalid'
  }
  if (query.diaryDateFrom !== null && !validCalendarDate(query.diaryDateFrom)) return 'note_query_date_invalid'
  if (query.diaryDateTo !== null && !validCalendarDate(query.diaryDateTo)) return 'note_query_date_invalid'
  if (
    query.tags.length > MAX_NOTE_TAGS
    || query.tags.some((tag) => tag.includes('\0') || tag.length === 0 || [...tag].length > MAX_NOTE_TAG_CHARS)
  ) {
    return 'note_tags_invalid'
  }
  if (query.status !== null && !noteStatuses.includes(query.status)) return 'note_status_invalid'
  if (!noteDeletedFilters.includes(query.deleted)) return 'note_deleted_filter_invalid'
  if (!noteSorts.includes(query.sort)) return 'note_sort_invalid'
  if (!Number.isInteger(query.limit) || query.limit < 1 || query.limit > MAX_NOTE_QUERY_LIMIT) {
    return 'note_query_limit_invalid'
  }
  if (!Number.isInteger(query.offset) || query.offset < 0 || query.offset > MAX_NOTE_QUERY_OFFSET) {
    return 'note_query_offset_invalid'
  }
  return null
}

function validNoteBody(bodyMarkdown: string): string | null {
  return new TextEncoder().encode(bodyMarkdown).length > MAX_NOTE_BODY_BYTES
    ? 'note_body_exceeds_4_mib'
    : null
}

function validNoteWriteInput(input: NoteWriteInput): string | null {
  const metaInvalid = validNoteMeta(input.meta)
  if (metaInvalid) return metaInvalid
  const bodyInvalid = validNoteBody(input.bodyMarkdown)
  if (bodyInvalid) return bodyInvalid
  if (input.kind === 'save' && (!isUuid(input.id) || !Number.isSafeInteger(input.expectedRevision) || input.expectedRevision < 1)) {
    return 'note_write_intent_invalid'
  }
  return null
}

function noteMetaWire(meta: NoteDraftMeta): Record<string, unknown> {
  return {
    title: meta.title,
    diary_date: meta.diaryDate,
    tags: meta.tags,
    status: meta.status,
    pinned: meta.pinned,
  }
}

function noteQueryWire(query: NoteQuery): Record<string, unknown> {
  return {
    search: query.search,
    diary_date_from: query.diaryDateFrom,
    diary_date_to: query.diaryDateTo,
    tags: query.tags,
    status: query.status,
    deleted: query.deleted,
    sort: query.sort,
    limit: query.limit,
    offset: query.offset,
  }
}

function normalizeNoteSummary(value: unknown): NoteSummary | null {
  if (
    !isRecord(value)
    || !isUuid(value.id)
    || typeof value.title !== 'string'
    || !noteStatuses.includes(value.status as NoteStatus)
    || typeof value.pinned !== 'boolean'
    || !isNonNegativeInteger(value.created_at_ms)
    || !isNonNegativeInteger(value.updated_at_ms)
    || !isNonNegativeInteger(value.revision)
    || value.revision === 0
    || !isNonNegativeInteger(value.body_bytes)
    || value.body_bytes > MAX_NOTE_BODY_BYTES
    || !isSha256Hex(value.body_sha256)
    || !Array.isArray(value.tags)
  ) return null
  const diaryDate = nullableString(value.diary_date)
  const deletedAtMs = nullableNonNegativeInteger(value.deleted_at_ms)
  if (
    diaryDate === undefined
    || (diaryDate !== null && !validCalendarDate(diaryDate))
    || deletedAtMs === undefined
    || value.tags.length > MAX_NOTE_TAGS
    || value.tags.some((tag) => typeof tag !== 'string' || tag.includes('\0') || tag.length === 0 || [...tag].length > MAX_NOTE_TAG_CHARS)
    || value.title.includes('\0')
    || [...value.title].length > MAX_NOTE_TITLE_CHARS
  ) return null
  return {
    id: value.id,
    title: value.title,
    diaryDate,
    tags: value.tags as string[],
    status: value.status as NoteStatus,
    pinned: value.pinned,
    createdAtMs: value.created_at_ms,
    updatedAtMs: value.updated_at_ms,
    deletedAtMs,
    revision: value.revision,
    bodyBytes: value.body_bytes,
    bodySha256: value.body_sha256,
  }
}

function normalizeNoteQuery(value: unknown): NoteQuery | null {
  if (
    !isRecord(value)
    || !noteDeletedFilters.includes(value.deleted as NoteDeletedFilter)
    || !noteSorts.includes(value.sort as NoteSort)
    || !isNonNegativeInteger(value.limit)
    || value.limit < 1
    || value.limit > MAX_NOTE_QUERY_LIMIT
    || !isNonNegativeInteger(value.offset)
    || value.offset > MAX_NOTE_QUERY_OFFSET
    || !Array.isArray(value.tags)
  ) return null
  const search = nullableString(value.search)
  const diaryDateFrom = nullableString(value.diary_date_from)
  const diaryDateTo = nullableString(value.diary_date_to)
  const status = value.status === null ? null : typeof value.status === 'string' && noteStatuses.includes(value.status as NoteStatus)
    ? value.status as NoteStatus
    : undefined
  if (
    search === undefined
    || (search !== null && (search.includes('\0') || [...search].length > MAX_NOTE_SEARCH_CHARS))
    || diaryDateFrom === undefined
    || (diaryDateFrom !== null && !validCalendarDate(diaryDateFrom))
    || diaryDateTo === undefined
    || (diaryDateTo !== null && !validCalendarDate(diaryDateTo))
    || status === undefined
    || value.tags.length > MAX_NOTE_TAGS
    || value.tags.some((tag) => typeof tag !== 'string' || tag.includes('\0') || tag.length === 0 || [...tag].length > MAX_NOTE_TAG_CHARS)
  ) return null
  return {
    search,
    diaryDateFrom,
    diaryDateTo,
    tags: value.tags as string[],
    status,
    deleted: value.deleted as NoteDeletedFilter,
    sort: value.sort as NoteSort,
    limit: value.limit,
    offset: value.offset,
  }
}

function noteQueriesEqual(left: NoteQuery, right: NoteQuery): boolean {
  return left.search === right.search
    && left.diaryDateFrom === right.diaryDateFrom
    && left.diaryDateTo === right.diaryDateTo
    && left.status === right.status
    && left.deleted === right.deleted
    && left.sort === right.sort
    && left.limit === right.limit
    && left.offset === right.offset
    && left.tags.length === right.tags.length
    && left.tags.every((tag, index) => tag === right.tags[index])
}

function normalizeNotePage(value: unknown): NotePage | null {
  if (!isRecord(value) || typeof value.has_more !== 'boolean' || !Array.isArray(value.notes)) return null
  const query = normalizeNoteQuery(value.query)
  const nextOffset = nullableNonNegativeInteger(value.next_offset)
  if (!query || nextOffset === undefined || value.notes.length > query.limit) return null
  const notes = value.notes.map(normalizeNoteSummary)
  if (!notes.every((note): note is NoteSummary => note !== null)) return null
  const expectedNext = query.offset + notes.length
  if (value.has_more && (notes.length === 0 || nextOffset !== expectedNext)) return null
  if (!value.has_more && nextOffset !== null) return null
  return { query, notes, hasMore: value.has_more, nextOffset }
}

function normalizeNoteDocument(value: unknown): NoteDocument | null {
  if (!isRecord(value) || typeof value.body_markdown !== 'string') return null
  const summary = normalizeNoteSummary(value.summary)
  if (!summary || new TextEncoder().encode(value.body_markdown).length !== summary.bodyBytes) return null
  return { summary, bodyMarkdown: value.body_markdown }
}

function normalizeNoteMutation(value: unknown): NoteMutationResult | null {
  if (!isRecord(value) || !isRecord(value.value)) return null
  const payload = value.value
  switch (value.kind) {
    case 'stored':
    case 'deleted':
    case 'restored': {
      const note = normalizeNoteSummary(payload)
      return note ? { kind: value.kind, note } : null
    }
    case 'conflict': {
      if (!isRecord(payload) || !isNonNegativeInteger(payload.expected_revision) || payload.expected_revision === 0) return null
      const current = normalizeNoteSummary(payload.current)
      return current
        ? { kind: 'conflict', expectedRevision: payload.expected_revision, current }
        : null
    }
    case 'upload_begun': {
      if (!isRecord(payload) || !isUuid(payload.upload_id) || !isNonNegativeInteger(payload.max_chunk_raw_bytes)) return null
      return { kind: 'upload_begun', uploadId: payload.upload_id, maxChunkRawBytes: payload.max_chunk_raw_bytes }
    }
    case 'upload_accepted': {
      if (
        !isRecord(payload)
        || !isUuid(payload.upload_id)
        || !isNonNegativeInteger(payload.next_sequence)
        || !isNonNegativeInteger(payload.next_offset)
      ) return null
      return {
        kind: 'upload_accepted',
        uploadId: payload.upload_id,
        nextSequence: payload.next_sequence,
        nextOffset: payload.next_offset,
      }
    }
    case 'upload_aborted':
      return isRecord(payload) && isUuid(payload.upload_id)
        ? { kind: 'upload_aborted', uploadId: payload.upload_id }
        : null
    default:
      return null
  }
}

function noteWriteMutationMatches(result: NoteMutationResult, input: NoteWriteInput): boolean {
  if (input.kind === 'create') return result.kind === 'stored'
  if (result.kind === 'stored') return result.note.id === input.id
  return result.kind === 'conflict'
    && result.expectedRevision === input.expectedRevision
    && result.current.id === input.id
}

function noteRevisionMutationMatches(
  result: NoteMutationResult,
  expectedKind: 'deleted' | 'restored',
  id: string,
  expectedRevision: number,
): boolean {
  if (result.kind === expectedKind) return result.note.id === id
  return result.kind === 'conflict'
    && result.expectedRevision === expectedRevision
    && result.current.id === id
}

function normalizeNoteExport(value: unknown): NoteExport | null {
  if (
    !isRecord(value)
    || !noteExportFormats.includes(value.format as NoteExportFormat)
    || typeof value.content !== 'string'
    || !isNonNegativeInteger(value.content_bytes)
    || !isSha256Hex(value.content_sha256)
  ) return null
  if (
    new TextEncoder().encode(value.content).length !== value.content_bytes
    || value.content_bytes > MAX_NOTE_EXPORT_BYTES
  ) return null
  return {
    format: value.format as NoteExportFormat,
    content: value.content,
    contentBytes: value.content_bytes,
    contentSha256: value.content_sha256,
  }
}

function invalidNoteInputError(reason: string): BridgeError {
  return { kind: 'protocol', code: 'invalid_note_input', reason, retryable: false }
}

function invalidNoteResponseError(code: string): BridgeError {
  return { kind: 'protocol', code, reason: code, retryable: false }
}

export async function listNotes(query: NoteQuery): Promise<NoteListResult> {
  const invalid = validNoteQuery(query)
  if (invalid) return { kind: 'error', error: invalidNoteInputError(invalid) }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const page = normalizeNotePage(await invoke<unknown>('notes_list', { query: noteQueryWire(query) }))
    return page && noteQueriesEqual(page.query, query)
      ? { kind: 'page', page }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_list_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_list_unreachable') }
  }
}

export async function getNote(id: string): Promise<NoteGetResult> {
  if (!isUuid(id)) return { kind: 'error', error: invalidNoteInputError('note_id_invalid') }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const document = normalizeNoteDocument(await invoke<unknown>('notes_get', { id }))
    return document && document.summary.id === id
      ? { kind: 'document', document }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_get_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_get_unreachable') }
  }
}

export async function writeNote(input: NoteWriteInput): Promise<NoteMutationFetchResult> {
  const invalid = validNoteWriteInput(input)
  if (invalid) return { kind: 'error', error: invalidNoteInputError(invalid) }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  const args = { meta: noteMetaWire(input.meta), bodyMarkdown: input.bodyMarkdown }
  try {
    let payload: unknown
    if (input.kind === 'create') {
      payload = await invoke<unknown>('notes_upsert', { intent: { kind: 'create' }, ...args })
    } else if (input.autosave) {
      payload = await invoke<unknown>('notes_autosave', {
        id: input.id,
        expectedRevision: input.expectedRevision,
        ...args,
      })
    } else {
      payload = await invoke<unknown>('notes_upsert', {
        intent: {
          kind: 'save',
          id: input.id,
          expected_revision: input.expectedRevision,
          autosave: false,
        },
        ...args,
      })
    }
    const result = normalizeNoteMutation(payload)
    return result && noteWriteMutationMatches(result, input)
      ? { kind: 'mutation', result }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_write_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_write_unreachable') }
  }
}

export async function deleteNote(id: string, expectedRevision: number): Promise<NoteMutationFetchResult> {
  if (!isUuid(id) || !Number.isSafeInteger(expectedRevision) || expectedRevision < 1) {
    return { kind: 'error', error: invalidNoteInputError('note_expected_revision_invalid') }
  }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const result = normalizeNoteMutation(await invoke<unknown>('notes_delete', { id, expectedRevision }))
    return result && noteRevisionMutationMatches(result, 'deleted', id, expectedRevision)
      ? { kind: 'mutation', result }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_delete_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_delete_unreachable') }
  }
}

export async function restoreNote(id: string, expectedRevision: number): Promise<NoteMutationFetchResult> {
  if (!isUuid(id) || !Number.isSafeInteger(expectedRevision) || expectedRevision < 1) {
    return { kind: 'error', error: invalidNoteInputError('note_expected_revision_invalid') }
  }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const result = normalizeNoteMutation(await invoke<unknown>('notes_restore', { id, expectedRevision }))
    return result && noteRevisionMutationMatches(result, 'restored', id, expectedRevision)
      ? { kind: 'mutation', result }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_restore_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_restore_unreachable') }
  }
}

export async function exportNotes(query: NoteQuery, format: NoteExportFormat): Promise<NoteExportResult> {
  const invalid = validNoteQuery(query)
  if (invalid) return { kind: 'error', error: invalidNoteInputError(invalid) }
  if (!noteExportFormats.includes(format)) return { kind: 'error', error: invalidNoteInputError('note_export_format_invalid') }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const exportResult = normalizeNoteExport(await invoke<unknown>('notes_export', {
      query: noteQueryWire(query),
      format,
    }))
    return exportResult && exportResult.format === format
      ? { kind: 'export', export: exportResult }
      : { kind: 'error', error: invalidNoteResponseError('invalid_notes_export_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'notes_export_unreachable') }
  }
}

const MAX_TRANSFER_QUERY_LIMIT = 64
const MAX_TRANSFER_QUERY_OFFSET = 100_000
const MAX_TRANSFER_STATE_FILTERS = 10
const MAX_TRANSFER_REMOTE_PATH_BYTES = 8 * 1024
const MAX_TRANSFER_ETAG_BYTES = 4 * 1024
const MAX_TRANSFER_REASON_BYTES = 96
const MAX_RETRY_ATTEMPTS = 20
const MAX_BANDWIDTH_BYTES_PER_SECOND = 1024 * 1024 * 1024 * 1024
const MAX_LOCAL_HANDLE_DISPLAY_NAME_BYTES = 255

function byteLength(value: string): number {
  return new TextEncoder().encode(value).length
}

function validReason(value: unknown): value is string {
  return typeof value === 'string'
    && value.length > 0
    && byteLength(value) <= MAX_TRANSFER_REASON_BYTES
    && /^[a-z0-9_.-]+$/.test(value)
}

function validRemotePath(value: unknown): value is string {
  return typeof value === 'string'
    && value.length > 0
    && byteLength(value) <= MAX_TRANSFER_REMOTE_PATH_BYTES
    && ![...value].some((char) => /\p{Cc}/u.test(char))
}

function normalizeObjectIdentity(value: unknown): ObjectIdentity | null {
  if (!isRecord(value)) return null
  const sizeBytes = nullableNonNegativeInteger(value.size_bytes)
  const modifiedAtUnixMs = nullableSafeInteger(value.modified_at_unix_ms)
  const etag = nullableString(value.etag)
  if (
    sizeBytes === undefined
    || modifiedAtUnixMs === undefined
    || etag === undefined
    || (etag !== null && (
      byteLength(etag) > MAX_TRANSFER_ETAG_BYTES
      || [...etag].some((char) => /\p{Cc}/u.test(char))
    ))
  ) return null
  return { sizeBytes, modifiedAtUnixMs, etag }
}

function validObjectIdentity(value: unknown): value is ObjectIdentity {
  if (!isRecord(value)) return false
  const sizeBytes = nullableNonNegativeInteger(value.sizeBytes)
  const modifiedAtUnixMs = nullableSafeInteger(value.modifiedAtUnixMs)
  const etag = nullableString(value.etag)
  return sizeBytes !== undefined
    && modifiedAtUnixMs !== undefined
    && etag !== undefined
    && (etag === null || (
      byteLength(etag) <= MAX_TRANSFER_ETAG_BYTES
      && ![...etag].some((char) => /\p{Cc}/u.test(char))
    ))
}

function normalizeRetryPolicy(value: unknown): TransferRetryPolicy | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.max_attempts)
    || value.max_attempts < 1
    || value.max_attempts > MAX_RETRY_ATTEMPTS
    || !isNonNegativeInteger(value.initial_backoff_ms)
    || value.initial_backoff_ms < 1
    || !isNonNegativeInteger(value.max_backoff_ms)
    || value.max_backoff_ms < value.initial_backoff_ms
    || value.max_backoff_ms > 24 * 60 * 60 * 1000
  ) return null
  return {
    maxAttempts: value.max_attempts,
    initialBackoffMs: value.initial_backoff_ms,
    maxBackoffMs: value.max_backoff_ms,
  }
}

function normalizeFeatureSupport(value: unknown): TransferFeatureSupport | null {
  if (!isRecord(value) || value.status !== 'supported' && value.status !== 'unsupported') return null
  if (value.status === 'supported') return { status: 'supported' }
  return validReason(value.reason)
    ? { status: 'unsupported', reason: value.reason }
    : null
}

function normalizeFeatureSet(value: unknown): TransferFeatureSet | null {
  if (!isRecord(value)) return null
  const pause = normalizeFeatureSupport(value.pause)
  const resume = normalizeFeatureSupport(value.resume)
  let resumeValidation: TransferFeatureSet['resumeValidation'] | undefined
  if (value.resume_validation === null) {
    resumeValidation = null
  } else if (isEnumValue(resumeValidationValues, value.resume_validation)) {
    resumeValidation = value.resume_validation
  }
  if (!pause || !resume || resumeValidation === undefined) return null
  if (resume.status === 'supported' && resumeValidation === null) return null
  if (resume.status === 'unsupported' && resumeValidation !== null) return null
  return { pause, resume, resumeValidation }
}

function normalizeProgress(value: unknown): TransferProgress | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.bytes_transferred)
    || !isNonNegativeInteger(value.total_bytes ?? 0)
    || value.total_bytes !== null && !isNonNegativeInteger(value.total_bytes)
  ) return null
  if (
    value.bytes_per_second !== null && !isNonNegativeInteger(value.bytes_per_second)
    || value.sampled_at_unix_ms !== null && !isSafeInteger(value.sampled_at_unix_ms)
  ) return null
  return {
    bytesTransferred: value.bytes_transferred,
    totalBytes: value.total_bytes,
    bytesPerSecond: value.bytes_per_second,
    sampledAtUnixMs: value.sampled_at_unix_ms,
  }
}

function normalizeCheckpoint(value: unknown): TransferCheckpoint | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.offset)
    || !isEnumValue(verificationLevels, value.verification)
    || !isSafeInteger(value.verified_at_unix_ms)
  ) return null
  let sourceIdentity: ObjectIdentity | null = null
  if (value.source_identity !== null) {
    const normalized = normalizeObjectIdentity(value.source_identity)
    if (!normalized) return null
    sourceIdentity = normalized
  }
  let destinationIdentity: ObjectIdentity | null = null
  if (value.destination_identity !== null) {
    const normalized = normalizeObjectIdentity(value.destination_identity)
    if (!normalized) return null
    destinationIdentity = normalized
  }
  return {
    offset: value.offset,
    sourceIdentity,
    destinationIdentity,
    verification: value.verification,
    verifiedAtUnixMs: value.verified_at_unix_ms,
  }
}

function normalizeFailure(value: unknown): TransferFailure | null {
  if (
    !isRecord(value)
    || !isEnumValue(remoteErrorKinds, value.kind)
    || !isEnumValue(remoteOperations, value.operation)
    || !validReason(value.reason)
    || !isEnumValue(retryDispositions, value.retry)
  ) return null
  return {
    kind: value.kind,
    operation: value.operation,
    reason: value.reason,
    retry: value.retry,
  }
}

function normalizeCompletion(value: unknown): TransferCompletion | null {
  if (
    !isRecord(value)
    || !isEnumValue(verificationLevels, value.verification)
    || !isSafeInteger(value.completed_at_unix_ms)
  ) return null
  let identity: ObjectIdentity | null = null
  if (value.identity !== null) {
    const normalized = normalizeObjectIdentity(value.identity)
    if (!normalized) return null
    identity = normalized
  }
  return { verification: value.verification, identity, completedAtUnixMs: value.completed_at_unix_ms }
}

function normalizeConflict(value: unknown): TransferConflict | null {
  if (!isRecord(value) || !validReason(value.reason)) return null
  if (value.checkpoint === null) return { reason: value.reason, checkpoint: null }
  const checkpoint = normalizeCheckpoint(value.checkpoint)
  return checkpoint ? { reason: value.reason, checkpoint } : null
}

function normalizeTransferState(value: unknown): TransferState | null {
  if (!isRecord(value) || typeof value.status !== 'string') return null
  switch (value.status) {
    case 'queued':
    case 'running':
    case 'pausing':
    case 'cancelling':
      return { status: value.status }
    case 'paused': {
      const checkpoint = normalizeCheckpoint(value.checkpoint)
      return checkpoint ? { status: 'paused', checkpoint } : null
    }
    case 'retry_scheduled': {
      if (!isSafeInteger(value.not_before_unix_ms)) return null
      const failure = normalizeFailure(value.failure)
      return failure ? { status: 'retry_scheduled', notBeforeUnixMs: value.not_before_unix_ms, failure } : null
    }
    case 'conflict': {
      const conflict = normalizeConflict(value.conflict)
      return conflict ? { status: 'conflict', conflict } : null
    }
    case 'completed': {
      const completion = normalizeCompletion(value.completion)
      return completion ? { status: 'completed', completion } : null
    }
    case 'failed': {
      const failure = normalizeFailure(value.failure)
      return failure ? { status: 'failed', failure } : null
    }
    case 'cancelled': {
      if (!isSafeInteger(value.cancelled_at_unix_ms)) return null
      if (value.checkpoint === null) {
        return { status: 'cancelled', checkpoint: null, cancelledAtUnixMs: value.cancelled_at_unix_ms }
      }
      const checkpoint = normalizeCheckpoint(value.checkpoint)
      return checkpoint
        ? { status: 'cancelled', checkpoint, cancelledAtUnixMs: value.cancelled_at_unix_ms }
        : null
    }
    default:
      return null
  }
}

function normalizeTransferEndpoint(value: unknown): TransferEndpoint | null {
  if (!isRecord(value)) return null
  if (value.kind === 'local') {
    return isUuid(value.handle) ? { kind: 'local', handle: value.handle } : null
  }
  if (
    value.kind === 'remote'
    && isUuid(value.profile_id)
    && isEnumValue(remoteProtocols, value.protocol)
    && validRemotePath(value.path)
  ) {
    return { kind: 'remote', profileId: value.profile_id, protocol: value.protocol, path: value.path }
  }
  return null
}

function normalizeTransferTask(value: unknown): TransferTask | null {
  if (
    !isRecord(value)
    || !isUuid(value.id)
    || !isEnumValue(transferDirections, value.direction)
    || !isNonNegativeInteger(value.completed_attempts)
    || !isNonNegativeInteger(value.revision)
    || !isSafeInteger(value.created_at_unix_ms)
    || !isSafeInteger(value.updated_at_unix_ms)
  ) return null
  const source = normalizeTransferEndpoint(value.source)
  const destination = normalizeTransferEndpoint(value.destination)
  const state = normalizeTransferState(value.state)
  const progress = normalizeProgress(value.progress)
  const retryPolicy = normalizeRetryPolicy(value.retry_policy)
  const features = normalizeFeatureSet(value.features)
  const expectedSource = value.expected_source === null ? null : normalizeObjectIdentity(value.expected_source)
  const expectedDestination = value.expected_destination === null ? null : normalizeObjectIdentity(value.expected_destination)
  if (
    !source || !destination || !state || !progress || !retryPolicy || !features
    || value.expected_source !== null && expectedSource === null
    || value.expected_destination !== null && expectedDestination === null
  ) return null
  const localCount = Number(source.kind === 'local') + Number(destination.kind === 'local')
  const remoteCount = Number(source.kind === 'remote') + Number(destination.kind === 'remote')
  const directionMatches = value.direction === 'upload'
    ? source.kind === 'local' && destination.kind === 'remote'
    : source.kind === 'remote' && destination.kind === 'local'
  if (localCount !== 1 || remoteCount !== 1 || !directionMatches) return null
  const bandwidthLimit = value.bandwidth_limit === null
    ? null
    : isNonNegativeInteger(value.bandwidth_limit) && value.bandwidth_limit >= 1
      ? value.bandwidth_limit
      : undefined
  if (
    bandwidthLimit === undefined
    || bandwidthLimit !== null && bandwidthLimit > MAX_BANDWIDTH_BYTES_PER_SECOND
    || !isEnumValue(conflictPolicies, value.conflict_policy)
    || value.completed_attempts > retryPolicy.maxAttempts
    || value.created_at_unix_ms < 0
    || value.updated_at_unix_ms < value.created_at_unix_ms
    || progress.totalBytes !== null && progress.bytesTransferred > progress.totalBytes
    || progress.bytesPerSecond !== null && progress.bytesPerSecond > MAX_BANDWIDTH_BYTES_PER_SECOND
  ) return null
  return {
    id: value.id,
    source,
    destination,
    direction: value.direction,
    expectedSource,
    expectedDestination,
    state,
    progress,
    retryPolicy,
    completedAttempts: value.completed_attempts,
    bandwidthLimit,
    conflictPolicy: value.conflict_policy,
    features,
    revision: value.revision,
    createdAtMs: value.created_at_unix_ms,
    updatedAtMs: value.updated_at_unix_ms,
  }
}

function normalizeTransferQuery(value: unknown): TransferQuery | null {
  if (
    !isRecord(value)
    || !isNonNegativeInteger(value.limit)
    || value.limit < 1
    || value.limit > MAX_TRANSFER_QUERY_LIMIT
    || !isNonNegativeInteger(value.offset)
    || value.offset > MAX_TRANSFER_QUERY_OFFSET
    || !Array.isArray(value.states)
  ) return null
  const direction = value.direction === null
    ? null
    : isEnumValue(transferDirections, value.direction)
      ? value.direction
      : undefined
  const profileId = value.profile_id === null ? null : typeof value.profile_id === 'string' && isUuid(value.profile_id)
    ? value.profile_id
    : undefined
  if (
    direction === undefined
    || profileId === undefined
    || value.states.length > MAX_TRANSFER_STATE_FILTERS
    || value.states.some((state, index) => (
      !isEnumValue(transferStateKinds, state)
      || value.states.indexOf(state) !== index
    ))
  ) return null
  return {
    limit: value.limit,
    offset: value.offset,
    states: value.states as TransferStateKind[],
    direction,
    profileId,
  }
}

function transferQueriesEqual(left: TransferQuery, right: TransferQuery): boolean {
  return left.limit === right.limit
    && left.offset === right.offset
    && left.direction === right.direction
    && left.profileId === right.profileId
    && left.states.length === right.states.length
    && left.states.every((state, index) => state === right.states[index])
}

function normalizeTransferPage(value: unknown): TransferPage | null {
  if (!isRecord(value) || typeof value.has_more !== 'boolean' || !Array.isArray(value.tasks)) return null
  const query = normalizeTransferQuery(value.query)
  const nextOffset = nullableNonNegativeInteger(value.next_offset)
  if (!query || nextOffset === undefined || value.tasks.length > query.limit) return null
  const tasks = value.tasks.map(normalizeTransferTask)
  if (!tasks.every((task): task is TransferTask => task !== null)) return null
  if (!tasks.every((task) => (
    (query.states.length === 0 || query.states.includes(task.state.status))
    && (query.direction === null || task.direction === query.direction)
    && (query.profileId === null
      || task.source.kind === 'remote' && task.source.profileId === query.profileId
      || task.destination.kind === 'remote' && task.destination.profileId === query.profileId)
  ))) return null
  const expectedNext = query.offset + tasks.length
  if (value.has_more && (tasks.length === 0 || nextOffset !== expectedNext)) return null
  if (!value.has_more && nextOffset !== null) return null
  return { query, tasks, hasMore: value.has_more, nextOffset }
}

function normalizeTransferMutation(
  value: unknown,
  id: TransferId,
  expectedRevision: number,
): TransferMutationResult | null {
  if (!isRecord(value)) return null
  if (value.result === 'updated') {
    const task = normalizeTransferTask(value.task)
    return task && task.id === id && task.revision >= expectedRevision
      ? { result: 'updated', task }
      : null
  }
  if (
    value.result === 'conflict'
    && isNonNegativeInteger(value.expected_revision)
    && value.expected_revision === expectedRevision
  ) {
    const current = normalizeTransferTask(value.current)
    return current && current.id === id && current.revision !== expectedRevision
      ? { result: 'conflict', expectedRevision: value.expected_revision, current }
      : null
  }
  return null
}

function normalizeTransferLocalHandleGrant(value: unknown): TransferLocalHandleGrant | null {
  if (
    !isRecord(value)
    || !isUuid(value.handle)
    || !isEnumValue(localHandlePurposes, value.purpose)
    || typeof value.display_name !== 'string'
    || value.display_name.length === 0
    || byteLength(value.display_name) > MAX_LOCAL_HANDLE_DISPLAY_NAME_BYTES
    || [...value.display_name].some((char) => char === '/' || char === '\\' || char === '\0' || /\p{C}/u.test(char))
  ) return null
  const sizeBytes = value.size_bytes === null
    ? null
    : isNonNegativeInteger(value.size_bytes)
      ? value.size_bytes
      : undefined
  if (sizeBytes === undefined || value.purpose === 'download_destination' && sizeBytes !== null) return null
  return { handle: value.handle, purpose: value.purpose, displayName: value.display_name, sizeBytes }
}

function validTransferEndpoint(endpoint: TransferDraft['source']): string | null {
  if (endpoint.kind === 'local') {
    return isUuid(endpoint.handle) ? null : 'transfer_local_handle_invalid'
  }
  if (!isUuid(endpoint.profileId)) return 'transfer_profile_id_invalid'
  return validRemotePath(endpoint.path) ? null : 'transfer_remote_path_invalid'
}

function validTransferDraft(draft: TransferDraft): string | null {
  if (!isUuid(draft.id)) return 'transfer_id_invalid'
  if (!transferDirections.includes(draft.direction)) return 'transfer_direction_invalid'
  const sourceInvalid = validTransferEndpoint(draft.source)
  const destinationInvalid = validTransferEndpoint(draft.destination)
  if (sourceInvalid) return sourceInvalid
  if (destinationInvalid) return destinationInvalid
  const localCount = Number(draft.source.kind === 'local') + Number(draft.destination.kind === 'local')
  const remoteCount = Number(draft.source.kind === 'remote') + Number(draft.destination.kind === 'remote')
  if (localCount !== 1 || remoteCount !== 1) return 'transfer_endpoints_mismatch'
  const directionMatches = draft.direction === 'upload'
    ? draft.source.kind === 'local' && draft.destination.kind === 'remote'
    : draft.source.kind === 'remote' && draft.destination.kind === 'local'
  if (!directionMatches) return 'transfer_endpoints_mismatch'
  if (draft.expectedSource !== null && !validObjectIdentity(draft.expectedSource)) {
    return 'transfer_expected_source_invalid'
  }
  if (draft.expectedDestination !== null && !validObjectIdentity(draft.expectedDestination)) {
    return 'transfer_expected_destination_invalid'
  }
  const { maxAttempts, initialBackoffMs, maxBackoffMs } = draft.retryPolicy
  if (
    !isNonNegativeInteger(maxAttempts)
    || maxAttempts < 1
    || maxAttempts > MAX_RETRY_ATTEMPTS
    || !isNonNegativeInteger(initialBackoffMs)
    || initialBackoffMs < 1
    || !isNonNegativeInteger(maxBackoffMs)
    || maxBackoffMs < initialBackoffMs
    || maxBackoffMs > 24 * 60 * 60 * 1000
  ) return 'transfer_retry_policy_invalid'
  if (
    draft.bandwidthLimit !== null
    && (!isNonNegativeInteger(draft.bandwidthLimit) || draft.bandwidthLimit < 1 || draft.bandwidthLimit > MAX_BANDWIDTH_BYTES_PER_SECOND)
  ) return 'transfer_bandwidth_limit_invalid'
  if (!conflictPolicies.includes(draft.conflictPolicy)) return 'transfer_conflict_policy_invalid'
  return null
}

function validTransferQuery(query: TransferQuery): string | null {
  if (!isNonNegativeInteger(query.limit) || query.limit < 1 || query.limit > MAX_TRANSFER_QUERY_LIMIT) {
    return 'transfer_query_limit_invalid'
  }
  if (!isNonNegativeInteger(query.offset) || query.offset > MAX_TRANSFER_QUERY_OFFSET) {
    return 'transfer_query_offset_invalid'
  }
  if (query.states.length > MAX_TRANSFER_STATE_FILTERS) return 'transfer_state_filters_invalid'
  if (query.states.some((state, index) => (
    !isEnumValue(transferStateKinds, state) || query.states.indexOf(state) !== index
  ))) return 'transfer_state_filters_invalid'
  if (query.direction !== null && !transferDirections.includes(query.direction)) {
    return 'transfer_direction_invalid'
  }
  if (query.profileId !== null && !isUuid(query.profileId)) return 'transfer_profile_id_invalid'
  return null
}

function transferEndpointWire(endpoint: TransferDraft['source']): Record<string, unknown> {
  return endpoint.kind === 'local'
    ? { kind: 'local', handle: endpoint.handle }
    : { kind: 'remote', profile_id: endpoint.profileId, path: endpoint.path }
}

function transferIdentityWire(identity: ObjectIdentity | null): Record<string, unknown> | null {
  return identity === null
    ? null
    : {
        size_bytes: identity.sizeBytes,
        modified_at_unix_ms: identity.modifiedAtUnixMs,
        etag: identity.etag,
      }
}

function transferDraftWire(draft: TransferDraft): Record<string, unknown> {
  return {
    id: draft.id,
    source: transferEndpointWire(draft.source),
    destination: transferEndpointWire(draft.destination),
    direction: draft.direction,
    expected_source: transferIdentityWire(draft.expectedSource),
    expected_destination: transferIdentityWire(draft.expectedDestination),
    retry_policy: {
      max_attempts: draft.retryPolicy.maxAttempts,
      initial_backoff_ms: draft.retryPolicy.initialBackoffMs,
      max_backoff_ms: draft.retryPolicy.maxBackoffMs,
    },
    bandwidth_limit: draft.bandwidthLimit,
    conflict_policy: draft.conflictPolicy,
  }
}

function transferQueryWire(query: TransferQuery): Record<string, unknown> {
  return {
    limit: query.limit,
    offset: query.offset,
    states: query.states,
    direction: query.direction,
    profile_id: query.profileId,
  }
}

function invalidTransferInputError(reason: string): BridgeError {
  return { kind: 'protocol', code: 'invalid_transfer_input', reason, retryable: false }
}

function invalidTransferResponseError(code: string): BridgeError {
  return { kind: 'protocol', code, reason: code, retryable: false }
}

export async function listTransfers(query: TransferQuery): Promise<TransferListResult> {
  const invalid = validTransferQuery(query)
  if (invalid) return { kind: 'error', error: invalidTransferInputError(invalid) }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const page = normalizeTransferPage(await invoke<unknown>('transfer_list', { query: transferQueryWire(query) }))
    return page && transferQueriesEqual(page.query, query)
      ? { kind: 'page', page }
      : { kind: 'error', error: invalidTransferResponseError('invalid_transfer_list_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'transfer_list_unreachable') }
  }
}

export async function getTransfer(id: TransferId): Promise<TransferTaskResult> {
  if (!isUuid(id)) return { kind: 'error', error: invalidTransferInputError('transfer_id_invalid') }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const task = normalizeTransferTask(await invoke<unknown>('transfer_get', { id }))
    return task && task.id === id
      ? { kind: 'task', task }
      : { kind: 'error', error: invalidTransferResponseError('invalid_transfer_get_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'transfer_get_unreachable') }
  }
}

export async function enqueueTransfer(draft: TransferDraft): Promise<TransferTaskResult> {
  const invalid = validTransferDraft(draft)
  if (invalid) return { kind: 'error', error: invalidTransferInputError(invalid) }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const task = normalizeTransferTask(await invoke<unknown>('transfer_enqueue', { draft: transferDraftWire(draft) }))
    return task && task.id === draft.id
      ? { kind: 'task', task }
      : { kind: 'error', error: invalidTransferResponseError('invalid_transfer_enqueue_response') }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, 'transfer_enqueue_unreachable') }
  }
}

async function transferMutation(
  command: string,
  id: TransferId,
  expectedRevision: number,
  extra: Record<string, unknown> = {},
): Promise<TransferMutationFetchResult> {
  if (!isUuid(id) || !Number.isSafeInteger(expectedRevision) || expectedRevision < 0) {
    return { kind: 'error', error: invalidTransferInputError('transfer_expected_revision_invalid') }
  }
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const result = normalizeTransferMutation(await invoke<unknown>(command, {
      id,
      expectedRevision,
      ...extra,
    }), id, expectedRevision)
    return result
      ? { kind: 'mutation', result }
      : { kind: 'error', error: invalidTransferResponseError(`invalid_${command}_response`) }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, `${command}_unreachable`) }
  }
}

export function cancelTransfer(id: TransferId, expectedRevision: number): Promise<TransferMutationFetchResult> {
  return transferMutation('transfer_cancel', id, expectedRevision)
}

export function retryTransfer(id: TransferId, expectedRevision: number): Promise<TransferMutationFetchResult> {
  return transferMutation('transfer_retry', id, expectedRevision)
}

export function resolveTransferConflict(
  id: TransferId,
  expectedRevision: number,
  policy: ConflictPolicy,
): Promise<TransferMutationFetchResult> {
  if (!conflictPolicies.includes(policy)) {
    return Promise.resolve({ kind: 'error', error: invalidTransferInputError('transfer_conflict_policy_invalid') })
  }
  return transferMutation('transfer_resolve_conflict', id, expectedRevision, { policy })
}

async function pickTransferLocalHandle(command: string): Promise<TransferPickResult> {
  if (!isDesktopBridgeAvailable()) {
    return { kind: 'error', error: normalizeBridgeError(null, 'desktop_bridge_unavailable') }
  }
  try {
    const payload = await invoke<unknown>(command)
    if (payload === null) return { kind: 'picked', grant: null }
    const grant = normalizeTransferLocalHandleGrant(payload)
    return grant
      ? { kind: 'picked', grant }
      : { kind: 'error', error: invalidTransferResponseError(`invalid_${command}_response`) }
  } catch (error) {
    return { kind: 'error', error: normalizeBridgeError(error, `${command}_unreachable`) }
  }
}

export function pickUploadSource(): Promise<TransferPickResult> {
  return pickTransferLocalHandle('transfer_pick_upload_source')
}

export function pickDownloadDestination(): Promise<TransferPickResult> {
  return pickTransferLocalHandle('transfer_pick_download_destination')
}
