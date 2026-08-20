import { beforeEach, describe, expect, it, vi } from 'vitest'

import { invoke } from '@tauri-apps/api/core'
import type { BackendStatus } from './types'

import {
  cancelTransfer,
  closeRemoteTerminal,
  connectRemoteSession,
  createRemoteDirectory,
  deleteRemoteEntry,
  deleteRemoteProfile,
  deleteRemoteSecret,
  deleteNote,
  captureJournalKnowledge,
  collectJournalUsage,
  enqueueTransfer,
  exportNotes,
  fetchJournalSummary,
  getBackendCapabilityReport,
  getBackendHealth,
  getNote,
  getNetworkSnapshot,
  getRemoteAdapterCatalog,
  getRemoteProfiles,
  getSystemInfo,
  getTelemetrySnapshot,
  getUsageSummary,
  getTransfer,
  listTransfers,
  listNotes,
  listRemoteDirectory,
  normalizeSpeedTestBasicEnd,
  normalizeSpeedTestDeepOutput,
  openRemoteTerminal,
  pickDownloadDestination,
  pickUploadSource,
  resolveTransferConflict,
  renameRemoteEntry,
  readRemoteTerminal,
  restoreNote,
  resizeRemoteTerminal,
  retryTransfer,
  pollRemoteTerminal,
  storeRemoteSecret,
  streamRemoteTerminal,
  upsertRemoteProfile,
  writeRemoteTerminal,
  writeNote,
} from './backend'

const channelTestState = vi.hoisted(() => ({
  channels: [] as Array<{ onmessage: (value: unknown) => void }>,
}))

vi.mock('@tauri-apps/api/core', () => ({
  Channel: class {
    onmessage = (_value: unknown) => {}

    constructor() {
      channelTestState.channels.push(this)
    }
  },
  invoke: vi.fn(),
}))

const mockedInvoke = vi.mocked(invoke)

const metric = (value: number | null, state = value === null ? 'unknown' : 'known') => ({
  value,
  state,
  ...(value === null ? { reason: 'metric_unknown' } : {}),
})

function networkRate(state: 'known' | 'warming_up' = 'known') {
  return {
    rx_bytes_per_second: state === 'known' ? 2048 : null,
    tx_bytes_per_second: state === 'known' ? 1024 : null,
    state,
    reason: state === 'known' ? 'aggregate_rate_known' : 'aggregate_rate_warming_up',
  }
}

function networkPayload() {
  return {
    schema_version: 1,
    snapshot_id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    captured_at_unix_ms: 1_786_154_400_000,
    observed_boottime_ms: 10_000,
    sample_interval_ms: 1_000,
    last_success_at_unix_ms: 1_786_154_400_000,
    freshness: 'fresh',
    retryable: false,
    system_traffic: { status: 'healthy', reason: 'network_system_traffic_available' },
    per_application: { status: 'unsupported', reason: 'unprivileged_bpf_permanently_disabled' },
    coverage: {
      reported_interfaces: 1,
      interfaces_with_counters: 1,
      includes_loopback: false,
      includes_tunnels: false,
      layered_accounting: 'not_detected',
      reason: 'all_reported_interfaces_have_counters',
    },
    totals: {
      scope: 'inclusive_interfaces',
      all_interfaces: { rx_bytes: 4096, tx_bytes: 2048 },
      physical: { rx_bytes: 4096, tx_bytes: 2048 },
      loopback: { rx_bytes: 0, tx_bytes: 0 },
      tunnel: { rx_bytes: 0, tx_bytes: 0 },
      other_virtual: { rx_bytes: 0, tx_bytes: 0 },
    },
    aggregate_rate: networkRate(),
    interfaces: [{
      index: 2,
      name: 'enp1s0',
      kind: 'physical',
      kernel_kind: null,
      is_up: true,
      carrier_up: true,
      counters: { rx_bytes: 4096, tx_bytes: 2048 },
      rate: networkRate(),
      transition: 'stable',
    }],
    applications: [],
  }
}

function telemetryPayload() {
  return {
    schema_version: 4,
    snapshot_id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    captured_at_unix_ms: 1_786_154_400_000,
    sample_interval_ms: 2_000,
    logical_cpu_count: 16,
    freshness: 'fresh',
    status: 'complete',
    reason: 'telemetry_healthy',
    retryable: false,
    scope: 'same_euid',
    last_success_at_unix_ms: 1_786_154_400_000,
    permission_denied_counts: [],
    issues: [],
    system_fd: {
      scope: 'system',
      file_nr_allocated: metric(10),
      file_nr_max: metric(0),
      file_max: metric(100),
      pressure_percent: metric(10),
    },
    applications: [
      {
        application_key: 'org.example.App',
        desktop_entry_id: 'org.example.App.desktop',
        display_label: 'Example App',
        grouping_resolution: 'desktop_entry_exact',
        process_count: 2,
        process_scope: 'same_euid',
        cgroup_scope: 'full_cgroup',
        cpu_percent_total_capacity_sum: metric(12.5),
        cgroup_cpu_percent_total_capacity: metric(10),
        rss_sum_bytes: metric(4096),
        pss_sum_bytes: metric(3072),
        memory_current_bytes: metric(8192),
        cgroup_process_count: metric(3),
        fd_used_sum: metric(4),
        fd_soft_limit_sum: metric(1024),
        fd_percent_of_attributed_sum: metric(50),
        fd_percent_of_soft_limit_sum: metric(0.390625),
        fd_max_process_percent_of_soft_limit: metric(90),
      },
    ],
  }
}

describe('getBackendHealth', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  const healthPayload = (status: BackendStatus, reason: string) => ({
    daemon_version: '0.1.0',
    health: 'degraded',
    reason: 'telemetry_partial',
    capabilities: [
      { id: 'appd.health.v1', status, reason },
      { id: 'telemetry.snapshot.v1', status: 'degraded', reason: 'telemetry_partial' },
    ],
  })

  it.each([
    ['healthy', 'appd_online'],
    ['degraded', 'appd_degraded'],
    ['unsupported', 'appd_unsupported'],
    ['unreachable', 'appd_unreachable'],
  ] as const)('maps appd.health.v1 status=%s and reason', async (status, reason) => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(healthPayload(status, reason))

    await expect(getBackendHealth()).resolves.toEqual({
      status,
      capabilityReason: reason,
    })
    expect(mockedInvoke).toHaveBeenCalledWith('appd_health')
  })

  it('does not present aggregate telemetry degradation as a bridge failure', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(healthPayload('healthy', 'appd_online'))

    await expect(getBackendHealth()).resolves.toEqual({
      status: 'healthy',
      capabilityReason: 'appd_online',
    })
  })

  it.each([
    null,
    'invalid',
    {},
    { health: 'healthy', reason: 'aggregate_only' },
    {
      daemon_version: '0.1.0',
      health: 'degraded',
      reason: 'telemetry_partial',
      capabilities: [
        { id: 'telemetry.snapshot.v1', status: 'degraded', reason: 'telemetry_partial' },
      ],
    },
  ])(
    'returns degraded for an invalid health response: %s',
    async (payload) => {
      window.__TAURI_INTERNALS__ = {}
      mockedInvoke.mockResolvedValue(payload)

      await expect(getBackendHealth()).resolves.toEqual({
        status: 'degraded',
        capabilityReason: 'invalid_health_response',
      })
    },
  )

  it('returns unsupported when the desktop bridge is unavailable', async () => {
    await expect(getBackendHealth()).resolves.toEqual({
      status: 'unsupported',
      capabilityReason: 'desktop_bridge_unavailable',
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('returns unreachable when appd_health invocation fails', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockRejectedValue(new Error('bridge failure'))

    await expect(getBackendHealth()).resolves.toEqual({
      status: 'unreachable',
      capabilityReason: 'appd_health_unreachable',
    })
  })

  it('preserves a typed bridge reason when health invocation fails', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockRejectedValue({
      kind: 'transport',
      code: 'appd_socket_unsafe',
      reason: 'appd_socket_unsafe',
      retryable: false,
    })

    await expect(getBackendHealth()).resolves.toEqual({
      status: 'unreachable',
      capabilityReason: 'appd_socket_unsafe',
    })
  })
})

describe('getBackendCapabilityReport', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  const payload = () => ({
    daemon_version: '0.1.0',
    health: 'degraded',
    reason: 'telemetry_partial',
    capabilities: [
      { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
      { id: 'telemetry.snapshot.v1', status: 'degraded', reason: 'telemetry_partial' },
      { id: 'network.per_app.v1', status: 'unsupported', reason: 'unprivileged_bpf_permanently_disabled' },
    ],
  })

  it('preserves the ordered runtime capability catalog', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(payload())

    await expect(getBackendCapabilityReport()).resolves.toEqual({
      kind: 'report',
      report: {
        daemonVersion: '0.1.0',
        health: { status: 'degraded', capabilityReason: 'telemetry_partial' },
        capabilities: [
          { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
          { id: 'telemetry.snapshot.v1', status: 'degraded', reason: 'telemetry_partial' },
          { id: 'network.per_app.v1', status: 'unsupported', reason: 'unprivileged_bpf_permanently_disabled' },
        ],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('appd_health')
  })

  it('rejects duplicate capability ids', async () => {
    window.__TAURI_INTERNALS__ = {}
    const duplicate = payload()
    duplicate.capabilities.push({ ...duplicate.capabilities[0] })
    mockedInvoke.mockResolvedValue(duplicate)

    await expect(getBackendCapabilityReport()).resolves.toMatchObject({
      kind: 'error',
      error: { kind: 'protocol', reason: 'invalid_capability_report', retryable: false },
    })
  })

  it('returns a typed transport error outside Tauri', async () => {
    await expect(getBackendCapabilityReport()).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'desktop_bridge_unavailable',
        reason: 'desktop_bridge_unavailable',
        retryable: true,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})

describe('getTelemetrySnapshot', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  it('normalizes schema v4 without inventing missing metrics', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = telemetryPayload()
    payload.applications[0].rss_sum_bytes = metric(null)
    mockedInvoke.mockResolvedValue(payload)

    const result = await getTelemetrySnapshot()

    expect(result.kind).toBe('snapshot')
    if (result.kind !== 'snapshot') throw new Error('expected snapshot')
    expect(result.snapshot.schemaVersion).toBe(4)
    expect(result.snapshot.applications[0].rssBytes).toEqual({
      value: null,
      state: 'unknown',
      reason: 'metric_unknown',
    })
    expect(mockedInvoke).toHaveBeenCalledWith('telemetry_snapshot')
  })

  it.each([
    { field: 'schema_version', value: 1 },
    { field: 'freshness', value: 'recent' },
    { field: 'captured_at_unix_ms', value: 'now' },
  ])('rejects an invalid $field', async ({ field, value }) => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({ ...telemetryPayload(), [field]: value })

    await expect(getTelemetrySnapshot()).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'invalid_telemetry_response',
        reason: 'invalid_telemetry_response',
        retryable: false,
      },
    })
  })

  it('rejects known metrics without values and unknown metrics with values', async () => {
    window.__TAURI_INTERNALS__ = {}
    const knownWithoutValue = telemetryPayload()
    knownWithoutValue.applications[0].fd_used_sum = metric(null, 'known')
    mockedInvoke.mockResolvedValueOnce(knownWithoutValue)

    const unknownWithValue = telemetryPayload()
    unknownWithValue.applications[0].fd_used_sum = metric(4, 'unknown')
    mockedInvoke.mockResolvedValueOnce(unknownWithValue)

    await expect(getTelemetrySnapshot()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_telemetry_response' },
    })
    await expect(getTelemetrySnapshot()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_telemetry_response' },
    })
  })

  it('preserves typed daemon failures', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockRejectedValue({
      kind: 'daemon',
      code: 'collector_unavailable',
      reason: 'helper_missing',
      retryable: true,
    })

    await expect(getTelemetrySnapshot()).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'daemon',
        code: 'collector_unavailable',
        reason: 'helper_missing',
        retryable: true,
      },
    })
  })

  it('returns a typed transport error outside Tauri', async () => {
    await expect(getTelemetrySnapshot()).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'desktop_bridge_unavailable',
        reason: 'desktop_bridge_unavailable',
        retryable: true,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})

describe('getNetworkSnapshot', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  it('normalizes schema v1 and preserves unknown rates as null', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = networkPayload()
    payload.freshness = 'warming_up'
    payload.system_traffic = { status: 'degraded', reason: 'network_snapshot_pending' }
    payload.aggregate_rate = networkRate('warming_up')
    payload.interfaces[0].rate = networkRate('warming_up')
    mockedInvoke.mockResolvedValue(payload)

    const result = await getNetworkSnapshot()

    expect(result.kind).toBe('snapshot')
    if (result.kind !== 'snapshot') throw new Error('expected network snapshot')
    expect(result.snapshot.aggregateRate).toMatchObject({
      state: 'warming_up',
      rxBytesPerSecond: null,
      txBytesPerSecond: null,
    })
    expect(result.snapshot.interfaces[0].rate.rxBytesPerSecond).toBeNull()
    expect(mockedInvoke).toHaveBeenCalledWith('network_snapshot')
  })

  it('accepts unsupported per-app capability only with no records', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(networkPayload())

    const result = await getNetworkSnapshot()
    expect(result).toMatchObject({
      kind: 'snapshot',
      snapshot: {
        perApplication: {
          status: 'unsupported',
          reason: 'unprivileged_bpf_permanently_disabled',
        },
        applications: [],
      },
    })
  })

  it('rejects unsupported per-app capability with records', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = networkPayload()
    payload.applications = [{
      application_key: 'org.example.App',
      rx_bytes: 1,
      tx_bytes: 2,
      rx_share_percent: 25,
      tx_share_percent: 50,
    }]
    mockedInvoke.mockResolvedValue(payload)

    await expect(getNetworkSnapshot()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_network_response', retryable: false },
    })
  })

  it('rejects coverage that does not match the interface list', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = networkPayload()
    payload.coverage.reported_interfaces = 2
    mockedInvoke.mockResolvedValue(payload)

    await expect(getNetworkSnapshot()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_network_response' },
    })
  })

  it('returns a factual transport error outside Tauri', async () => {
    await expect(getNetworkSnapshot()).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'desktop_bridge_unavailable',
        reason: 'desktop_bridge_unavailable',
        retryable: true,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})

function usagePayload(period: 'daily' | 'weekly' = 'daily') {
  const bucketKey = period === 'daily' ? '2026-08-10' : '2026-W33'
  return {
    schema_version: 3,
    snapshot_id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    captured_at_unix_ms: 1_786_291_200_000,
    query: { period, bucket_key: bucketKey },
    status: 'healthy',
    reason: 'usage_available',
    retryable: false,
    coverage: {
      status: 'healthy',
      reason: 'usage_tracking_active',
      niri_event_stream_connected: true,
      logind_session_available: true,
      event_gap_count: 0,
      last_checkpoint_unix_ms: 1_786_291_200_000,
      tracking_started_unix_ms: 1_786_204_800_000,
      bucket_start_covered: true,
      definition: 'foreground_unlocked_input_active_300s_monotonic',
    },
    applications: [{
      app_id: 'org.example.Editor',
      bucket_key: bucketKey,
      timezone_id: 'Asia/Shanghai',
      utc_offset_seconds: 28_800,
      duration_ns: 3_600_000_000_000,
      last_wall_utc_ms: 1_786_291_200_000,
    }],
  }
}

describe('getUsageSummary', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  it('normalizes a healthy daily summary without changing monotonic duration', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(usagePayload())

    const result = await getUsageSummary({ period: 'daily', bucketKey: '2026-08-10' })

    expect(result).toMatchObject({
      kind: 'summary',
      summary: {
        query: { period: 'daily', bucketKey: '2026-08-10' },
        coverage: {
          definition: 'foreground_unlocked_input_active_300s_monotonic',
          eventGapCount: 0,
        },
        applications: [{
          appId: 'org.example.Editor',
          durationNs: 3_600_000_000_000,
          timezoneId: 'Asia/Shanghai',
        }],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('usage_summary', {
      query: { period: 'daily', bucket_key: '2026-08-10' },
    })
  })

  it('preserves a typed ISO weekly query', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(usagePayload('weekly'))

    const result = await getUsageSummary({ period: 'weekly', bucketKey: '2026-W33' })

    expect(result).toMatchObject({
      kind: 'summary',
      summary: { query: { period: 'weekly', bucketKey: '2026-W33' } },
    })
  })

  it('rejects invalid query keys before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(getUsageSummary({ period: 'daily', bucketKey: '2026-W33' })).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'invalid_usage_query',
        reason: 'usage_bucket_key_invalid',
        retryable: false,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects an application whose bucket differs from the query', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = usagePayload()
    payload.applications[0].bucket_key = '2026-08-09'
    mockedInvoke.mockResolvedValue(payload)

    await expect(getUsageSummary({ period: 'daily', bucketKey: '2026-08-10' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_usage_response', retryable: false },
    })
  })

  it('rejects a healthy summary without authoritative session coverage', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = usagePayload()
    payload.coverage.logind_session_available = false
    mockedInvoke.mockResolvedValue(payload)

    await expect(getUsageSummary({ period: 'daily', bucketKey: '2026-08-10' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_usage_response' },
    })
  })

  it('returns a factual transport error outside Tauri', async () => {
    await expect(getUsageSummary({ period: 'weekly', bucketKey: '2026-W33' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'desktop_bridge_unavailable', retryable: true },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

describe('getSystemInfo', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  it('normalizes a healthy report with sections and groups', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      schema_version: 1,
      captured_at_unix_ms: 1_784_000_000_000,
      tool_version: 'fastfetch 2.67.0 (x86_64)',
      status: 'healthy',
      reason: 'fastfetch_ok',
      retryable: false,
      sections: [
        {
          id: 'OS',
          groups: [{ entries: [{ key: 'os_name', value: 'CachyOS Linux' }] }],
        },
      ],
    })

    const result = await getSystemInfo()

    expect(result).toEqual({
      kind: 'systemInfo',
      report: {
        schemaVersion: 1,
        capturedAtUnixMs: 1_784_000_000_000,
        toolVersion: 'fastfetch 2.67.0 (x86_64)',
        status: 'healthy',
        reason: 'fastfetch_ok',
        retryable: false,
        sections: [
          {
            id: 'OS',
            groups: [{ title: null, entries: [{ key: 'os_name', value: 'CachyOS Linux' }] }],
          },
        ],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('system_info')
  })

  it('preserves a group title for multi-device sections', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      schema_version: 1,
      captured_at_unix_ms: null,
      tool_version: null,
      status: 'healthy',
      reason: 'fastfetch_ok',
      retryable: false,
      sections: [
        {
          id: 'GPU',
          groups: [{ title: 'NVIDIA RTX 4070', entries: [{ key: 'driver', value: 'nvidia' }] }],
        },
      ],
    })

    const result = await getSystemInfo()

    expect(result).toMatchObject({
      kind: 'systemInfo',
      report: {
        sections: [{ id: 'GPU', groups: [{ title: 'NVIDIA RTX 4070' }] }],
      },
    })
  })

  it('rejects a report with malformed entries', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      schema_version: 1,
      captured_at_unix_ms: null,
      tool_version: null,
      status: 'healthy',
      reason: 'fastfetch_ok',
      retryable: false,
      sections: [
        {
          id: 'OS',
          groups: [{ entries: [{ key: '', value: 'CachyOS Linux' }] }],
        },
      ],
    })

    await expect(getSystemInfo()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_system_info_report', retryable: false },
    })
  })

  it('returns a factual transport error outside Tauri', async () => {
    await expect(getSystemInfo()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'desktop_bridge_unavailable', retryable: true },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})

function noteSummaryPayload(overrides: Record<string, unknown> = {}) {
  return {
    id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    title: '今日备忘',
    diary_date: '2026-08-10',
    tags: ['工作'],
    status: 'active',
    pinned: false,
    created_at_ms: 1_786_154_400_000,
    updated_at_ms: 1_786_154_400_000,
    deleted_at_ms: null,
    revision: 1,
    body_bytes: 12,
    body_sha256: '0'.repeat(64),
    ...overrides,
  }
}

function notePagePayload() {
  return {
    query: {
      search: null,
      diary_date_from: null,
      diary_date_to: null,
      tags: [],
      status: null,
      deleted: 'exclude',
      sort: 'updated_desc',
      limit: 64,
      offset: 0,
    },
    notes: [noteSummaryPayload()],
    has_more: false,
    next_offset: null,
  }
}

describe('notes bridge', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  const validQuery = {
    search: null,
    diaryDateFrom: null,
    diaryDateTo: null,
    tags: [],
    status: null,
    deleted: 'exclude',
    sort: 'updated_desc',
    limit: 64,
    offset: 0,
  } as const

  it('normalizes a note page and sends snake_case wire query', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(notePagePayload())

    const result = await listNotes({ ...validQuery })

    expect(result).toMatchObject({
      kind: 'page',
      page: {
        hasMore: false,
        nextOffset: null,
        notes: [{
          id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
          diaryDate: '2026-08-10',
          tags: ['工作'],
          status: 'active',
          revision: 1,
        }],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_list', {
      query: {
        search: null,
        diary_date_from: null,
        diary_date_to: null,
        tags: [],
        status: null,
        deleted: 'exclude',
        sort: 'updated_desc',
        limit: 64,
        offset: 0,
      },
    })
  })

  it('rejects an invalid query before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(listNotes({ ...validQuery, limit: 0 })).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'invalid_note_input',
        reason: 'note_query_limit_invalid',
        retryable: false,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects an impossible diary date in the query', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(listNotes({ ...validQuery, diaryDateFrom: '2026-02-29' })).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'note_query_date_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects a note page whose echoed query does not match the request', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = notePagePayload()
    payload.query.limit = 32
    mockedInvoke.mockResolvedValue(payload)

    await expect(listNotes({ ...validQuery })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_list_response' },
    })
  })

  it('normalizes a note document with matching body length', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      summary: noteSummaryPayload({ body_bytes: 11 }),
      body_markdown: 'hello world',
    })

    const result = await getNote('019fe096-aeac-7bc1-8077-6e960dbc5570')

    expect(result).toMatchObject({
      kind: 'document',
      document: { bodyMarkdown: 'hello world' },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_get', {
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    })
  })

  it('rejects a document whose body length contradicts the summary', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      summary: noteSummaryPayload({ body_bytes: 12 }),
      body_markdown: 'not eleven bytes',
    })

    await expect(getNote('019fe096-aeac-7bc1-8077-6e960dbc5570')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_get_response' },
    })
  })

  it('rejects a note document for a different id', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      summary: noteSummaryPayload({
        id: '019fe096-aeac-7bc1-8077-6e960dbc5571',
        body_bytes: 11,
      }),
      body_markdown: 'hello world',
    })

    await expect(getNote('019fe096-aeac-7bc1-8077-6e960dbc5570')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_get_response' },
    })
  })

  it('rejects a non-UUID note id before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(getNote('not-a-uuid')).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'note_id_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('routes create to notes_upsert with unit intent and normalizes stored', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({ kind: 'stored', value: noteSummaryPayload() })

    const result = await writeNote({
      kind: 'create',
      meta: {
        title: '今日备忘',
        diaryDate: '2026-08-10',
        tags: ['工作'],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: 'hello world',
    })

    expect(result).toMatchObject({ kind: 'mutation', result: { kind: 'stored' } })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_upsert', {
      intent: { kind: 'create' },
      meta: {
        title: '今日备忘',
        diary_date: '2026-08-10',
        tags: ['工作'],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: 'hello world',
    })
  })

  it('routes autosave to notes_autosave with expected revision and keeps typed conflict', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      kind: 'conflict',
      value: {
        expected_revision: 1,
        current: noteSummaryPayload({ revision: 2 }),
      },
    })

    const result = await writeNote({
      kind: 'save',
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
      autosave: true,
      meta: {
        title: '今日备忘',
        diaryDate: null,
        tags: [],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: 'hello world',
    })

    expect(result).toMatchObject({
      kind: 'mutation',
      result: { kind: 'conflict', expectedRevision: 1, current: { revision: 2 } },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_autosave', {
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
      meta: expect.objectContaining({ diary_date: null }),
      bodyMarkdown: 'hello world',
    })
  })

  it('routes non-autosave save to notes_upsert with tagged save intent', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({ kind: 'stored', value: noteSummaryPayload({ revision: 2 }) })

    const result = await writeNote({
      kind: 'save',
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
      autosave: false,
      meta: {
        title: '今日备忘',
        diaryDate: null,
        tags: [],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: 'hello world',
    })

    expect(result).toMatchObject({ kind: 'mutation', result: { kind: 'stored', note: { revision: 2 } } })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_upsert', {
      intent: {
        kind: 'save',
        id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
        expected_revision: 1,
        autosave: false,
      },
      meta: expect.any(Object),
      bodyMarkdown: 'hello world',
    })
  })

  it('preserves a string rejection from Tauri when note writes fail', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockRejectedValue('invalid args `bodyMarkdown`: missing required key')

    await expect(writeNote({
      kind: 'create',
      meta: {
        title: 'today',
        diaryDate: '2026-08-19',
        tags: [],
        status: 'active',
        pinned: false,
      },
      bodyMarkdown: 'dinner',
    })).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'notes_write_unreachable',
        reason: 'invalid args `bodyMarkdown`: missing required key',
        retryable: true,
      },
    })
  })

  it('rejects note write responses that do not match the write intent', async () => {
    window.__TAURI_INTERNALS__ = {}
    const meta = {
      title: '今日备忘',
      diaryDate: null,
      tags: [],
      status: 'active' as const,
      pinned: false,
    }
    mockedInvoke.mockResolvedValueOnce({
      kind: 'conflict',
      value: { expected_revision: 1, current: noteSummaryPayload({ revision: 2 }) },
    })
    mockedInvoke.mockResolvedValueOnce({
      kind: 'stored',
      value: noteSummaryPayload({ id: '019fe096-aeac-7bc1-8077-6e960dbc5571', revision: 2 }),
    })
    mockedInvoke.mockResolvedValueOnce({
      kind: 'conflict',
      value: { expected_revision: 2, current: noteSummaryPayload({ revision: 3 }) },
    })

    await expect(writeNote({ kind: 'create', meta, bodyMarkdown: 'hello world' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_write_response' },
    })
    const save = {
      kind: 'save' as const,
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
      autosave: false,
      meta,
      bodyMarkdown: 'hello world',
    }
    await expect(writeNote(save)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_write_response' },
    })
    await expect(writeNote(save)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_write_response' },
    })
  })

  it('rejects a body above 4 MiB before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(writeNote({
      kind: 'create',
      meta: {
        title: 'x',
        diaryDate: null,
        tags: [],
        status: 'draft',
        pinned: false,
      },
      bodyMarkdown: 'x'.repeat(4 * 1024 * 1024 + 1),
    })).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'note_body_exceeds_4_mib' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('normalizes deleted and restored mutations with wire revision', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValueOnce({ kind: 'deleted', value: noteSummaryPayload() })
    mockedInvoke.mockResolvedValueOnce({ kind: 'restored', value: noteSummaryPayload() })

    const deleted = await deleteNote('019fe096-aeac-7bc1-8077-6e960dbc5570', 1)
    const restored = await restoreNote('019fe096-aeac-7bc1-8077-6e960dbc5570', 1)

    expect(deleted).toMatchObject({ kind: 'mutation', result: { kind: 'deleted' } })
    expect(restored).toMatchObject({ kind: 'mutation', result: { kind: 'restored' } })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_delete', {
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
    })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_restore', {
      id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
      expectedRevision: 1,
    })
  })

  it('rejects delete and restore responses that do not match the command', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValueOnce({ kind: 'restored', value: noteSummaryPayload() })
    mockedInvoke.mockResolvedValueOnce({
      kind: 'conflict',
      value: {
        expected_revision: 2,
        current: noteSummaryPayload({ revision: 3 }),
      },
    })

    await expect(deleteNote('019fe096-aeac-7bc1-8077-6e960dbc5570', 1)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_delete_response' },
    })
    await expect(restoreNote('019fe096-aeac-7bc1-8077-6e960dbc5570', 1)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_restore_response' },
    })
  })

  it('normalizes a bounded export and rejects an unsupported format before invoking', async () => {
    window.__TAURI_INTERNALS__ = {}
    const content = '# title\nbody'
    mockedInvoke.mockResolvedValue({
      format: 'markdown',
      content,
      content_bytes: new TextEncoder().encode(content).length,
      content_sha256: 'a'.repeat(64),
    })

    const exported = await exportNotes({ ...validQuery }, 'markdown')
    expect(exported).toMatchObject({ kind: 'export', export: { format: 'markdown' } })
    expect(mockedInvoke).toHaveBeenCalledWith('notes_export', {
      query: expect.objectContaining({ limit: 64 }),
      format: 'markdown',
    })

    mockedInvoke.mockReset()
    await expect(exportNotes({ ...validQuery }, 'pdf' as never)).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'note_export_format_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects an export whose returned format differs from the request', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      format: 'json',
      content: '{}',
      content_bytes: 2,
      content_sha256: 'a'.repeat(64),
    })

    await expect(exportNotes({ ...validQuery }, 'markdown')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_notes_export_response' },
    })
  })

  it('returns a factual transport error outside Tauri', async () => {
    await expect(listNotes({ ...validQuery })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'desktop_bridge_unavailable', retryable: true },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})

const TRANSFER_UUID = '019fe096-aeac-7bc1-8077-6e960dbc5570'
const TRANSFER_HANDLE = '019fe096-aeac-7bc1-8077-6e960dbc5571'

function transferTaskPayload(state: Record<string, unknown> = { status: 'queued' }) {
  return {
    id: TRANSFER_UUID,
    source: { kind: 'local', handle: TRANSFER_HANDLE },
    destination: { kind: 'remote', profile_id: TRANSFER_UUID, protocol: 'sftp', path: '/remote/file.txt' },
    direction: 'upload',
    expected_source: null,
    expected_destination: null,
    state,
    progress: {
      bytes_transferred: 0,
      total_bytes: null,
      bytes_per_second: null,
      sampled_at_unix_ms: null,
    },
    retry_policy: { max_attempts: 3, initial_backoff_ms: 1000, max_backoff_ms: 60_000 },
    completed_attempts: 0,
    bandwidth_limit: null,
    conflict_policy: 'rename',
    features: {
      pause: { status: 'unsupported', reason: 'transfer_pause_requires_inflight_control' },
      resume: { status: 'supported' },
      resume_validation: 'remote_identity',
    },
    revision: 1,
    created_at_unix_ms: 1_786_154_400_000,
    updated_at_unix_ms: 1_786_154_400_000,
  }
}

function transferPagePayload(tasks = [transferTaskPayload()]) {
  return {
    query: { limit: 64, offset: 0, states: [], direction: null as 'upload' | 'download' | null, profile_id: null },
    tasks,
    has_more: false,
    next_offset: null,
  }
}

describe('journal fetch bridge', () => {
  const input = {
    localDate: '2026-08-20',
    timezone: 'Asia/Shanghai',
    windowStartMs: 1_787_155_200_000,
    windowEndMs: 1_787_241_600_000,
  }

  function payload() {
    return {
      schema_version: 1,
      local_date: '2026-08-20',
      timezone: 'Asia/Shanghai',
      title: '2026-08-20 工作日志',
      markdown_body: '# 2026-08-20 工作日志',
      work_items: [{
        workstream: '本机控制台',
        state: 'completed',
        summary: '完成日志功能',
        evidence: ['package tests passed'],
        source_session_ids: ['session-1'],
      }],
      knowledge_items: [{
        topic: 'Markdown',
        summary: '渲染面可以直接编辑',
        source_session_ids: ['session-1'],
      }],
      knowledge_candidates: [{
        source_session_id: 'session-1',
        recommended: true,
        reason: 'long_knowledge_session',
        recommended_skill: 'capture-conversations-to-vault',
      }],
      remaining_items: [],
      source_coverage: [{
        source: 'codex',
        state: 'healthy',
        reason: 'session_source_ready',
        scanned_sessions: 2,
        included_sessions: 1,
        ignored_short_sessions: 1,
      }],
      token_usage: {
        state: 'healthy',
        reason: 'cc_switch_usage_ready',
        window_start_ms: input.windowStartMs,
        window_end_ms: input.windowEndMs,
        last_synced_at_ms: 1_787_200_000_000,
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 10,
        cache_creation_tokens: 0,
        reported_total_tokens: 120,
        total_method: 'input_plus_output',
        by_source: [{
          source: 'codex',
          request_count: 2,
          input_tokens: 100,
          output_tokens: 20,
          cache_read_tokens: 10,
          cache_creation_tokens: 0,
          reported_total_tokens: 120,
        }],
      },
      warnings: [],
    }
  }

  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI_INTERNALS__
    delete window.__TAURI__
  })

  it('normalizes a structured summary and preserves the local-day window', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(payload())

    await expect(fetchJournalSummary(input)).resolves.toMatchObject({
      kind: 'summary',
      summary: {
        schemaVersion: 1,
        localDate: '2026-08-20',
        tokenUsage: { reportedTotalTokens: 120, totalMethod: 'input_plus_output' },
        sourceCoverage: [{ source: 'codex', includedSessions: 1 }],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('journal_fetch', {
      request: {
        local_date: '2026-08-20',
        timezone: 'Asia/Shanghai',
        window_start_ms: input.windowStartMs,
        window_end_ms: input.windowEndMs,
      },
    })
  })

  it('collects session and token facts without waiting for AI summarization', async () => {
    window.__TAURI_INTERNALS__ = {}
    const summaryPayload = payload()
    mockedInvoke.mockResolvedValue({
      schema_version: 1,
      local_date: summaryPayload.local_date,
      timezone: summaryPayload.timezone,
      source_coverage: summaryPayload.source_coverage,
      token_usage: summaryPayload.token_usage,
      sessions: [{
        source: 'codex',
        session_id: 'session-1',
        title: '完成日志功能',
        workspace: '/home/skynit/workspace/sky',
        updated_at_ms: 1_787_200_000_000,
        eligibility: {
          state: 'included',
          reason: 'substantive_session',
          substantive_messages: 8,
          content_chars: 4000,
          length_class: 'long',
        },
        message_count: 12,
      }],
      warnings: [],
    })

    await expect(collectJournalUsage(input)).resolves.toMatchObject({
      kind: 'collection',
      collection: {
        tokenUsage: { reportedTotalTokens: 120 },
        sessions: [{ sessionId: 'session-1', messageCount: 12 }],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('journal_collect', {
      request: {
        local_date: '2026-08-20',
        timezone: 'Asia/Shanghai',
        window_start_ms: input.windowStartMs,
        window_end_ms: input.windowEndMs,
      },
    })
  })

  it('rejects malformed summary facts and invalid day windows', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      ...payload(),
      token_usage: { ...payload().token_usage, window_end_ms: input.windowEndMs + 1 },
    })
    await expect(fetchJournalSummary(input)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_journal_fetch_response' },
    })

    mockedInvoke.mockReset()
    await expect(fetchJournalSummary({ ...input, windowEndMs: input.windowStartMs + 1_000 })).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'journal_fetch_input_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('keeps knowledge capture behind explicit confirmation', async () => {
    window.__TAURI_INTERNALS__ = {}
    await expect(captureJournalKnowledge(input, 'session-1', false)).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'journal_knowledge_input_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()

    mockedInvoke.mockResolvedValue({
      schema_version: 1,
      session_id: 'session-1',
      state: 'stored',
      note_paths: ['/home/skynit/Uni/ming/30-知识/Markdown.md'],
      warnings: [],
    })
    await expect(captureJournalKnowledge(input, 'session-1', true)).resolves.toMatchObject({
      kind: 'capture',
      result: { state: 'stored', sessionId: 'session-1' },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('journal_capture_knowledge', {
      request: {
        fetch: {
          local_date: '2026-08-20',
          timezone: 'Asia/Shanghai',
          window_start_ms: input.windowStartMs,
          window_end_ms: input.windowEndMs,
        },
        session_id: 'session-1',
        confirmed: true,
      },
    })
  })
})

describe('transfers bridge', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  const validQuery = { limit: 64, offset: 0, states: [], direction: null, profileId: null }

  it('normalizes a transfer page and sends snake_case wire query', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(transferPagePayload())

    const result = await listTransfers({ ...validQuery })

    expect(result).toMatchObject({
      kind: 'page',
      page: {
        hasMore: false,
        nextOffset: null,
        tasks: [{
          id: TRANSFER_UUID,
          direction: 'upload',
          state: { status: 'queued' },
          progress: { bytesTransferred: 0, totalBytes: null },
          features: { resume: { status: 'supported' }, resumeValidation: 'remote_identity' },
          revision: 1,
        }],
      },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_list', {
      query: { limit: 64, offset: 0, states: [], direction: null, profile_id: null },
    })
  })

  it('rejects an invalid query before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(listTransfers({ ...validQuery, limit: 0 })).resolves.toEqual({
      kind: 'error',
      error: {
        kind: 'protocol',
        code: 'invalid_transfer_input',
        reason: 'transfer_query_limit_invalid',
        retryable: false,
      },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects a page whose task state does not match the state filter', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = transferPagePayload()
    payload.query.states = ['completed']
    mockedInvoke.mockResolvedValue(payload)

    await expect(listTransfers({ ...validQuery, states: ['completed'] })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_list_response' },
    })
  })

  it('rejects a page whose task direction does not match the direction filter', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = transferPagePayload()
    payload.query.direction = 'download'
    mockedInvoke.mockResolvedValue(payload)

    await expect(listTransfers({ ...validQuery, direction: 'download' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_list_response' },
    })
  })

  it('rejects a page whose echoed query does not match the request', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = transferPagePayload()
    payload.query.limit = 32
    mockedInvoke.mockResolvedValue(payload)

    await expect(listTransfers({ ...validQuery })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_list_response' },
    })
  })

  it('normalizes a completed task through transfer_get', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(transferTaskPayload({
      status: 'completed',
      completion: { verification: 'size', identity: null, completed_at_unix_ms: 1_786_154_400_100 },
    }))

    const result = await getTransfer(TRANSFER_UUID)

    expect(result).toMatchObject({
      kind: 'task',
      task: { state: { status: 'completed', completion: { verification: 'size' } } },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_get', { id: TRANSFER_UUID })
  })

  it('rejects a non-UUID transfer id before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(getTransfer('not-a-uuid')).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'transfer_id_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects transfer tasks that violate the Rust public invariants', async () => {
    window.__TAURI_INTERNALS__ = {}
    const invalidPayloads = [
      { destination: { kind: 'remote', profile_id: TRANSFER_UUID, protocol: 'webdav', path: '/remote/file.txt' } },
      { destination: { kind: 'remote', profile_id: TRANSFER_UUID, protocol: 'sftp', path: '/remote\nfile.txt' } },
      { expected_source: { size_bytes: 'invalid', modified_at_unix_ms: null, etag: null } },
      { expected_source: { size_bytes: null, modified_at_unix_ms: null, etag: 'bad\netag' } },
      { completed_attempts: 4 },
      { created_at_unix_ms: -1 },
      { updated_at_unix_ms: 1_786_154_399_999 },
      { progress: { bytes_transferred: 2, total_bytes: 1, bytes_per_second: null, sampled_at_unix_ms: null } },
      { progress: { bytes_transferred: 0, total_bytes: null, bytes_per_second: 1_099_511_627_777, sampled_at_unix_ms: null } },
      { features: { pause: { status: 'supported' }, resume: { status: 'supported' }, resume_validation: null } },
      { features: { pause: { status: 'supported' }, resume: { status: 'unsupported', reason: 'resume_unavailable' }, resume_validation: 'size_only' } },
      { state: { status: 'completed', completion: { verification: 'size', identity: { size_bytes: 'invalid' }, completed_at_unix_ms: 1_786_154_400_100 } } },
      { state: { status: 'cancelled', checkpoint: { offset: 0 }, cancelled_at_unix_ms: 1_786_154_400_100 } },
    ]

    for (const override of invalidPayloads) {
      mockedInvoke.mockResolvedValueOnce({ ...transferTaskPayload(), ...override })
      await expect(getTransfer(TRANSFER_UUID)).resolves.toMatchObject({
        kind: 'error',
        error: { code: 'invalid_transfer_get_response' },
      })
    }
  })

  it('rejects get responses for a different task id', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      ...transferTaskPayload(),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5572',
    })

    await expect(getTransfer(TRANSFER_UUID)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_get_response' },
    })
  })

  it('sends a snake_case draft for local-to-remote upload enqueue', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(transferTaskPayload())

    const result = await enqueueTransfer({
      id: TRANSFER_UUID,
      source: { kind: 'local', handle: TRANSFER_HANDLE },
      destination: { kind: 'remote', profileId: TRANSFER_UUID, path: '/remote/file.txt' },
      direction: 'upload',
      expectedSource: { sizeBytes: 13_443_148, modifiedAtUnixMs: null, etag: null },
      expectedDestination: null,
      retryPolicy: { maxAttempts: 3, initialBackoffMs: 1000, maxBackoffMs: 60_000 },
      bandwidthLimit: null,
      conflictPolicy: 'rename',
    })

    expect(result).toMatchObject({ kind: 'task', task: { id: TRANSFER_UUID } })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_enqueue', {
      draft: {
        id: TRANSFER_UUID,
        source: { kind: 'local', handle: TRANSFER_HANDLE },
        destination: { kind: 'remote', profile_id: TRANSFER_UUID, path: '/remote/file.txt' },
        direction: 'upload',
        expected_source: { size_bytes: 13_443_148, modified_at_unix_ms: null, etag: null },
        expected_destination: null,
        retry_policy: { max_attempts: 3, initial_backoff_ms: 1000, max_backoff_ms: 60_000 },
        bandwidth_limit: null,
        conflict_policy: 'rename',
      },
    })
  })

  it('rejects a mismatched direction before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}

    await expect(enqueueTransfer({
      id: TRANSFER_UUID,
      source: { kind: 'remote', profileId: TRANSFER_UUID, path: '/remote/file.txt' },
      destination: { kind: 'local', handle: TRANSFER_HANDLE },
      direction: 'upload',
      expectedSource: null,
      expectedDestination: null,
      retryPolicy: { maxAttempts: 3, initialBackoffMs: 1000, maxBackoffMs: 60_000 },
      bandwidthLimit: null,
      conflictPolicy: 'fail',
    })).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'transfer_endpoints_mismatch' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('wires cancel and retry with expected revision and normalizes updated', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValueOnce({ result: 'updated', task: transferTaskPayload({ status: 'cancelling' }) })
    mockedInvoke.mockResolvedValueOnce({
      result: 'updated',
      task: {
        ...transferTaskPayload({ status: 'retry_scheduled', not_before_unix_ms: 1_786_154_400_100, failure: { kind: 'timeout', operation: 'read', reason: 'remote_io_timeout', retry: 'backoff' } }),
        revision: 2,
      },
    })

    const cancelled = await cancelTransfer(TRANSFER_UUID, 1)
    const retried = await retryTransfer(TRANSFER_UUID, 2)

    expect(cancelled).toMatchObject({ kind: 'mutation', result: { result: 'updated', task: { state: { status: 'cancelling' } } } })
    expect(retried).toMatchObject({
      kind: 'mutation',
      result: { result: 'updated', task: { state: { status: 'retry_scheduled', failure: { retry: 'backoff' } } } },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_cancel', { id: TRANSFER_UUID, expectedRevision: 1 })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_retry', { id: TRANSFER_UUID, expectedRevision: 2 })
  })

  it('accepts revision zero requests and rejects mismatched mutation responses', async () => {
    window.__TAURI_INTERNALS__ = {}
    const initial = transferTaskPayload({ status: 'cancelled', checkpoint: null, cancelled_at_unix_ms: 1_786_154_400_100 })
    initial.revision = 0
    mockedInvoke.mockResolvedValueOnce({ result: 'updated', task: initial })
    mockedInvoke.mockResolvedValueOnce({ result: 'updated', task: { ...transferTaskPayload(), id: TRANSFER_HANDLE } })
    mockedInvoke.mockResolvedValueOnce({ result: 'conflict', expected_revision: 2, current: transferTaskPayload() })

    await expect(cancelTransfer(TRANSFER_UUID, 0)).resolves.toMatchObject({
      kind: 'mutation',
      result: { result: 'updated', task: { id: TRANSFER_UUID, revision: 0 } },
    })
    await expect(cancelTransfer(TRANSFER_UUID, 1)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_cancel_response' },
    })
    await expect(retryTransfer(TRANSFER_UUID, 1)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_retry_response' },
    })
  })

  it('keeps typed conflict from resolve_conflict and rejects unknown policy before invoke', async () => {
    window.__TAURI_INTERNALS__ = {}
    const current = transferTaskPayload()
    current.revision = 2
    mockedInvoke.mockResolvedValue({ result: 'conflict', expected_revision: 1, current })

    const resolved = await resolveTransferConflict(TRANSFER_UUID, 1, 'overwrite')
    expect(resolved).toMatchObject({
      kind: 'mutation',
      result: { result: 'conflict', expectedRevision: 1, current: { revision: 2 } },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_resolve_conflict', {
      id: TRANSFER_UUID,
      expectedRevision: 1,
      policy: 'overwrite',
    })

    mockedInvoke.mockReset()
    await expect(resolveTransferConflict(TRANSFER_UUID, 1, 'delete' as never)).resolves.toMatchObject({
      kind: 'error',
      error: { reason: 'transfer_conflict_policy_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('normalizes a picker grant and returns null on cancellation', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValueOnce({
      handle: TRANSFER_HANDLE,
      purpose: 'upload_source',
      display_name: 'report.pdf',
      size_bytes: 4096,
    })
    mockedInvoke.mockResolvedValueOnce(null)

    const picked = await pickUploadSource()
    const cancelled = await pickUploadSource()

    expect(picked).toMatchObject({
      kind: 'picked',
      grant: { handle: TRANSFER_HANDLE, purpose: 'upload_source', displayName: 'report.pdf', sizeBytes: 4096 },
    })
    expect(cancelled).toEqual({ kind: 'picked', grant: null })
    expect(mockedInvoke).toHaveBeenCalledWith('transfer_pick_upload_source')
  })

  it('rejects a download destination grant that carries a size', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      handle: TRANSFER_HANDLE,
      purpose: 'download_destination',
      display_name: 'output.bin',
      size_bytes: 1,
    })

    await expect(pickDownloadDestination()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_transfer_pick_download_destination_response' },
    })
  })

  it('returns a factual transport error outside Tauri', async () => {
    await expect(listTransfers({ ...validQuery })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'desktop_bridge_unavailable', retryable: true },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})
})

const remoteOperations = [
  'list',
  'stat',
  'read',
  'write',
  'create_directory',
  'rename',
  'delete',
  'resume_read',
  'resume_write',
  'atomic_rename',
  'set_permissions',
]

function remoteCapabilities(status: 'supported' | 'unsupported' = 'supported') {
  return remoteOperations.map((operation) => ({
    operation,
    status: status === 'supported'
      ? { status: 'supported' }
      : { status: 'unsupported', reason: 'operation_not_available' },
  }))
}

function remoteCatalogPayload() {
  return {
    schema_version: 1,
    snapshot_id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    captured_at_unix_ms: 1_786_154_400_000,
    adapters: [
      { protocol: 'ssh', availability: { status: 'healthy' }, terminal: { status: 'supported' }, file_operations: remoteCapabilities('unsupported') },
      { protocol: 'sftp', availability: { status: 'healthy' }, terminal: { status: 'unsupported', reason: 'terminal_not_applicable' }, file_operations: remoteCapabilities('supported') },
      { protocol: 'ftp', availability: { status: 'degraded', reason: 'plain_ftp_explicitly_enabled' }, terminal: { status: 'unsupported', reason: 'terminal_not_applicable' }, file_operations: remoteCapabilities('supported') },
      { protocol: 'ftps_explicit', availability: { status: 'healthy' }, terminal: { status: 'unsupported', reason: 'terminal_not_applicable' }, file_operations: remoteCapabilities('supported') },
      {
        protocol: 'smb',
        availability: { status: 'healthy' },
        terminal: { status: 'unsupported', reason: 'terminal_not_applicable' },
        file_operations: remoteOperations.map((operation) => ({
          operation,
          status: operation === 'set_permissions'
            ? { status: 'unsupported', reason: 'smb_set_permissions_not_implemented' }
            : { status: 'supported' },
        })),
      },
    ],
  }
}

function remoteProfilePayload(id = '019fe096-aeac-7bc1-8077-6e960dbc5570') {
  return {
    id,
    label: '工作站',
    protocol: 'ssh' as const,
    endpoint: { host: 'workstation.local', port: 22 },
    username: 'operator',
    domain: null,
    authentication: { method: 'ssh_agent' as const },
    trust: { kind: 'ssh_known_hosts' as const, first_use: 'ask_user' as const },
    options: { protocol: 'ssh' as const, jump_profiles: [], agent_forwarding: false },
  }
}

function remoteEntryPayload(name: string, path: string, kind: 'file' | 'directory' = 'file') {
  return {
    name,
    path,
    kind,
    identity: { size_bytes: kind === 'file' ? 4 : null, modified_at_unix_ms: null, etag: null },
    unix_mode: null,
    capabilities: remoteCapabilities(),
  }
}

function terminalStatusPayload(state: 'running' | 'closed_by_client' = 'running') {
  return {
    state: { state },
    transcript_retained_bytes: 0,
    transcript_dropped_bytes: 0,
  }
}

describe('remote frontend contracts', () => {
  beforeEach(() => {
    mockedInvoke.mockReset()
    channelTestState.channels.length = 0
    delete window.__TAURI__
    delete window.__TAURI_INTERNALS__
  })

  it('normalizes the complete adapter catalog with the production SMB file boundary', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue(remoteCatalogPayload())

    const result = await getRemoteAdapterCatalog()

    expect(result.kind).toBe('data')
    if (result.kind !== 'data') throw new Error('expected remote catalog')
    expect(result.data.adapters).toHaveLength(5)
    expect(result.data.adapters.find((adapter) => adapter.protocol === 'smb')?.availability).toEqual({
      status: 'healthy',
      capabilityReason: 'available',
    })
    expect(result.data.adapters.find((adapter) => adapter.protocol === 'smb')?.fileOperations.filter((operation) => operation.status === 'supported')).toHaveLength(10)
    expect(mockedInvoke).toHaveBeenCalledWith('remote_capabilities', undefined)
  })

  it('rejects an incomplete adapter catalog', async () => {
    window.__TAURI_INTERNALS__ = {}
    const payload = remoteCatalogPayload()
    payload.adapters.pop()
    mockedInvoke.mockResolvedValue(payload)

    await expect(getRemoteAdapterCatalog()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_catalog_response', retryable: false },
    })
  })

  it('normalizes a bounded remote profile page', async () => {
    window.__TAURI_INTERNALS__ = {}
    mockedInvoke.mockResolvedValue({
      result: 'page',
      value: {
        profiles: [{
          profile: {
            id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
            label: '工作站',
            protocol: 'ssh',
            endpoint: { host: 'workstation.local', port: 22 },
            username: 'operator',
            domain: null,
            authentication: { method: 'ssh_agent' },
            trust: { kind: 'ssh_known_hosts', first_use: 'ask_user' },
            options: { protocol: 'ssh', jump_profiles: [], agent_forwarding: false },
          },
          revision: 0,
          created_at_unix_ms: 1,
          updated_at_unix_ms: 1,
        }],
        next_after: null,
      },
    })

    const result = await getRemoteProfiles()

    expect(result).toMatchObject({
      kind: 'data',
      data: { profiles: [{ profile: { label: '工作站', endpoint: { host: 'workstation.local' } } }] },
    })
    expect(mockedInvoke).toHaveBeenCalledWith('remote_profile', {
      command: { operation: 'list', query: { after: null, limit: 16 } },
    })
  })

  it('preserves a bounded pinned FTPS certificate from the profile bridge', async () => {
    window.__TAURI_INTERNALS__ = {}
    const certificatePem = '-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n'
    mockedInvoke.mockResolvedValue({
      result: 'page',
      value: {
        profiles: [{
          profile: {
            id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
            label: '家用服务器',
            protocol: 'ftps_explicit',
            endpoint: { host: 'sea', port: 21 },
            username: 'operator',
            domain: null,
            authentication: {
              method: 'password',
              secret: {
                backend: 'secret_service',
                item_id: '019fe096-aeac-7bc1-8077-6e960dbc5571',
              },
            },
            trust: { kind: 'pinned_tls_certificate', certificate_pem: certificatePem },
            options: {
              protocol: 'ftps_explicit',
              data_connection: 'passive',
              require_protected_data_channel: true,
            },
          },
          revision: 1,
          created_at_unix_ms: 1,
          updated_at_unix_ms: 2,
        }],
        next_after: null,
      },
    })

    await expect(getRemoteProfiles()).resolves.toMatchObject({
      kind: 'data',
      data: {
        profiles: [{
          profile: {
            endpoint: { host: 'sea' },
            trust: { kind: 'pinned_tls_certificate', certificate_pem: certificatePem },
          },
        }],
      },
    })
  })

  it('rejects remote profile pages and upserts that do not match the request', async () => {
    window.__TAURI_INTERNALS__ = {}
    const profile = remoteProfilePayload()
    mockedInvoke.mockResolvedValueOnce({
      result: 'page',
      value: {
        profiles: [{ profile: remoteProfilePayload('019fe096-aeac-7bc1-8077-6e960dbc5569'), revision: 0, created_at_unix_ms: 1, updated_at_unix_ms: 1 }],
        next_after: null,
      },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'stored',
      value: { profile, revision: 1, created_at_unix_ms: 1, updated_at_unix_ms: 1 },
    })

    await expect(getRemoteProfiles('019fe096-aeac-7bc1-8077-6e960dbc5570')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_profile_page_response' },
    })
    await expect(upsertRemoteProfile(profile, null)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_profile_response' },
    })
  })

  it('sends a revision-checked profile delete and rejects a mismatched response identity', async () => {
    window.__TAURI_INTERNALS__ = {}
    const profileId = '019fe096-aeac-7bc1-8077-6e960dbc5570'
    mockedInvoke.mockResolvedValueOnce({
      result: 'deleted',
      value: { profile_id: profileId },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'deleted',
      value: { profile_id: '019fe096-aeac-7bc1-8077-6e960dbc5571' },
    })

    await expect(deleteRemoteProfile(profileId, 4)).resolves.toEqual({ kind: 'data', data: profileId })
    expect(mockedInvoke).toHaveBeenNthCalledWith(1, 'remote_profile', {
      command: { operation: 'delete', profile_id: profileId, expected_revision: 4 },
    })
    await expect(deleteRemoteProfile(profileId, 4)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_profile_delete_response', retryable: false },
    })
  })

  it('rejects remote file-session responses that do not match profile or list query', async () => {
    window.__TAURI_INTERNALS__ = {}
    const profileId = '019fe096-aeac-7bc1-8077-6e960dbc5570'
    const otherId = '019fe096-aeac-7bc1-8077-6e960dbc5571'
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedInvoke.mockResolvedValueOnce({
      result: 'session',
      value: {
        id: sessionId,
        profile_id: otherId,
        protocol: 'ssh',
        state: { status: 'connected' },
        capabilities: remoteCapabilities(),
        opened_at_unix_ms: 1,
        updated_at_unix_ms: 1,
      },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'directory_page',
      value: {
        session_id: sessionId,
        path: '/wrong',
        offset: 0,
        entries: [remoteEntryPayload('file.txt', '/wrong/file.txt')],
        next_offset: 1,
      },
    })

    await expect(connectRemoteSession(profileId)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_session_response' },
    })
    await expect(listRemoteDirectory(sessionId, '/expected', 0)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_directory_response' },
    })
  })

  it('sends typed create, rename, and delete file-session mutations', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const entry = (name: string, path: string, kind: 'file' | 'directory') => ({
      name,
      path,
      kind,
      identity: { size_bytes: kind === 'file' ? 4 : null, modified_at_unix_ms: null, etag: null },
      unix_mode: null,
      capabilities: remoteCapabilities(),
    })
    mockedInvoke
      .mockResolvedValueOnce({ result: 'entry', value: entry('reports', '/reports', 'directory') })
      .mockResolvedValueOnce({ result: 'entry', value: entry('final.txt', '/reports/final.txt', 'file') })
      .mockResolvedValueOnce({ result: 'deleted', value: { session_id: sessionId } })

    await expect(createRemoteDirectory(sessionId, '/reports')).resolves.toMatchObject({
      kind: 'data',
      data: { path: '/reports', kind: 'directory' },
    })
    expect(mockedInvoke).toHaveBeenNthCalledWith(1, 'remote_session', {
      command: { operation: 'create_directory', session_id: sessionId, path: '/reports' },
    })

    await expect(renameRemoteEntry(sessionId, '/reports/draft.txt', '/reports/final.txt')).resolves.toMatchObject({
      kind: 'data',
      data: { path: '/reports/final.txt', name: 'final.txt' },
    })
    expect(mockedInvoke).toHaveBeenNthCalledWith(2, 'remote_session', {
      command: {
        operation: 'rename',
        session_id: sessionId,
        from: '/reports/draft.txt',
        to: '/reports/final.txt',
      },
    })

    await expect(deleteRemoteEntry(sessionId, '/reports/final.txt')).resolves.toEqual({
      kind: 'data',
      data: sessionId,
    })
    expect(mockedInvoke).toHaveBeenNthCalledWith(3, 'remote_session', {
      command: { operation: 'delete', session_id: sessionId, path: '/reports/final.txt' },
    })
  })

  it('stores and deletes opaque Secret Service references without retaining the transient byte array', async () => {
    window.__TAURI_INTERNALS__ = {}
    const captured: unknown[] = []
    mockedInvoke.mockImplementationOnce(async (_command, args) => {
      captured.push(structuredClone(args))
      return {
        result: 'stored',
        value: { reference: { backend: 'secret_service', item_id: '019fe096-aeac-7bc1-8077-6e960dbc5570' } },
      }
    })
    const password = new Uint8Array([115, 101, 99, 114, 101, 116])

    const stored = await storeRemoteSecret('password', password)

    expect(stored).toEqual({
      kind: 'data',
      data: { backend: 'secret_service', item_id: '019fe096-aeac-7bc1-8077-6e960dbc5570' },
    })
    expect(captured).toEqual([{
      command: { operation: 'store', kind: 'password', value: [115, 101, 99, 114, 101, 116] },
    }])
    expect(password).toEqual(new Uint8Array([115, 101, 99, 114, 101, 116]))
    expect(mockedInvoke.mock.calls[0]?.[1]).toEqual({
      command: { operation: 'store', kind: 'password', value: [0, 0, 0, 0, 0, 0] },
    })

    mockedInvoke.mockResolvedValueOnce({ result: 'deleted' })
    const deleted = await deleteRemoteSecret(stored.kind === 'data' ? stored.data : { backend: 'secret_service', item_id: '' })
    expect(deleted).toEqual({ kind: 'data', data: '019fe096-aeac-7bc1-8077-6e960dbc5570' })
    expect(mockedInvoke).toHaveBeenLastCalledWith('secret', {
      command: {
        operation: 'delete',
        reference: { backend: 'secret_service', item_id: '019fe096-aeac-7bc1-8077-6e960dbc5570' },
      },
    })
  })

  it('opens and resizes a remote terminal with validated typed dimensions', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedInvoke.mockResolvedValueOnce({
      result: 'opened',
      value: {
        session_id: sessionId,
        capabilities: {
          max_output_chunk_bytes: 45_056,
          max_input_chunk_bytes: 45_056,
          max_transcript_bytes: 65_536,
          max_rows: 1_000,
          max_columns: 1_000,
          max_pixel_dimension: 32_767,
          nonblocking_output: true,
          fixed_openssh_program: true,
        },
        status: {
          state: { state: 'running' },
          transcript_retained_bytes: 0,
          transcript_dropped_bytes: 0,
        },
      },
    })

    const opened = await openRemoteTerminal(
      '019fe096-aeac-7bc1-8077-6e960dbc5570',
      { rows: 32, columns: 120, pixelWidth: 960, pixelHeight: 640 },
    )

    expect(opened).toMatchObject({
      kind: 'data',
      data: { sessionId, capabilities: { maxPixelDimension: 32_767 } },
    })
    expect(mockedInvoke).toHaveBeenLastCalledWith('remote_terminal', {
      command: {
        operation: 'open',
        profile_id: '019fe096-aeac-7bc1-8077-6e960dbc5570',
        size: { rows: 32, columns: 120, pixel_width: 960, pixel_height: 640 },
        accept_new_host_key: false,
      },
    })

    mockedInvoke.mockResolvedValueOnce({ result: 'resized', value: { session_id: sessionId } })
    await expect(resizeRemoteTerminal(sessionId, {
      rows: 40,
      columns: 144,
      pixelWidth: 1_152,
      pixelHeight: 720,
    })).resolves.toEqual({ kind: 'data', data: sessionId })
    expect(mockedInvoke).toHaveBeenLastCalledWith('remote_terminal', {
      command: {
        operation: 'resize',
        session_id: sessionId,
        size: { rows: 40, columns: 144, pixel_width: 1_152, pixel_height: 720 },
      },
    })

    mockedInvoke.mockResolvedValueOnce({ result: 'resized', value: { session_id: '019fe096-aeac-7bc1-8077-6e960dbc5599' } })
    await expect(resizeRemoteTerminal(sessionId, {
      rows: 40,
      columns: 144,
      pixelWidth: 1_152,
      pixelHeight: 720,
    })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_resize_response' },
    })

    mockedInvoke.mockClear()
    await expect(resizeRemoteTerminal(sessionId, {
      rows: 0,
      columns: 80,
      pixelWidth: 0,
      pixelHeight: 0,
    })).resolves.toMatchObject({ kind: 'error', error: { code: 'remote_terminal_size_invalid' } })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('normalizes bounded terminal channel events before exposing them to the view', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const events: unknown[] = []
    mockedInvoke.mockImplementationOnce(async (_command, args) => {
      const channel = channelTestState.channels[0]
      expect(args).toMatchObject({ sessionId, maxBytes: 45_056, onEvent: channel })
      channel.onmessage({
        event: 'started',
        session_id: sessionId,
        max_bytes: 45_056,
        status: terminalStatusPayload(),
      })
      channel.onmessage({ event: 'data', session_id: sessionId, data: btoa('ready') })
      channel.onmessage({
        event: 'ended',
        session_id: sessionId,
        status: terminalStatusPayload('closed_by_client'),
      })
    })

    await expect(streamRemoteTerminal(sessionId, 45_056, (event) => events.push(event)))
      .resolves.toEqual({ kind: 'data', data: undefined })
    expect(events).toEqual([
      expect.objectContaining({ event: 'started', sessionId, maxBytes: 45_056 }),
      { event: 'data', sessionId, encodedData: btoa('ready') },
      expect.objectContaining({ event: 'ended', sessionId }),
    ])
  })

  it('rejects terminal results that do not match session, byte count, or close state', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const otherId = '019fe096-aeac-7bc1-8077-6e960dbc5599'
    mockedInvoke.mockResolvedValueOnce({
      result: 'read',
      value: { session_id: otherId, output: { status: 'pending' } },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'status',
      value: { session_id: otherId, status: terminalStatusPayload() },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'wrote',
      value: { session_id: sessionId, accepted_bytes: 4 },
    })
    mockedInvoke.mockResolvedValueOnce({
      result: 'closed',
      value: { session_id: sessionId, status: terminalStatusPayload('running') },
    })

    await expect(readRemoteTerminal(sessionId)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_read_response' },
    })
    await expect(pollRemoteTerminal(sessionId)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_status_response' },
    })
    await expect(writeRemoteTerminal(sessionId, btoa('hello'))).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_write_response' },
    })
    await expect(closeRemoteTerminal(sessionId)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_close_response' },
    })

    mockedInvoke.mockClear()
    await expect(writeRemoteTerminal(sessionId, 'not base64!')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_terminal_data_invalid' },
    })
    await expect(writeRemoteTerminal(sessionId, '')).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_terminal_data_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('rejects terminal read data above the command byte limit', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const oversized = btoa('x'.repeat(45_057))
    mockedInvoke.mockResolvedValue({
      result: 'read',
      value: { session_id: sessionId, output: { status: 'data', data: oversized } },
    })

    await expect(readRemoteTerminal(sessionId)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'invalid_remote_terminal_read_response' },
    })
  })

  it('requests terminal reads with the contract byte limit and rejects invalid limits', async () => {
    window.__TAURI_INTERNALS__ = {}
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedInvoke.mockResolvedValueOnce({
      result: 'read',
      value: { session_id: sessionId, output: { status: 'data', data: btoa('hello') } },
    })
    await expect(readRemoteTerminal(sessionId)).resolves.toMatchObject({
      kind: 'data',
      data: { status: 'data' },
    })
    expect(mockedInvoke).toHaveBeenLastCalledWith('remote_terminal', {
      command: { operation: 'read', session_id: sessionId, max_bytes: 45_056 },
    })

    mockedInvoke.mockResolvedValueOnce({
      result: 'read',
      value: { session_id: sessionId, output: { status: 'data', data: btoa('hello') } },
    })
    await expect(readRemoteTerminal(sessionId, 10_000)).resolves.toMatchObject({
      kind: 'data',
      data: { status: 'data' },
    })
    expect(mockedInvoke).toHaveBeenLastCalledWith('remote_terminal', {
      command: { operation: 'read', session_id: sessionId, max_bytes: 10_000 },
    })

    mockedInvoke.mockClear()
    await expect(readRemoteTerminal(sessionId, 0)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_terminal_read_limit_invalid' },
    })
    await expect(readRemoteTerminal(sessionId, 45_057)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_terminal_read_limit_invalid' },
    })
    await expect(readRemoteTerminal(sessionId, Number.NaN)).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_terminal_read_limit_invalid' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('normalizes WiFi dBm and bars without deriving missing measurements', () => {
    expect(normalizeSpeedTestDeepOutput({
      type: 'wifi_scan',
      payload: {
        scanned_at_unix_ms: 1_000,
        source: 'nmcli + iw scan dump',
        networks: [{
          ssid: 'Rhino-5G',
          signal_percent: 82,
          signal_dbm: -59,
          signal_bars: '▂▄▆█',
          channel: 36,
          band: '5 GHz',
          security: 'WPA2 WPA3',
        }],
        error: null,
      },
    })).toEqual({
      type: 'wifi_scan',
      payload: {
        scannedAtUnixMs: 1_000,
        source: 'nmcli + iw scan dump',
        networks: [{
          ssid: 'Rhino-5G',
          signalPercent: 82,
          signalDbm: -59,
          signalBars: '▂▄▆█',
          channel: 36,
          band: '5 GHz',
          security: 'WPA2 WPA3',
        }],
        error: null,
      },
    })

    expect(normalizeSpeedTestDeepOutput({
      type: 'wifi_scan',
      payload: {
        scanned_at_unix_ms: 1_001,
        source: 'nmcli',
        networks: [{
          ssid: 'No cached dBm',
          signal_percent: 40,
          signal_dbm: null,
          signal_bars: '▂▄__',
          channel: 11,
          band: '2.4 GHz',
          security: 'WPA2',
        }],
        error: null,
      },
    })).toMatchObject({
      type: 'wifi_scan',
      payload: { networks: [{ signalDbm: null, signalBars: '▂▄__' }] },
    })
  })

  it('normalizes the v2 multi-stream bandwidth contract and rejects invalid stream counts', () => {
    const payload = {
      schema_version: 2,
      started_at_unix_ms: 1_000,
      ended_at_unix_ms: 2_000,
      stages: [{
        stage: 'bandwidth',
        payload: {
          measurements: [{
            kind: 'international',
            label: '国际线路',
            source: 'speed.cloudflare.com',
            parallel_streams: 4,
            download_bits_per_second: 100_000_000,
            upload_bits_per_second: 50_000_000,
            http_code: 200,
            error: null,
          }],
        },
      }],
      cancelled: false,
      error: null,
    }
    expect(normalizeSpeedTestBasicEnd(payload)).toMatchObject({
      schemaVersion: 2,
      stages: [{ payload: { measurements: [{ parallelStreams: 4 }] } }],
    })
    expect(normalizeSpeedTestBasicEnd({
      ...payload,
      stages: [{
        ...payload.stages[0],
        payload: {
          measurements: [{
            ...payload.stages[0].payload.measurements[0],
            parallel_streams: 0,
          }],
        },
      }],
    })).toBeNull()
  })

  it('rejects invalid secret bytes and malformed references before invoking Tauri', async () => {
    window.__TAURI_INTERNALS__ = {}
    await expect(storeRemoteSecret('password', new Uint8Array())).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_secret_input_invalid', retryable: false },
    })
    await expect(storeRemoteSecret('password', new Uint8Array(8193))).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_secret_input_invalid', retryable: false },
    })
    await expect(deleteRemoteSecret({ backend: 'secret_service', item_id: 'not-a-uuid' })).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'remote_secret_reference_invalid', retryable: false },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })

  it('returns a factual unsupported error outside Tauri', async () => {
    await expect(getRemoteAdapterCatalog()).resolves.toMatchObject({
      kind: 'error',
      error: { code: 'desktop_bridge_unavailable' },
    })
    expect(mockedInvoke).not.toHaveBeenCalled()
  })
})
