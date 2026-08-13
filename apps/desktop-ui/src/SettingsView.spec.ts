import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { getBackendCapabilityReport, getUsageSummary } from './backend'
import SettingsView from './views/SettingsView.vue'

vi.mock('./backend', () => ({
  getBackendCapabilityReport: vi.fn(),
  getUsageSummary: vi.fn(),
}))

const mockedReport = vi.mocked(getBackendCapabilityReport)
const mockedUsage = vi.mocked(getUsageSummary)

const reportPayload = {
  kind: 'report' as const,
  report: {
    daemonVersion: '0.1.0',
    health: { status: 'degraded', capabilityReason: 'telemetry_partial' },
    capabilities: [
      { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
      { id: 'telemetry.snapshot.v1', status: 'degraded', reason: 'telemetry_partial' },
      { id: 'network.system.v1', status: 'healthy', reason: 'rtnetlink_system_counters_available' },
      { id: 'network.per_app.v1', status: 'unsupported', reason: 'unprivileged_bpf_permanently_disabled' },
      { id: 'usage.foreground.v1', status: 'healthy', reason: 'usage_tracking_active' },
      { id: 'remote.ssh.v1', status: 'healthy', reason: 'remote_adapter_available' },
      { id: 'remote.sftp.v1', status: 'healthy', reason: 'remote_adapter_available' },
      { id: 'remote.ftp.v1', status: 'degraded', reason: 'plain_ftp_explicitly_enabled' },
      { id: 'remote.smb.v1', status: 'healthy', reason: 'remote_adapter_available' },
      { id: 'transfers.v1', status: 'healthy', reason: 'transfer_runner_active_public_commands_available' },
      { id: 'notes.v1', status: 'healthy', reason: 'notes_ready' },
    ],
  },
}

const usagePayload = {
  kind: 'summary' as const,
  summary: {
    schemaVersion: 3,
    snapshotId: 'snap-1',
    capturedAtUnixMs: 1786523610724,
    query: { period: 'daily', bucketKey: '2026-08-13' },
    status: 'degraded',
    reason: 'usage_historical_gaps_present',
    retryable: false,
    coverage: {
      status: 'degraded',
      reason: 'usage_historical_gaps_present',
      niriEventStreamConnected: true,
      logindSessionAvailable: true,
      eventGapCount: 2,
      lastCheckpointUnixMs: 1786523610724,
      trackingStartedUnixMs: 1786523610724,
      bucketStartCovered: true,
      definition: 'foreground_unlocked_input_active_300s_monotonic',
    },
    applications: [],
  },
}

describe('SettingsView', () => {
  beforeEach(() => {
    mockedReport.mockReset()
    mockedUsage.mockReset()
  })

  it('groups the backend capability facts by system, remote and data', async () => {
    mockedReport.mockResolvedValue(reportPayload)
    mockedUsage.mockResolvedValue(usagePayload)

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.text()).toContain('设置')
    expect(wrapper.text()).toContain('appd 服务')
    expect(wrapper.text()).toContain('应用资源采集')
    expect(wrapper.text()).toContain('按应用流量')
    expect(wrapper.text()).toContain('SSH')
    expect(wrapper.text()).toContain('SMB2/3')
    expect(wrapper.text()).toContain('传输队列')
    expect(wrapper.text()).toContain('备忘录')
    expect(wrapper.text()).toContain('unprivileged_bpf_permanently_disabled')
    expect(wrapper.text()).toContain('plain_ftp_explicitly_enabled')
    expect(wrapper.findAll('.settings-row').length).toBeGreaterThanOrEqual(11)
    expect(wrapper.findAll('.settings-token.is-healthy').length).toBe(10)
  })

  it('marks configured-but-unavailable capability rows as unsupported', async () => {
    mockedReport.mockResolvedValue({
      kind: 'report',
      report: {
        daemonVersion: '0.1.0',
        health: { status: 'healthy', capabilityReason: 'appd_online' },
        capabilities: [
          { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
        ],
      },
    })
    mockedUsage.mockResolvedValue(usagePayload)

    const wrapper = mount(SettingsView)
    await flushPromises()

    const tokens = wrapper.findAll('.settings-token.is-unsupported')
    expect(tokens.length).toBeGreaterThanOrEqual(1)
    expect(wrapper.text()).toContain('unknown_capability')
  })

  it('renders the usage scope facts from the backend summary', async () => {
    mockedReport.mockResolvedValue(reportPayload)
    mockedUsage.mockResolvedValue(usagePayload)

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.text()).toContain('统计始于')
    expect(wrapper.text()).toContain('2026-08-12')
    expect(wrapper.text()).toContain('窗口处于前台聚焦、会话已解锁，且最近 5 分钟内有输入')
    expect(wrapper.text()).toContain('usage_historical_gaps_present')
    expect(wrapper.text()).toContain('已覆盖完整周期起点')
    expect(wrapper.text()).toContain('connected')
    expect(wrapper.text()).toContain('available')
  })

  it('shows six not-implemented configuration entries', async () => {
    mockedReport.mockResolvedValue(reportPayload)
    mockedUsage.mockResolvedValue(usagePayload)

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.text()).toContain('采集周期')
    expect(wrapper.text()).toContain('数据保留期')
    expect(wrapper.text()).toContain('通知')
    expect(wrapper.text()).toContain('快捷键')
    expect(wrapper.text()).toContain('隐私偏好')
    expect(wrapper.text()).toContain('远程连接默认值')
    expect(wrapper.text()).toContain('not_implemented')
  })

  it('surfaces an unreachable daemon and retries the facts', async () => {
    mockedReport.mockResolvedValueOnce({
      kind: 'error',
      error: { kind: 'transport', code: 'transport', reason: 'daemon_unreachable', retryable: true },
    })
    mockedUsage.mockResolvedValueOnce({
      kind: 'error',
      error: { kind: 'transport', code: 'transport', reason: 'daemon_unreachable', retryable: true },
    })

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.find('.settings-error').exists()).toBe(true)
    expect(wrapper.text()).toContain('设置事实不可用')
    expect(wrapper.text()).toContain('daemon_unreachable')

    mockedReport.mockResolvedValue(reportPayload)
    mockedUsage.mockResolvedValue(usagePayload)
    await wrapper.find('.settings-retry').trigger('click')
    await flushPromises()

    expect(wrapper.find('.settings-error').exists()).toBe(false)
    expect(wrapper.text()).toContain('appd 服务')
  })

  it('renders a usage failure as a degraded fact row with its reason', async () => {
    mockedReport.mockResolvedValue(reportPayload)
    mockedUsage.mockResolvedValue({
      kind: 'error',
      error: { kind: 'daemon', code: 'usage', reason: 'usage_database_corrupt', retryable: false },
    })

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.text()).toContain('使用时间')
    expect(wrapper.text()).toContain('degraded')
    expect(wrapper.text()).toContain('usage_database_corrupt')
  })

  it('shows unknown backend capabilities in the other-capabilities band', async () => {
    mockedReport.mockResolvedValue({
      kind: 'report',
      report: {
        daemonVersion: '0.1.0',
        health: { status: 'healthy', capabilityReason: 'appd_online' },
        capabilities: [
          { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
          { id: 'future.feature.v1', status: 'unsupported', reason: 'not_implemented' },
        ],
      },
    })
    mockedUsage.mockResolvedValue(usagePayload)

    const wrapper = mount(SettingsView)
    await flushPromises()

    expect(wrapper.text()).toContain('future.feature.v1')
    expect(wrapper.text()).toContain('后端声明的运行能力')
  })
})
