import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import BackendHealth from './components/BackendHealth.vue'
import { getBackendHealth } from './backend'

vi.mock('./backend', () => ({
  getBackendHealth: vi.fn(),
}))

const mockedGetBackendHealth = vi.mocked(getBackendHealth)

describe('BackendHealth', () => {
  beforeEach(() => {
    mockedGetBackendHealth.mockReset()
  })

  afterEach(() => vi.useRealTimers())

  it('renders a healthy bridge and its capability reason', async () => {
    mockedGetBackendHealth.mockResolvedValue({
      status: 'healthy',
      capabilityReason: 'appd_ready',
    })

    const wrapper = mount(BackendHealth)
    await flushPromises()

    expect(wrapper.text()).toContain('healthy')
    expect(wrapper.text()).toContain('appd_ready')
    expect(wrapper.find('button').attributes('aria-label')).toBe('重新检查桌面桥接')
  })

  it('renders unsupported instead of inventing browser backend data', async () => {
    mockedGetBackendHealth.mockResolvedValue({
      status: 'unsupported',
      capabilityReason: 'desktop_bridge_unavailable',
    })

    const wrapper = mount(BackendHealth)
    await flushPromises()

    expect(wrapper.text()).toContain('unsupported')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
    expect(wrapper.text()).not.toContain('healthy')
  })

  it('deduplicates refreshes and rechecks bridge health every ten seconds', async () => {
    vi.useFakeTimers()
    let finishHealth!: () => void
    mockedGetBackendHealth.mockReturnValue(new Promise((resolve) => {
      finishHealth = () => resolve({ status: 'healthy', capabilityReason: 'appd_ready' })
    }))
    const wrapper = mount(BackendHealth)
    await flushPromises()

    await (wrapper.vm as unknown as { refresh: () => Promise<void> }).refresh()
    expect(mockedGetBackendHealth).toHaveBeenCalledOnce()
    finishHealth()
    await flushPromises()
    mockedGetBackendHealth.mockResolvedValue({ status: 'degraded', capabilityReason: 'appd_recovering' })
    await vi.advanceTimersByTimeAsync(10_000)
    await flushPromises()

    expect(mockedGetBackendHealth).toHaveBeenCalledTimes(2)
    expect(wrapper.text()).toContain('appd_recovering')
    wrapper.unmount()
  })

  it('stops automatic checks and ignores a late result after unmount', async () => {
    vi.useFakeTimers()
    let finishHealth!: () => void
    mockedGetBackendHealth.mockReturnValue(new Promise((resolve) => {
      finishHealth = () => resolve({ status: 'healthy', capabilityReason: 'late_appd_ready' })
    }))
    const wrapper = mount(BackendHealth)
    await flushPromises()
    wrapper.unmount()
    finishHealth()
    await flushPromises()
    await vi.advanceTimersByTimeAsync(20_000)

    expect(mockedGetBackendHealth).toHaveBeenCalledOnce()
  })
})
