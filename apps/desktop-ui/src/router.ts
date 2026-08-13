import { createMemoryHistory, createRouter, createWebHashHistory } from 'vue-router'

import ApplicationsView from './views/ApplicationsView.vue'
import DashboardView from './views/DashboardView.vue'
import MemosView from './views/MemosView.vue'
import NetworkView from './views/NetworkView.vue'
import RemoteView from './views/RemoteView.vue'
import SettingsView from './views/SettingsView.vue'
import TransfersView from './views/TransfersView.vue'

export const routes = [
  { path: '/', name: 'dashboard', component: DashboardView, meta: { label: '仪表盘' } },
  { path: '/applications', name: 'applications', component: ApplicationsView, meta: { label: '应用' } },
  { path: '/network', name: 'network', component: NetworkView, meta: { label: '网络' } },
  { path: '/remote', name: 'remote', component: RemoteView, meta: { label: '远程连接' } },
  { path: '/transfers', name: 'transfers', component: TransfersView, meta: { label: '传输队列' } },
  { path: '/memos', name: 'memos', component: MemosView, meta: { label: '备忘录' } },
  { path: '/settings', name: 'settings', component: SettingsView, meta: { label: '设置' } },
]

export function createAppRouter(history = createWebHashHistory()) {
  return createRouter({ history, routes })
}

export const router = createAppRouter()

export { createMemoryHistory }
