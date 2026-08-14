import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  cancelSpeedTest,
  getBackendCapabilityReport,
  getNetworkSnapshot,
  runSpeedTestBasic,
  runSpeedTestDeep,
} from './backend'
import NetworkView from './views/NetworkView.vue'
import type {
  BackendCapability,
  NetworkInterfaceSample,
  NetworkSnapshot,
  SpeedTestBasicEnd,
} from './types'

vi.mock('./backend', () => ({
  getNetworkSnapshot: vi.fn(),
  getBackendCapabilityReport: vi.fn(),
  runSpeedTestBasic: vi.fn(),
  cancelSpeedTest: vi.fn(),
  runSpeedTestDeep: vi.fn(),
}))

const mockedNetwork = vi.mocked(getNetworkSnapshot)
const mockedCapabilities = vi.mocked(getBackendCapabilityReport)
const mockedBasic = vi.mocked(runSpeedTestBasic)
const mockedCancel = vi.mocked(cancelSpeedTest)
const mockedDeep = vi.mocked(runSpeedTestDeep)

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
    expect(wrapper.get('.network-table').classes()).toContain('interface-table')
    expect(wrapper.findAll('.interface-table colgroup col')).toHaveLength(6)
    expect(wrapper.findAll('.interface-table .rate-column')).toHaveLength(2)
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
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
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
    expect(document.activeElement).toBe(wrapper.get('[data-network-tab="speedtest"]').element)
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

function capability(id: string, status: BackendCapability['status'], reason: string): BackendCapability {
  return { id, status, reason }
}

function speedtestCapabilities() {
  return {
    kind: 'report' as const,
    report: {
      daemonVersion: '0.1.0',
      health: { status: 'healthy' as const, capabilityReason: 'all_requested_capabilities_available' },
      capabilities: [
        capability('network.speedtest.v1', 'healthy', 'curl_available'),
        capability('network.deeptest.v1', 'healthy', 'iperf3_available'),
      ],
    },
  }
}

function basicEnd(): SpeedTestBasicEnd {
  return {
    schemaVersion: 1,
    startedAtUnixMs: 1_000,
    endedAtUnixMs: 2_000,
    stages: [
      {
        stage: 'latency',
        payload: {
          targets: [
            {
              host: 'github.com',
              probes: [{ connectMs: 1, ttfbMs: 1697, httpCode: 200, error: null }],
              avgTtfbMs: 1697,
            },
            {
              host: 'bilibili.com',
              probes: [{ connectMs: 1, ttfbMs: 120, httpCode: 200, error: null }],
              avgTtfbMs: 120,
            },
          ],
        },
      },
      {
        stage: 'bandwidth',
        payload: {
          measurements: [
            {
              kind: 'international',
              label: '国际线路',
              source: 'speed.cloudflare.com',
              downloadBitsPerSecond: 32_900_000,
              uploadBitsPerSecond: 16_600_000,
              httpCode: 200,
              error: null,
            },
            {
              kind: 'domestic',
              label: '阿里云',
              source: 'mirrors.aliyun.com',
              downloadBitsPerSecond: 0,
              uploadBitsPerSecond: null,
              httpCode: 302,
              error: 'curl_exit_47_too_many_redirects',
            },
          ],
        },
      },
      {
        stage: 'ip_purity',
        payload: {
          purity: {
            source: 'ip-api.com + ipok.io',
            ip: '38.92.26.68',
            country: '美国',
            region: '犹他州',
            city: 'Draper',
            isp: 'FiberState, LLC',
            org: 'Fiberstate LLC',
            asn: 'AS26042',
            asname: 'FIBERSTATE',
            proxy: false,
            hosting: false,
            mobile: false,
            riskScore: 30,
            ipType: 'hosting',
            signals: ['hosting'],
            riskSources: [
              { source: 'ip-api', risk: 10, weight: 0.5 },
              { source: 'Scamalytics', risk: 30, weight: 0.9 },
            ],
            blocklistChecked: 5,
            blocklistListed: [],
            riskError: null,
            error: null,
          },
        },
      },
    ],
    cancelled: false,
    error: null,
  }
}

describe('NetworkView speedtest tab', () => {
  beforeEach(() => {
    window.location.hash = ''
    mockedNetwork.mockReset()
    mockedCapabilities.mockReset()
    mockedBasic.mockReset()
    mockedCancel.mockReset()
    mockedDeep.mockReset()
  })

  it('shows speedtest capabilities and the idle empty state', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('[data-network-tab="speedtest"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.text()).toContain('基础测速')
    expect(wrapper.text()).toContain('尚未测速')
    expect(wrapper.text()).toContain('curl_available')
    expect(wrapper.text()).toContain('iperf3_available')
    expect(wrapper.get('.speed-start').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('disables the start button when the speedtest capability is unsupported', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue({
      kind: 'report',
      report: {
        daemonVersion: '0.1.0',
        health: { status: 'degraded', capabilityReason: 'appd_online_with_unavailable_capabilities' },
        capabilities: [
          capability('network.speedtest.v1', 'unsupported', 'curl_missing'),
          capability('network.deeptest.v1', 'degraded', 'iperf3_missing'),
        ],
      },
    })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('.speed-start').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('curl_missing')
    wrapper.unmount()
  })

  it('runs the basic test and renders latency, bandwidth and IP purity facts', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    mockedBasic.mockResolvedValue({ kind: 'end', end: basicEnd() })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.get('.speed-start').trigger('click')
    await flushPromises()

    expect(mockedBasic).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('github.com')
    expect(wrapper.text()).toContain('1697')
    expect(wrapper.text()).toContain('bilibili.com')
    expect(wrapper.text()).toContain('正常')
    expect(wrapper.text()).toContain('32.9')
    expect(wrapper.text()).toContain('16.6')
    expect(wrapper.text()).toContain('curl_exit_47_too_many_redirects')
    expect(wrapper.text()).toContain('上次测速')
    wrapper.unmount()
  })

  it('loads latency and bandwidth sections progressively as stage frames arrive', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    const end = basicEnd()
    mockedBasic.mockImplementation((_stages, onStage) => {
      onStage(end.stages[0])
      return Promise.resolve({ kind: 'end', end })
    })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.get('.speed-start').trigger('click')
    await flushPromises()

    // latency section is rendered as soon as its stage frame arrives
    expect(wrapper.text()).toContain('github.com')
    expect(mockedBasic).toHaveBeenCalledWith(['latency', 'bandwidth'], expect.any(Function))
    wrapper.unmount()
  })

  it('detects IP purity in its own tab with risk score and derived human/bot share', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    const end = basicEnd()
    mockedBasic.mockImplementation((stages, onStage) => {
      onStage(end.stages[2])
      return Promise.resolve({ kind: 'end', end })
    })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.findAll('.speed-segment')[2].trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('尚未检测')
    await wrapper.get('.speed-start').trigger('click')
    await flushPromises()

    expect(mockedBasic).toHaveBeenCalledWith(['ip_purity'], expect.any(Function))
    expect(wrapper.text()).toContain('出口 IP')
    expect(wrapper.text()).toContain('38.92.26.68')
    expect(wrapper.text()).toContain('风险值')
    expect(wrapper.text()).toContain('30/100')
    expect(wrapper.text()).toContain('中风险')
    expect(wrapper.text()).toContain('真人 70% · 机器人 30%')
    expect(wrapper.text()).toContain('Scamalytics')
    expect(wrapper.text()).toContain('未标记')
    wrapper.unmount()
  })

  it('cancels a running basic test', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    mockedBasic.mockReturnValue(new Promise(() => {}))
    mockedCancel.mockResolvedValue({ kind: 'cancelled', result: { cancelled: true, reason: 'cancellation_requested' } })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.get('.speed-start').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('正在测量')
    const cancelButton = wrapper.findAll('.speed-secondary').find((button) => button.text().includes('取消'))
    expect(cancelButton).toBeDefined()
    await cancelButton!.trigger('click')
    await flushPromises()
    expect(mockedCancel).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('reports a typed bridge failure from the basic test', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    mockedBasic.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'speedtest_basic_unreachable',
        reason: 'appd_socket_unavailable',
        retryable: true,
      },
    })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.get('.speed-start').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('测速失败')
    expect(wrapper.text()).toContain('appd_socket_unavailable')
    wrapper.unmount()
  })

  it('runs iperf3, wifi scan and linssid commands from the deep panel', async () => {
    mockedNetwork.mockResolvedValue({ kind: 'snapshot', snapshot: snapshot([interfaceSample()]) })
    mockedCapabilities.mockResolvedValue(speedtestCapabilities())
    mockedDeep.mockImplementation((command) => {
      if (command.command === 'iperf3_start') {
        return Promise.resolve({
          kind: 'output',
          output: {
            type: 'iperf3',
            payload: {
              server: '10.0.0.8',
              port: 5201,
              direction: 'upload',
              durationSecs: 10,
              parallel: 1,
              startedAtUnixMs: 1,
              endedAtUnixMs: 2,
              downloadBitsPerSecond: null,
              uploadBitsPerSecond: 95_000_000,
              retransmits: 2,
              jitterMs: null,
              error: null,
            },
          },
        })
      }
      if (command.command === 'wifi_scan') {
        return Promise.resolve({
          kind: 'output',
          output: {
            type: 'wifi_scan',
            payload: {
              scannedAtUnixMs: 1_000,
              source: 'nmcli',
              networks: [
                { ssid: 'Rhino-5G', signalPercent: 100, channel: 36, band: '5 GHz', security: 'WPA2 WPA3' },
              ],
              error: null,
            },
          },
        })
      }
      return Promise.resolve({
        kind: 'output',
        output: {
          type: 'linssid',
          payload: { launched: true, executable: '/usr/sbin/linssid', reason: 'launched_via_pkexec' },
        },
      })
    })
    const wrapper = mount(NetworkView)
    await flushPromises()
    await wrapper.get('[data-network-tab="speedtest"]').trigger('click')
    await flushPromises()
    await wrapper.findAll('.speed-segment')[1].trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('尚未运行测速')
    await wrapper.get('.iperf-form').trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('95.0')
    expect(wrapper.text()).toContain('10.0.0.8:5201')

    const scanButton = wrapper.findAll('.speed-secondary').find((button) => button.text().includes('扫描'))
    expect(scanButton).toBeDefined()
    await scanButton!.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Rhino-5G')
    expect(wrapper.text()).toContain('WPA2 WPA3')

    const linssidButton = wrapper.findAll('.speed-secondary').find((button) => button.text().includes('启动 LinSSID'))
    expect(linssidButton).toBeDefined()
    await linssidButton!.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('launched_via_pkexec')
    wrapper.unmount()
  })
})
