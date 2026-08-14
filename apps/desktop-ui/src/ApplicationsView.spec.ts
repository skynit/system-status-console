import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { getBackendHealth, getTelemetrySnapshot, getUsageSummary } from './backend'
import ApplicationsView from './views/ApplicationsView.vue'
import { createAppRouter, createMemoryHistory } from './router'

vi.mock('./backend', () => ({
  getBackendHealth: vi.fn(),
  getTelemetrySnapshot: vi.fn(),
  getUsageSummary: vi.fn(),
}))

const mockedHealth = vi.mocked(getBackendHealth)
const mockedTelemetry = vi.mocked(getTelemetrySnapshot)
const mockedUsage = vi.mocked(getUsageSummary)

enableAutoUnmount(afterEach)
afterEach(() => vi.useRealTimers())

async function mountView(path = '/applications', attachTo?: HTMLElement) {
  const router = createAppRouter(createMemoryHistory())
  await router.push(path)
  await router.isReady()
  return mount(ApplicationsView, {
    ...(attachTo ? { attachTo } : {}),
    global: { plugins: [router] },
  })
}

function metric(value: number | null, state: 'known' | 'unknown' | 'permission_denied' = value === null ? 'unknown' : 'known') {
  return {
    value,
    state,
    reason: state === 'known' ? null : `metric_${state}`,
  }
}

function snapshot(applications = [{
  applicationKey: 'org.example.App',
  desktopEntryId: 'org.example.App.desktop',
  displayLabel: 'Example App',
  groupingResolution: 'desktop_entry_exact' as const,
  processCount: 2,
  processScope: 'same_euid',
  cgroupScope: 'full_cgroup',
  cpuPercentTotalCapacity: metric(12.5),
  cgroupCpuPercentTotalCapacity: metric(10),
  rssBytes: metric(1_048_576),
  pssBytes: metric(786_432),
  memoryCurrentBytes: metric(2_097_152),
  cgroupProcessCount: metric(3),
  fdUsed: metric(4),
  fdSoftLimit: metric(1024),
  fdPercentOfAttributed: metric(50),
  fdPercentOfSoftLimit: metric(0.390625),
  fdMaxProcessPercentOfSoftLimit: metric(90),
}]) {
  return {
    schemaVersion: 4 as const,
    snapshotId: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    capturedAtUnixMs: 1_786_154_400_000,
    sampleIntervalMs: 2_000,
    logicalCpuCount: 16,
    freshness: 'fresh' as const,
    status: 'complete' as const,
    reason: 'telemetry_healthy',
    retryable: false,
    scope: 'same_euid',
    lastSuccessAtUnixMs: 1_786_154_400_000,
    permissionDeniedCounts: [],
    issues: [],
    systemFd: {
      scope: 'system',
      fileNrAllocated: metric(10),
      fileNrMax: metric(0),
      fileMax: metric(100),
      pressurePercent: metric(10),
    },
    applications,
  }
}

function usageSummary(applications = [{
  appId: 'org.example.Editor',
  bucketKey: '2026-08-10',
  timezoneId: 'Asia/Shanghai',
  utcOffsetSeconds: 28_800,
  durationNs: 5_405_000_000_000,
  lastWallUtcMs: 1_786_154_400_000,
}]) {
  return {
    schemaVersion: 3 as const,
    snapshotId: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    capturedAtUnixMs: 1_786_154_400_000,
    query: {
      period: 'daily' as const,
      bucketKey: '2026-08-10',
    },
    status: 'healthy' as const,
    reason: 'usage_tracking_healthy',
    retryable: false,
    coverage: {
      status: 'healthy' as const,
      reason: 'usage_coverage_complete',
      niriEventStreamConnected: true,
      logindSessionAvailable: true,
      eventGapCount: 0,
      lastCheckpointUnixMs: 1_786_154_400_000,
      trackingStartedUnixMs: 1_786_118_400_000,
      bucketStartCovered: true,
      definition: 'foreground_unlocked_input_active_300s_monotonic' as const,
    },
    applications,
  }
}

describe('ApplicationsView', () => {
  beforeEach(() => {
    mockedHealth.mockReset()
    mockedTelemetry.mockReset()
    mockedUsage.mockReset()
    mockedHealth.mockResolvedValue({ status: 'healthy', capabilityReason: 'appd_online' })
    mockedUsage.mockResolvedValue({ kind: 'summary', summary: usageSummary([]) })
  })

  it('renders only backend-provided application metrics', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot() })
    const wrapper = await mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('Example App')
    expect(wrapper.text()).toContain('12.5%')
    expect(wrapper.text()).toContain('1.0 MiB')
    expect(wrapper.text()).toContain('已用 4')
    expect(wrapper.text()).toContain('软限制 1024 · 0.4%')
    expect(wrapper.text()).not.toContain('4 / 1024')
    expect(wrapper.text()).toContain('telemetry_healthy')
    expect(wrapper.findAll('tbody tr')).toHaveLength(1)
    expect(wrapper.findAll('colgroup col')).toHaveLength(6)
  })

  it('sorts resource rows by text and numeric metrics while keeping unavailable values last', async () => {
    const template = snapshot().applications[0]
    mockedTelemetry.mockResolvedValue({
      kind: 'snapshot',
      snapshot: snapshot([
        {
          ...template,
          applicationKey: 'beta.app',
          displayLabel: 'Beta',
          cpuPercentTotalCapacity: metric(20),
        },
        {
          ...template,
          applicationKey: 'alpha.app',
          displayLabel: 'Alpha',
          cpuPercentTotalCapacity: metric(5),
        },
        {
          ...template,
          applicationKey: 'gamma.app',
          displayLabel: 'Gamma',
          cpuPercentTotalCapacity: metric(null, 'permission_denied'),
        },
      ]),
    })
    const wrapper = await mountView()
    await flushPromises()
    const labels = () => wrapper.findAll('.application-label').map((label) => label.text())

    expect(wrapper.findAll('.telemetry-sort-button')).toHaveLength(6)
    expect(labels()).toEqual(['Beta', 'Alpha', 'Gamma'])

    await wrapper.get('[data-sort-key="application"]').trigger('click')
    expect(wrapper.get('thead th:nth-child(1)').attributes('aria-sort')).toBe('ascending')
    expect(labels()).toEqual(['Alpha', 'Beta', 'Gamma'])

    await wrapper.get('[data-sort-key="application"]').trigger('click')
    expect(wrapper.get('thead th:nth-child(1)').attributes('aria-sort')).toBe('descending')
    expect(labels()).toEqual(['Gamma', 'Beta', 'Alpha'])

    await wrapper.get('[data-sort-key="cpu"]').trigger('click')
    expect(wrapper.get('thead th:nth-child(1)').attributes('aria-sort')).toBe('none')
    expect(wrapper.get('thead th:nth-child(2)').attributes('aria-sort')).toBe('ascending')
    expect(labels()).toEqual(['Alpha', 'Beta', 'Gamma'])

    await wrapper.get('[data-sort-key="cpu"]').trigger('click')
    expect(wrapper.get('thead th:nth-child(2)').attributes('aria-sort')).toBe('descending')
    expect(labels()).toEqual(['Beta', 'Alpha', 'Gamma'])
  })

  it('refreshes resources from the freshness control', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot() })
    const wrapper = await mountView()
    await flushPromises()

    const freshness = wrapper.get('button.freshness-label')
    expect(freshness.attributes('aria-label')).toBe('数据新鲜度 fresh，刷新应用资源')

    await freshness.trigger('click')
    await flushPromises()

    expect(mockedTelemetry).toHaveBeenCalledTimes(2)
  })

  it('keeps resource refresh busy until its own request completes after a panel switch', async () => {
    let finishTelemetry!: () => void
    mockedTelemetry.mockReturnValue(new Promise((resolve) => {
      finishTelemetry = () => resolve({ kind: 'snapshot', snapshot: snapshot() })
    }))
    const wrapper = await mountView()
    await flushPromises()

    expect(wrapper.get('.applications-refresh').attributes('disabled')).toBeDefined()
    await wrapper.get('[data-application-panel="usage"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('.applications-refresh').attributes('disabled')).toBeUndefined()

    await wrapper.get('[data-application-panel="resources"]').trigger('click')
    expect(wrapper.get('.applications-refresh').attributes('disabled')).toBeDefined()
    finishTelemetry()
    await flushPromises()
    expect(wrapper.get('.applications-refresh').attributes('disabled')).toBeUndefined()
  })

  it('keeps the last successful resource snapshot when refresh fails', async () => {
    mockedTelemetry
      .mockResolvedValueOnce({ kind: 'snapshot', snapshot: snapshot() })
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'appd_socket_unavailable',
          reason: 'appd_socket_unavailable',
          retryable: true,
        },
      })
    const wrapper = await mountView()
    await flushPromises()

    await wrapper.get('button.freshness-label').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Example App')
    expect(wrapper.text()).toContain('刷新失败，正在显示上一次成功数据')
    expect(wrapper.get('.telemetry-refresh-error').text()).toContain('appd_socket_unavailable')
    expect(wrapper.find('.telemetry-state.is-error').exists()).toBe(false)
  })

  it('refreshes the active resource tab every second and pauses on the usage tab', async () => {
    vi.useFakeTimers()
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot() })
    const wrapper = await mountView()
    await flushPromises()

    expect(mockedTelemetry).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(999)
    expect(mockedTelemetry).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(1)
    await flushPromises()
    expect(mockedTelemetry).toHaveBeenCalledTimes(2)

    await wrapper.get('[data-application-panel="usage"]').trigger('click')
    await flushPromises()
    await vi.advanceTimersByTimeAsync(1_000)
    expect(mockedTelemetry).toHaveBeenCalledTimes(2)

    await wrapper.get('[data-application-panel="resources"]').trigger('click')
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(mockedTelemetry).toHaveBeenCalledTimes(3)
  })

  it('labels large file-descriptor limits and preserves attribution state', async () => {
    const application = snapshot().applications[0]
    application.fdSoftLimit = metric(1_441_792)
    application.fdPercentOfAttributed = metric(null, 'permission_denied')
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([application]) })
    const wrapper = await mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('已用 4')
    expect(wrapper.text()).toContain('软限制 1441792 · 0.4%')
    expect(wrapper.text()).toContain('归因 permission_denied')
    expect(wrapper.get('.fd-soft-limit').attributes('title')).toBe('软限制 1441792 · 0.4%')
    expect(wrapper.text()).not.toContain('4 / 1441792')
  })

  it('renders the factual empty state without sample rows', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([]) })
    const wrapper = await mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('暂无可用数据')
    expect(wrapper.find('tbody').exists()).toBe(false)
  })

  it('renders typed bridge failure and supports retry', async () => {
    mockedTelemetry.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'appd_socket_unavailable',
        reason: 'appd_socket_unavailable',
        retryable: true,
      },
    })
    const wrapper = await mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('应用资源不可用')
    expect(wrapper.text()).toContain('appd_socket_unavailable')
    expect(wrapper.text()).toContain('unreachable')
    await wrapper.get('.telemetry-state .quiet-action').trigger('click')
    await flushPromises()
    expect(mockedTelemetry).toHaveBeenCalledTimes(2)
  })

  it('loads usage lazily and renders only returned durations and coverage facts', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([]) })
    mockedUsage.mockResolvedValue({ kind: 'summary', summary: usageSummary() })
    const wrapper = await mountView()
    await flushPromises()

    expect(mockedUsage).not.toHaveBeenCalled()
    await wrapper.get('[data-application-panel="usage"]').trigger('click')
    await flushPromises()

    expect(mockedUsage).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('org.example.Editor')
    expect(wrapper.text()).toContain('1 小时 30 分钟 5 秒')
    expect(wrapper.text()).toContain('100.0%')
    expect(wrapper.text()).toContain('usage_coverage_complete')
    expect(wrapper.text()).toContain('统计始于')
    expect(wrapper.text()).toContain('前台时长')
    expect(wrapper.text()).toContain('最后前台')
    expect(wrapper.text()).toContain('已记录占比')
    expect(wrapper.text()).toContain('已覆盖完整周期起点')
    expect(wrapper.text()).toContain('窗口处于前台聚焦、会话已解锁，且最近 5 分钟内有输入')
    expect(wrapper.text()).not.toContain('foreground_unlocked_input_active_300s_monotonic')
  })

  it('keeps seconds visible after a duration crosses one minute', async () => {
    const summary = usageSummary([
      {
        appId: 'org.example.MinuteBoundary',
        bucketKey: '2026-08-10',
        timezoneId: 'Asia/Shanghai',
        utcOffsetSeconds: 28_800,
        durationNs: 119_000_000_000,
        lastWallUtcMs: 1_786_154_400_000,
      },
    ])
    mockedUsage.mockResolvedValue({ kind: 'summary', summary })

    const wrapper = await mountView('/applications?panel=usage')
    await flushPromises()

    expect(wrapper.text()).toContain('1 分钟 59 秒')
    expect(wrapper.text()).not.toContain('1 分钟100.0%')
  })

  it('shows an incomplete epoch bucket as degraded with its factual start time', async () => {
    const partial = usageSummary()
    partial.status = 'degraded'
    partial.reason = 'usage_tracking_epoch_partial'
    partial.retryable = true
    partial.coverage.status = 'degraded'
    partial.coverage.reason = 'usage_tracking_epoch_partial'
    partial.coverage.bucketStartCovered = false
    mockedUsage.mockResolvedValue({ kind: 'summary', summary: partial })

    const wrapper = await mountView('/applications?panel=usage')
    await flushPromises()

    expect(wrapper.text()).toContain('usage_tracking_epoch_partial')
    expect(wrapper.text()).toContain('统计始于')
    expect(wrapper.text()).toContain('仅包含统计开始后的记录')
    expect(wrapper.find('.usage-fact-row .is-degraded').exists()).toBe(true)
  })

  it('refreshes the current usage bucket every ten seconds but leaves history stable', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 12, 12, 0, 0))
    const wrapper = await mountView('/applications?panel=usage')
    await flushPromises()

    expect(mockedUsage).toHaveBeenCalledTimes(1)
    expect(mockedUsage).toHaveBeenLastCalledWith({ period: 'daily', bucketKey: '2026-08-12' })

    await vi.advanceTimersByTimeAsync(9_999)
    expect(mockedUsage).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushPromises()
    expect(mockedUsage).toHaveBeenCalledTimes(2)

    await wrapper.get('[aria-label="上一个时间段"]').trigger('click')
    await flushPromises()
    expect(mockedUsage).toHaveBeenCalledTimes(3)
    expect(mockedUsage).toHaveBeenLastCalledWith({ period: 'daily', bucketKey: '2026-08-11' })

    await vi.advanceTimersByTimeAsync(10_000)
    await flushPromises()
    expect(mockedUsage).toHaveBeenCalledTimes(3)
  })

  it('follows the next local day when the current usage bucket crosses midnight', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(2026, 7, 12, 23, 59, 55))
    await mountView('/applications?panel=usage')
    await flushPromises()

    expect(mockedUsage).toHaveBeenLastCalledWith({ period: 'daily', bucketKey: '2026-08-12' })

    vi.setSystemTime(new Date(2026, 7, 13, 0, 0, 5))
    await vi.advanceTimersByTimeAsync(10_000)
    await flushPromises()

    expect(mockedUsage).toHaveBeenCalledTimes(2)
    expect(mockedUsage).toHaveBeenLastCalledWith({ period: 'daily', bucketKey: '2026-08-13' })
  })

  it('manually refreshes usage from the shared refresh control', async () => {
    const wrapper = await mountView('/applications?panel=usage')
    await flushPromises()

    const refresh = wrapper.get('button.applications-refresh')
    expect(refresh.attributes('aria-label')).toBe('刷新使用时间')
    await refresh.trigger('click')
    await flushPromises()

    expect(mockedUsage).toHaveBeenCalledTimes(2)
  })

  it('starts directly in weekly usage without requesting resource telemetry', async () => {
    mockedUsage.mockResolvedValue({ kind: 'summary', summary: usageSummary([]) })
    const wrapper = await mountView('/applications?panel=usage&period=weekly')
    await flushPromises()

    expect(wrapper.get('[data-application-panel="usage"]').attributes('aria-selected')).toBe('true')
    expect(mockedTelemetry).not.toHaveBeenCalled()
    expect(mockedUsage).toHaveBeenCalledTimes(1)
    expect(mockedUsage).toHaveBeenCalledWith({
      period: 'weekly',
      bucketKey: expect.stringMatching(/^\d{4}-W\d{2}$/),
    })
  })

  it('requests a weekly bucket and supports typed usage retry', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([]) })
    mockedUsage
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'usage_summary_unreachable',
          reason: 'usage_summary_unreachable',
          retryable: true,
        },
      })
      .mockResolvedValueOnce({ kind: 'summary', summary: usageSummary([]) })
      .mockResolvedValueOnce({ kind: 'summary', summary: usageSummary([]) })
    const wrapper = await mountView()
    await flushPromises()

    await wrapper.get('[data-application-panel="usage"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('使用时间不可用')
    expect(wrapper.text()).toContain('usage_summary_unreachable')

    await wrapper.get('.usage-state .quiet-action').trigger('click')
    await flushPromises()
    expect(mockedUsage).toHaveBeenCalledTimes(2)

    await wrapper.get('.usage-period-control button:nth-child(2)').trigger('click')
    await flushPromises()
    expect(mockedUsage).toHaveBeenLastCalledWith(expect.objectContaining({
      period: 'weekly',
      bucketKey: expect.stringMatching(/^\d{4}-W\d{2}$/),
    }))
  })

  it('supports arrow-key navigation between resource and usage tabs', async () => {
    mockedTelemetry.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([]) })
    const wrapper = await mountView('/applications', document.body)
    await flushPromises()

    await wrapper.get('[data-application-panel="resources"]').trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    await new Promise((resolve) => requestAnimationFrame(resolve))

    expect(wrapper.get('[data-application-panel="usage"]').attributes('aria-selected')).toBe('true')
    expect(document.activeElement).toBe(wrapper.get('[data-application-panel="usage"]').element)
  })

  it('moves selection and focus across daily and weekly usage tabs', async () => {
    const wrapper = await mountView('/applications?panel=usage', document.body)
    await flushPromises()

    const daily = wrapper.get('[data-usage-period="daily"]')
    expect(wrapper.get('.usage-table-wrap').exists()).toBe(true)
    expect(daily.attributes('role')).toBe('tab')
    expect(daily.attributes('aria-selected')).toBe('true')

    await daily.trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    await new Promise((resolve) => requestAnimationFrame(resolve))
    const weekly = wrapper.get('[data-usage-period="weekly"]')
    expect(weekly.attributes('aria-selected')).toBe('true')
    expect(weekly.attributes('tabindex')).toBe('0')
    expect(document.activeElement).toBe(weekly.element)
    expect(mockedUsage).toHaveBeenLastCalledWith(expect.objectContaining({ period: 'weekly' }))

    await weekly.trigger('keydown', { key: 'Home' })
    await flushPromises()
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(document.activeElement).toBe(wrapper.get('[data-usage-period="daily"]').element)
  })
})
