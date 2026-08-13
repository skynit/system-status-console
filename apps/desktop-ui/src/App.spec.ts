import { mount, flushPromises } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import App from './App.vue'
import { createAppRouter, createMemoryHistory } from './router'

describe('AppShell', () => {
  it('exposes all seven routes without fabricated domain data', async () => {
    const router = createAppRouter(createMemoryHistory())
    await router.push('/')
    await router.isReady()

    const wrapper = mount(App, {
      global: { plugins: [router] },
    })
    await flushPromises()

    expect(wrapper.get('.skip-link').attributes('href')).toBe('#main-content')
    expect(wrapper.get('.app-main').attributes('tabindex')).toBe('-1')
    expect(wrapper.findAll('.nav-item')).toHaveLength(7)
    expect(wrapper.text()).toContain('仪表盘')
    expect(wrapper.text()).toContain('应用')
    expect(wrapper.text()).toContain('网络')
    expect(wrapper.text()).toContain('远程连接')
    expect(wrapper.text()).toContain('传输队列')
    expect(wrapper.text()).toContain('备忘录')
    expect(wrapper.text()).toContain('设置')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
    expect(wrapper.text()).toContain('能力目录不可用')
    expect(wrapper.text()).not.toContain('not_implemented')
    expect(wrapper.text()).not.toMatch(/\b(?:42|73|128)\s*(?:%|MB|GB)\b/)

    await wrapper.get('.menu-button').trigger('click')
    expect(wrapper.get('.menu-button').attributes('aria-expanded')).toBe('true')
    expect(wrapper.findAll('.mobile-nav-item')).toHaveLength(7)
    await wrapper.get('.mobile-nav').trigger('keydown', { key: 'Escape' })
    await flushPromises()
    expect(wrapper.get('.menu-button').attributes('aria-expanded')).toBe('false')
    expect(wrapper.find('.mobile-nav').exists()).toBe(false)

    await router.push('/memos')
    await flushPromises()
    expect(wrapper.find('h1').text()).toBe('备忘录')
    expect(wrapper.text()).toContain('unsupported')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
  })

  it('switches the workspace content together with primary navigation', async () => {
    const router = createAppRouter(createMemoryHistory())
    await router.push('/applications')
    await router.isReady()

    const wrapper = mount(App, {
      global: { plugins: [router] },
    })
    await flushPromises()

    expect(wrapper.find('.applications-console').exists()).toBe(true)
    expect(wrapper.find('.network-console').exists()).toBe(false)

    await wrapper.get('.primary-nav a[href="/network"]').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('network')
    expect(wrapper.find('.applications-console').exists()).toBe(false)
    expect(wrapper.find('.network-console').exists()).toBe(true)
  })

  it('keeps the remote workspace instance across a transfer queue round trip', async () => {
    const router = createAppRouter(createMemoryHistory())
    await router.push('/remote')
    await router.isReady()

    const wrapper = mount(App, {
      global: { plugins: [router] },
    })
    await flushPromises()
    const remoteWorkspace = wrapper.get('.remote-console').element

    await router.push('/transfers')
    await flushPromises()
    expect(wrapper.find('.remote-console').exists()).toBe(false)

    await router.push('/remote')
    await flushPromises()
    expect(wrapper.get('.remote-console').element).toBe(remoteWorkspace)
  })

  it('keeps primary navigation active for a dashboard deep link with query state', async () => {
    const router = createAppRouter(createMemoryHistory())
    await router.push('/network?tab=applications')
    await router.isReady()
    const wrapper = mount(App, {
      global: { plugins: [router] },
    })
    await flushPromises()

    const networkLink = wrapper.get('.primary-nav a[href="/network"]')
    expect(networkLink.classes()).toContain('router-link-exact-active')
  })
})
