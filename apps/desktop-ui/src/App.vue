<script setup lang="ts">
import { computed, KeepAlive, nextTick, ref, watch } from 'vue'
import { RouterLink, RouterView, useRoute } from 'vue-router'
import { Menu, X } from 'lucide-vue-next'

import BackendHealth from './components/BackendHealth.vue'

const route = useRoute()
const mobileNavOpen = ref(false)
const menuButton = ref<HTMLButtonElement | null>(null)

const navigation = [
  { label: '仪表盘', to: '/', name: 'dashboard', index: '01', english: 'STATUS' },
  { label: '应用', to: '/applications', name: 'applications', index: '02', english: 'APPLICATIONS' },
  { label: '网络', to: '/network', name: 'network', index: '03', english: 'NETWORK' },
  { label: '远程连接', to: '/remote', name: 'remote', index: '04', english: 'REMOTE' },
  { label: '传输队列', to: '/transfers', name: 'transfers', index: '05', english: 'TRANSFERS' },
  { label: '备忘录', to: '/memos', name: 'memos', index: '06', english: 'MEMOS' },
  { label: '设置', to: '/settings', name: 'settings', index: '07', english: 'SETTINGS' },
] as const

const activeNavigation = computed(() => (
  navigation.find((item) => item.name === route.name) ?? navigation[0]
))
const dashboardActive = computed(() => route.name === 'dashboard')
const operationsWorkspaceActive = computed(() => !dashboardActive.value)

watch(() => route.fullPath, () => {
  mobileNavOpen.value = false
})

async function closeMobileNav() {
  mobileNavOpen.value = false
  await nextTick()
  menuButton.value?.focus()
}
</script>

<template>
  <div
    class="app-shell"
    :class="[
      dashboardActive ? 'dashboard-route-active' : 'light-route-active',
      { 'mobile-nav-open': mobileNavOpen },
    ]"
  >
    <a class="skip-link" href="#main-content">跳至主工作区</a>

    <header class="topbar">
      <RouterLink class="brand-lockup" to="/" aria-label="本机控制台仪表盘" @click="mobileNavOpen = false">
        <span>ULTRAMARINE</span>
        <span>CONTROL</span>
        <span>FIELD</span>
        <span>+ H</span>
      </RouterLink>

      <nav class="primary-nav" aria-label="应用页面">
        <RouterLink
          v-for="item in navigation"
          :key="item.to"
          class="nav-item"
          :to="item.to"
          :aria-label="item.label"
          @click="mobileNavOpen = false"
        >
          {{ item.label }}
        </RouterLink>
      </nav>

      <div class="topbar-actions">
        <BackendHealth />
        <button
          ref="menuButton"
          class="menu-button"
          type="button"
          :aria-label="mobileNavOpen ? '关闭导航菜单' : '打开导航菜单'"
          :title="mobileNavOpen ? '关闭导航菜单' : '打开导航菜单'"
          :aria-expanded="mobileNavOpen"
          @click="mobileNavOpen = !mobileNavOpen"
        >
          <X v-if="mobileNavOpen" :size="22" aria-hidden="true" />
          <Menu v-else :size="22" aria-hidden="true" />
        </button>
      </div>
    </header>

    <nav v-if="mobileNavOpen" class="mobile-nav" aria-label="移动端应用页面" @keydown.esc.stop="closeMobileNav">
      <RouterLink
        v-for="item in navigation"
        :key="`mobile-${item.to}`"
        class="mobile-nav-item"
        :to="item.to"
        @click="mobileNavOpen = false"
      >
        <span>{{ item.index }}</span>
        <strong>{{ item.label }}</strong>
        <small>{{ item.english }}</small>
      </RouterLink>
    </nav>

    <main id="main-content" class="app-main" tabindex="-1">
      <header v-if="!dashboardActive" class="route-banner" aria-hidden="true">
        <span class="route-index">{{ activeNavigation.index }}</span>
        <div>
          <span class="route-name">{{ activeNavigation.label }}</span>
          <span class="route-english">{{ activeNavigation.english }}</span>
        </div>
      </header>

      <div class="view-region" :class="{ 'is-operations-workspace': operationsWorkspaceActive }">
        <RouterView v-slot="{ Component }">
          <Transition name="route-enter">
            <KeepAlive include="RemoteView">
              <component :is="Component" :key="route.name" />
            </KeepAlive>
          </Transition>
        </RouterView>
      </div>
    </main>
  </div>
</template>
