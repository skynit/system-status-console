import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { getNetworkSnapshot } from './backend'
import NetworkView from './views/NetworkView.vue'
import type { NetworkInterfaceSample, NetworkSnapshot } from './types'

vi.mock('./backend', () => ({
  getNetworkSnapshot: vi.fn(),
}))

const mockedNetwork = vi.mocked(getNetworkSnapshot)

function rate(state: 'known' | 'warming_up' = 'known') {
  return {
    rxBytesPerSecond: state === 'known' ? 2048 : null,
    txBytesPerSecond: state === 'known' ? 1024 : null,
    state,
    reason: state === 'known' ? 'aggregate_rate_known' : 'aggregate_rate_warming_up',
  } as const
}

function interfaceSample(): NetworkInterfaceSample {
  return {
    index: 2,
    name: 'enp1s0',
    kind: 'physical',
    kernelKind: null,
    isUp: true,
    carrierUp: true,
    counters: { rxBytes: 4096, txBytes: 2048 },
    rate: rate(),
    transition: 'stable',
  }
}

function snapshot(interfaces: NetworkInterfaceSample[] = []): NetworkSnapshot {
  const warmingUp = interfaces.length === 0
  return {
    schemaVersion: 1,
    snapshotId: '019fe096-aeac-7bc1-8077-6e960dbc5570',
    capturedAtUnixMs: warmingUp ? null : 1_786_154_400_000,
    observedBoottimeMs: warmingUp ? null : 10_000,
    sampleIntervalMs: warmingUp ? null : 1_000,
    lastSuccessAtUnixMs: warmingUp ? null : 1_786_154_400_000,
    freshness: warmingUp ? 'warming_up' : 'fresh',
    retryable: warmingUp,
    systemTraffic: {
      status: warmingUp ? 'degraded' : 'healthy',
      reason: warmingUp ? 'network_snapshot_pending' : 'network_system_traffic_available',
    },
    perApplication: {
      status: 'unsupported',
      reason: 'unprivileged_bpf_permanently_disabled',
    },
    coverage: {
      reportedInterfaces: interfaces.length,
      interfacesWithCounters: interfaces.length,
      includesLoopback: false,
      includesTunnels: false,
      layeredAccounting: 'not_detected',
      reason: warmingUp ? 'coverage_unknown' : 'all_reported_interfaces_have_counters',
    },
    totals: warmingUp ? null : {
      scope: 'inclusive_interfaces',
      allInterfaces: { rxBytes: 4096, txBytes: 2048 },
      physical: { rxBytes: 4096, txBytes: 2048 },
      loopback: { rxBytes: 0, txBytes: 0 },
      tunnel: { rxBytes: 0, txBytes: 0 },
      otherVirtual: { rxBytes: 0, txBytes: 0 },
    },
    aggregateRate: rate(warmingUp ? 'warming_up' : 'known'),
    interfaces,
    applications: [],
  }
}

describe('NetworkView', () => {
  beforeEach(() => {
    window.location.hash = ''
    mockedNetwork.mockReset()
  })

  it('renders the factual first-sample state without invented interfaces or zero rates', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot() })
    const wrapper = mount(NetworkView)
    await flushPromises()

    expect(wrapper.text()).toContain('等待首次网络采样')
    expect(wrapper.text()).toContain('aggregate_rate_warming_up')
    expect(wrapper.text()).toContain('unprivileged_bpf_permanently_disabled')
    expect(wrapper.findAll('.network-table tbody tr')).toHaveLength(0)
    wrapper.unmount()
  })

  it('renders only backend-provided interface counters and rates', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    const wrapper = mount(NetworkView)
    await flushPromises()

    expect(wrapper.text()).toContain('enp1s0')
    expect(wrapper.text()).toContain('2.0 KiB/s')
    expect(wrapper.text()).toContain('1.0 KiB/s')
    expect(wrapper.text()).toContain('1/1')
    expect(wrapper.text()).toContain('fresh')
    expect(wrapper.findAll('.network-table tbody tr')).toHaveLength(1)
    expect(wrapper.get('[data-network-tab="interfaces"]').attributes('aria-controls')).toBe('network-panel-interfaces')
    expect(wrapper.get('.network-workspace').attributes('role')).toBe('tabpanel')
    expect(wrapper.get('.network-workspace').attributes('aria-labelledby')).toBe('network-tab-interfaces')
    wrapper.unmount()
  })

  it('keeps unsupported per-application traffic empty and readable', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="applications"]').trigger('click')

    expect(wrapper.text()).toContain('按应用流量不可用')
    expect(wrapper.text()).toContain('unprivileged_bpf_permanently_disabled')
    expect(wrapper.findAll('.network-table tbody tr')).toHaveLength(0)
    wrapper.unmount()
  })

  it('opens the per-application view directly from the dashboard deep link', async () => {
    window.location.hash = '#/network?tab=applications'
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    const wrapper = mount(NetworkView)
    await flushPromises()

    expect(wrapper.get('[data-network-tab="applications"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.text()).toContain('按应用流量不可用')
    wrapper.unmount()
  })

  it('moves selection and focus across network tabs with arrow, Home, and End keys', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    const wrapper = mount(NetworkView, { attachTo: document.body })
    await flushPromises()

    await wrapper.get('[data-network-tab="interfaces"]').trigger('keydown', { key: 'ArrowRight' })
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(wrapper.get('[data-network-tab="applications"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.get('[data-network-tab="applications"]').attributes('tabindex')).toBe('0')
    expect(document.activeElement).toBe(wrapper.get('[data-network-tab="applications"]').element)

    await wrapper.get('[data-network-tab="applications"]').trigger('keydown', { key: 'Home' })
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(document.activeElement).toBe(wrapper.get('[data-network-tab="interfaces"]').element)

    await wrapper.get('[data-network-tab="interfaces"]').trigger('keydown', { key: 'End' })
    await new Promise((resolve) => requestAnimationFrame(resolve))
    expect(document.activeElement).toBe(wrapper.get('[data-network-tab="applications"]').element)
    wrapper.unmount()
  })

  it('renders typed bridge failure and retries the network request', async () => {
    mockedNetwork.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'appd_socket_unavailable',
        reason: 'appd_socket_unavailable',
        retryable: true,
      },
    })
    const wrapper = mount(NetworkView)
    await flushPromises()

    expect(wrapper.text()).toContain('网络快照不可用')
    expect(wrapper.text()).toContain('appd_socket_unavailable')
    expect(wrapper.get('.network-inspector').text()).toContain('unreachable')
    expect(wrapper.get('.network-inspector').text()).not.toContain('unsupported')
    await wrapper.get('.network-state .network-secondary-button').trigger('click')
    await flushPromises()
    expect(mockedNetwork).toHaveBeenCalledTimes(2)
    wrapper.unmount()
  })

  it('keeps application capability pending until the first snapshot completes', async () => {
    let finishSnapshot!: () => void
    mockedNetwork.mockReturnValue(new Promise((resolve) => {
      finishSnapshot = () => resolve({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    }))
    const wrapper = mount(NetworkView)

    expect(wrapper.get('.network-inspector').text()).toContain('degraded')
    expect(wrapper.get('.network-inspector').text()).toContain('network_snapshot_pending')
    expect(wrapper.get('.network-inspector').text()).not.toContain('unsupported')
    finishSnapshot()
    await flushPromises()
    wrapper.unmount()
  })

  it('keeps the last successful network snapshot when refresh fails', async () => {
    mockedNetwork
      .mockResolvedValueOnce({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'appd_socket_unavailable',
          reason: 'appd_socket_unavailable',
          retryable: true,
        },
      })
    const wrapper = mount(NetworkView)
    await flushPromises()

    await wrapper.get('button.network-refresh').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('enp1s0')
    expect(wrapper.text()).toContain('刷新失败，正在显示上一次成功数据')
    expect(wrapper.get('.network-refresh-error').text()).toContain('appd_socket_unavailable')
    expect(wrapper.find('.network-state.is-error').exists()).toBe(false)
    wrapper.unmount()
  })
})
