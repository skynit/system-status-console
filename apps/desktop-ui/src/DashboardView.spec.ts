import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { getBackendCapabilityReport } from './backend'
import DashboardView from './views/DashboardView.vue'

vi.mock('./backend', () => ({
  getBackendCapabilityReport: vi.fn(),
  getBackendHealth: vi.fn().mockResolvedValue({
    status: 'unsupported',
    capabilityReason: 'desktop_bridge_unavailable',
  }),
}))

const mockedReport = vi.mocked(getBackendCapabilityReport)

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}

describe('DashboardView capability catalog', () => {
  beforeEach(() => {
    mockedReport.mockReset()
  })

  it('renders the runtime statuses returned by appd', async () => {
    mockedReport.mockResolvedValue({
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

    const wrapper = mount(DashboardView, {
      global: {
        stubs: {
          RouterLink: {
            props: ['to'],
            template: '<a class="router-link-stub" :data-path="typeof to === \'string\' ? to : to.path" :data-query="to.query?.tab ?? to.query?.panel ?? \'\'"><slot /></a>',
          },
        },
      },
    })
    await flushPromises()

    expect(wrapper.findAll('.capability-row')).toHaveLength(3)
    expect(wrapper.text()).toContain('实时能力目录')
    expect(wrapper.text()).toContain('3 项')
    expect(wrapper.text()).toContain('appd 服务')
    expect(wrapper.text()).toContain('应用资源')
    expect(wrapper.text()).toContain('按应用流量')
    expect(wrapper.find('.capability-row.is-healthy').text()).toContain('appd_online')
    expect(wrapper.find('.capability-row.is-degraded').text()).toContain('telemetry_partial')
    expect(wrapper.find('.capability-row.is-unsupported').text()).toContain('unprivileged_bpf_permanently_disabled')
    expect(wrapper.findAll('.router-link-stub')).toHaveLength(2)
    expect(wrapper.findAll('.router-link-stub')[0].attributes('data-path')).toBe('/applications')
    expect(wrapper.findAll('.router-link-stub')[0].attributes('data-query')).toBe('resources')
    expect(wrapper.findAll('.router-link-stub')[0].attributes('aria-label')).toBe('应用资源：degraded，telemetry_partial；进入详情')
    expect(wrapper.findAll('.router-link-stub')[1].attributes('data-path')).toBe('/network')
    expect(wrapper.findAll('.router-link-stub')[1].attributes('data-query')).toBe('applications')
    expect(wrapper.find('.capability-row:not(.is-link)').attributes('aria-label')).toBeUndefined()
  })

  it('shows the exact bridge error without fabricated capability rows', async () => {
    mockedReport.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'appd_socket_unavailable',
        reason: 'appd_socket_unavailable',
        retryable: true,
      },
    })

    const wrapper = mount(DashboardView, {
      global: { stubs: { RouterLink: true } },
    })
    await flushPromises()

    expect(wrapper.text()).toContain('能力目录不可用')
    expect(wrapper.findAll('.capability-row')).toHaveLength(1)
    expect(wrapper.find('.capability-row.is-unreachable').text()).toContain('appd_socket_unavailable')
    expect(wrapper.text()).not.toContain('not_implemented')
  })

  it('keeps the last successful capability catalog when refresh fails', async () => {
    mockedReport
      .mockResolvedValueOnce({
        kind: 'report',
        report: {
          daemonVersion: '0.1.0',
          health: { status: 'healthy', capabilityReason: 'appd_online' },
          capabilities: [
            { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
          ],
        },
      })
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'appd_socket_unavailable',
          reason: 'appd_socket_unavailable',
          retryable: true,
        },
      })

    const wrapper = mount(DashboardView, {
      global: { stubs: { RouterLink: true } },
    })
    await flushPromises()
    await wrapper.get('button.dashboard-refresh').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('能力目录刷新失败')
    expect(wrapper.text()).toContain('appd_online')
    expect(wrapper.text()).toContain('appd_socket_unavailable')
    expect(wrapper.findAll('.capability-row')).toHaveLength(2)
  })

  it('keeps the current catalog visible and prevents overlapping refresh requests', async () => {
    const pendingRefresh = deferred<Awaited<ReturnType<typeof getBackendCapabilityReport>>>()
    mockedReport
      .mockResolvedValueOnce({
        kind: 'report',
        report: {
          daemonVersion: '0.1.0',
          health: { status: 'healthy', capabilityReason: 'appd_online' },
          capabilities: [
            { id: 'appd.health.v1', status: 'healthy', reason: 'appd_online' },
          ],
        },
      })
      .mockReturnValueOnce(pendingRefresh.promise)

    const wrapper = mount(DashboardView, {
      global: { stubs: { RouterLink: true } },
    })
    await flushPromises()

    await wrapper.get('button.dashboard-refresh').trigger('click')
    expect(wrapper.text()).toContain('正在刷新能力目录')
    expect(wrapper.text()).toContain('appd_online')
    expect(wrapper.get('.capability-list').attributes('aria-busy')).toBe('true')
    expect(mockedReport).toHaveBeenCalledTimes(2)

    await wrapper.get('button.dashboard-refresh').trigger('click')
    expect(mockedReport).toHaveBeenCalledTimes(2)

    pendingRefresh.resolve({
      kind: 'report',
      report: {
        daemonVersion: '0.1.0',
        health: { status: 'healthy', capabilityReason: 'appd_online' },
        capabilities: [
          { id: 'appd.health.v1', status: 'healthy', reason: 'appd_refreshed' },
        ],
      },
    })
    await flushPromises()

    expect(wrapper.text()).toContain('实时能力目录')
    expect(wrapper.text()).toContain('appd_refreshed')
    expect(wrapper.get('.capability-list').attributes('aria-busy')).toBe('false')
  })
})
