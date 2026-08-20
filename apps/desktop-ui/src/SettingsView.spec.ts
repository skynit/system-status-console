import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { getBackendCapabilityReport, getSystemInfo, getUsageSummary } from './backend'
import SettingsView from './views/SettingsView.vue'

vi.mock('./backend', () => ({
  getBackendCapabilityReport: vi.fn(),
  getSystemInfo: vi.fn(),
  getUsageSummary: vi.fn(),
}))

const mockedReport = vi.mocked(getBackendCapabilityReport)
const mockedUsage = vi.mocked(getUsageSummary)
const mockedSystemInfo = vi.mocked(getSystemInfo)

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

const systemInfoPayload = {
  kind: 'systemInfo' as const,
  report: {
    schemaVersion: 1,
    capturedAtUnixMs: 1786523610724,
    toolVersion: 'fastfetch 2.67.0 (x86_64)',
    status: 'healthy' as const,
    reason: 'fastfetch_ok',
    retryable: false,
    sections: [
      {
        id: 'OS',
        groups: [
          {
            title: null,
            entries: [
              { key: 'os_name', value: 'CachyOS Linux' },
              { key: 'os_version', value: 'rolling · cachyos' },
            ],
          },
        ],
      },
      {
        id: 'CPU',
        groups: [
          {
            title: null,
            entries: [
              { key: 'cpu_name', value: 'AMD Ryzen AI 7 H 450' },
              { key: 'cores', value: '8 物理 / 16 逻辑' },
            ],
          },
        ],
      },
      {
        id: 'GPU',
        groups: [
          {
            title: 'NVIDIA RTX 4070',
            entries: [{ key: 'driver', value: 'nvidia · Discrete' }],
          },
        ],
      },
    ],
  },
}

async function mountReady(): Promise<ReturnType<typeof mount>> {
  mockedReport.mockResolvedValue(reportPayload)
  mockedUsage.mockResolvedValue(usagePayload)
  const wrapper = mount(SettingsView)
  await flushPromises()
  return wrapper
}

async function selectAppsSection(
  wrapper: ReturnType<typeof mount>,
  section: 'system' | 'remote' | 'data' | 'usage' | 'configuration',
): Promise<void> {
  await wrapper.find(`[data-settings-section="${section}"]`).trigger('click')
}

describe('SettingsView', () => {
  beforeEach(() => {
    mockedReport.mockReset()
    mockedUsage.mockReset()
    mockedSystemInfo.mockReset()
  })

  it('keeps refresh outside the primary tablist and supports section arrow navigation', async () => {
    const wrapper = await mountReady()
    const primaryTablist = wrapper.get('.settings-toolbar > .settings-tabs')
    const sectionTabs = wrapper.findAll('.settings-section-tab')

    expect(primaryTablist.attributes('role')).toBe('tablist')
    expect(primaryTablist.find('.settings-refresh').exists()).toBe(false)
    expect(sectionTabs).toHaveLength(5)
    const systemSection = wrapper.get('[data-settings-section="system"]')
    expect(systemSection.attributes('aria-selected')).toBe('true')

    await systemSection.trigger('keydown', { key: 'ArrowDown' })

    expect(wrapper.get('[data-settings-section="remote"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.text()).toContain('SMB2/3')
  })

  it('groups backend capability facts into system, remote and data sections', async () => {
    const wrapper = await mountReady()

    expect(wrapper.text()).toContain('应用设置')
    expect(wrapper.text()).toContain('系统信息')
    expect(wrapper.text()).toContain('appd 服务')
    expect(wrapper.text()).toContain('应用资源采集')
    expect(wrapper.text()).toContain('按应用流量')
    expect(wrapper.text()).toContain('unprivileged_bpf_permanently_disabled')
    expect(wrapper.findAll('.settings-row')).toHaveLength(5)

    await selectAppsSection(wrapper, 'remote')
    expect(wrapper.text()).toContain('SSH')
    expect(wrapper.text()).toContain('SMB2/3')
    expect(wrapper.text()).toContain('plain_ftp_explicitly_enabled')
    expect(wrapper.findAll('.settings-row')).toHaveLength(4)

    await selectAppsSection(wrapper, 'data')
    expect(wrapper.text()).toContain('传输队列')
    expect(wrapper.text()).toContain('日志')
    expect(wrapper.findAll('.settings-row')).toHaveLength(2)
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
    const wrapper = await mountReady()
    await selectAppsSection(wrapper, 'usage')

    expect(wrapper.text()).toContain('统计始于')
    expect(wrapper.text()).toContain('2026-08-12')
    expect(wrapper.text()).toContain('窗口处于前台聚焦、会话已解锁，且最近 5 分钟内有输入')
    expect(wrapper.text()).toContain('usage_historical_gaps_present')
    expect(wrapper.text()).toContain('已覆盖完整周期起点')
    expect(wrapper.text()).toContain('connected')
    expect(wrapper.text()).toContain('available')
  })

  it('shows six not-implemented configuration entries', async () => {
    const wrapper = await mountReady()
    await selectAppsSection(wrapper, 'configuration')

    expect(wrapper.get('#settings-panel-apps').exists()).toBe(true)
    expect(wrapper.findAll('#settings-panel-apps .settings-row')).toHaveLength(6)
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
    await selectAppsSection(wrapper, 'usage')

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
    await selectAppsSection(wrapper, 'data')

    expect(wrapper.text()).toContain('future.feature.v1')
    expect(wrapper.text()).toContain('后端声明的运行能力')
  })

  it('loads system info lazily when the system tab is opened', async () => {
    mockedSystemInfo.mockResolvedValue(systemInfoPayload)
    const wrapper = await mountReady()

    expect(mockedSystemInfo).not.toHaveBeenCalled()
    expect(wrapper.find('#settings-panel-system').exists()).toBe(false)

    await wrapper.find('[data-settings-tab="system"]').trigger('click')
    await flushPromises()

    expect(mockedSystemInfo).toHaveBeenCalledTimes(1)
    expect(wrapper.find('#settings-panel-system').exists()).toBe(true)
  })

  it('renders healthy fastfetch facts with aligned sections and device groups', async () => {
    mockedSystemInfo.mockResolvedValue(systemInfoPayload)
    const wrapper = await mountReady()

    await wrapper.find('[data-settings-tab="system"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('fastfetch 2.67.0 (x86_64)')
    expect(wrapper.text()).toContain('healthy')
    expect(wrapper.text()).toContain('操作系统')
    expect(wrapper.text()).toContain('CachyOS Linux')
    expect(wrapper.text()).toContain('rolling · cachyos')
    expect(wrapper.text()).toContain('处理器')
    expect(wrapper.text()).toContain('8 物理 / 16 逻辑')
    expect(wrapper.text()).toContain('NVIDIA RTX 4070')
    expect(wrapper.text()).toContain('nvidia · Discrete')

    const blocks = wrapper.findAll('.sys-block')
    expect(blocks.length).toBe(3)
    expect(wrapper.findAll('.sys-title').length).toBe(3)
    expect(wrapper.find('.sys-pairs').exists()).toBe(true)
  })

  it('shows an unsupported band with reason when fastfetch is missing', async () => {
    mockedSystemInfo.mockResolvedValue({
      kind: 'systemInfo',
      report: {
        schemaVersion: 1,
        capturedAtUnixMs: null,
        toolVersion: null,
        status: 'unsupported',
        reason: 'fastfetch_not_found',
        retryable: false,
        sections: [],
      },
    })
    const wrapper = await mountReady()

    await wrapper.find('[data-settings-tab="system"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('系统信息不可用')
    expect(wrapper.text()).toContain('unsupported')
    expect(wrapper.text()).toContain('fastfetch_not_found')
    expect(wrapper.find('.sys-pairs').exists()).toBe(false)
  })

  it('surfaces a system-info transport error and retries', async () => {
    mockedSystemInfo.mockResolvedValueOnce({
      kind: 'error',
      error: { kind: 'transport', code: 'transport', reason: 'system_info_unreachable', retryable: true },
    })
    const wrapper = await mountReady()

    await wrapper.find('[data-settings-tab="system"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('系统信息不可用')
    expect(wrapper.text()).toContain('system_info_unreachable')

    mockedSystemInfo.mockResolvedValue(systemInfoPayload)
    await wrapper.find('.settings-retry').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('CachyOS Linux')
    expect(wrapper.find('.settings-error').exists()).toBe(false)
  })

  it('keeps stale system facts visible when a refresh fails', async () => {
    mockedSystemInfo.mockResolvedValue(systemInfoPayload)
    const wrapper = await mountReady()
    await wrapper.find('[data-settings-tab="system"]').trigger('click')
    await flushPromises()

    mockedSystemInfo.mockResolvedValueOnce({
      kind: 'error',
      error: { kind: 'daemon', code: 'daemon', reason: 'system_info_daemon_error', retryable: true },
    })
    await wrapper.find('.settings-refresh').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('系统信息刷新失败，正在显示上一次成功数据')
    expect(wrapper.text()).toContain('system_info_daemon_error')
    expect(wrapper.text()).toContain('CachyOS Linux')
  })
})
