import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  cancelTransfer,
  enqueueTransfer,
  getBackendHealth,
  getRemoteProfiles,
  listTransfers,
  pickDownloadDestination,
  pickUploadSource,
  resolveTransferConflict,
  retryTransfer,
} from './backend'
import TransfersView from './views/TransfersView.vue'
import type { StoredRemoteProfile, TransferState, TransferTask } from './types'

vi.mock('./backend', () => ({
  cancelTransfer: vi.fn(),
  enqueueTransfer: vi.fn(),
  getBackendHealth: vi.fn(),
  getRemoteProfiles: vi.fn(),
  listTransfers: vi.fn(),
  pickDownloadDestination: vi.fn(),
  pickUploadSource: vi.fn(),
  resolveTransferConflict: vi.fn(),
  retryTransfer: vi.fn(),
}))

const mockedHealth = vi.mocked(getBackendHealth)
const mockedList = vi.mocked(listTransfers)
const mockedProfiles = vi.mocked(getRemoteProfiles)
const mockedCancel = vi.mocked(cancelTransfer)
const mockedRetry = vi.mocked(retryTransfer)
const mockedResolve = vi.mocked(resolveTransferConflict)
const mockedEnqueue = vi.mocked(enqueueTransfer)
const mockedPickUpload = vi.mocked(pickUploadSource)
const mockedPickDownload = vi.mocked(pickDownloadDestination)
const navigationTestState = vi.hoisted(() => ({
  leaveGuard: null as null | (() => boolean),
}))

vi.mock('vue-router', () => ({
  onBeforeRouteLeave: vi.fn((guard: () => boolean) => {
    navigationTestState.leaveGuard = guard
  }),
}))

const profileId = '019fe096-aeac-7bc1-8077-6e960dbc5570'
const taskId = '019fe096-aeac-7bc1-8077-6e960dbc5571'
const ftpProfileId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
const smbProfileId = '019fe096-aeac-7bc1-8077-6e960dbc5581'

function transferTask(state: TransferState): TransferTask {
  return {
    id: taskId,
    source: { kind: 'local', handle: '019fe096-aeac-7bc1-8077-6e960dbc5572' },
    destination: { kind: 'remote', profileId, protocol: 'sftp', path: '/reports/out.txt' },
    direction: 'upload',
    expectedSource: null,
    expectedDestination: null,
    state,
    progress: {
      bytesTransferred: 512,
      totalBytes: 1024,
      bytesPerSecond: 256,
      sampledAtUnixMs: 1_786_154_400_000,
    },
    retryPolicy: { maxAttempts: 3, initialBackoffMs: 1000, maxBackoffMs: 30_000 },
    completedAttempts: 0,
    bandwidthLimit: null,
    conflictPolicy: 'fail',
    features: {
      pause: { status: 'unsupported', reason: 'pause_not_available' },
      resume: { status: 'supported' },
      resumeValidation: 'remote_identity',
    },
    revision: 1,
    createdAtMs: 1_786_154_300_000,
    updatedAtMs: 1_786_154_400_000,
  }
}

function page(tasks: TransferTask[], queryOverrides: Record<string, unknown> = {}, pageOverrides: Record<string, unknown> = {}) {
  return {
    query: { limit: 16, offset: 0, states: [], direction: null, profileId: null, ...queryOverrides },
    tasks,
    hasMore: false,
    nextOffset: null,
    ...pageOverrides,
  }
}

function profile(): StoredRemoteProfile {
  return {
    profile: {
      id: profileId,
      label: '文件服务器',
      protocol: 'sftp',
      endpoint: { host: 'files.local', port: 22 },
      username: 'operator',
      domain: null,
      authentication: { method: 'ssh_agent' },
      trust: { kind: 'ssh_known_hosts', first_use: 'ask_user' },
      options: { protocol: 'sftp', jump_profiles: [] },
    },
    revision: 0,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  }
}

function ftpProfile(): StoredRemoteProfile {
  return {
    profile: {
      id: ftpProfileId,
      label: '旧设备',
      protocol: 'ftp',
      endpoint: { host: 'legacy.local', port: 21 },
      username: null,
      domain: null,
      authentication: { method: 'anonymous' },
      trust: { kind: 'plaintext_acknowledged' },
      options: { protocol: 'ftp', data_connection: 'passive' },
    },
    revision: 0,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  }
}

function smbProfile(): StoredRemoteProfile {
  return {
    profile: {
      id: smbProfileId,
      label: '共享文件',
      protocol: 'smb',
      endpoint: { host: 'nas.local', port: 445 },
      username: null,
      domain: null,
      authentication: { method: 'kerberos' },
      trust: { kind: 'smb_negotiated' },
      options: {
        protocol: 'smb',
        share: 'documents',
        minimum_dialect: 'smb3',
        require_signing: true,
        require_encryption: true,
      },
    },
    revision: 0,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  }
}

describe('TransfersView', () => {
  beforeEach(() => {
    navigationTestState.leaveGuard = null
    window.location.hash = ''
    for (const mock of [
      mockedHealth,
      mockedList,
      mockedProfiles,
      mockedCancel,
      mockedRetry,
      mockedResolve,
      mockedEnqueue,
      mockedPickUpload,
      mockedPickDownload,
    ]) mock.mockReset()
    mockedHealth.mockResolvedValue({ status: 'healthy', capabilityReason: 'transfer_runner_active_public_commands_available' })
    mockedList.mockResolvedValue({ kind: 'page', page: page([]) })
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [profile(), ftpProfile(), smbProfile()], nextAfter: null },
    })
  })

  it('renders a factual empty queue without invented tasks', async () => {
    const wrapper = mount(TransfersView)
    await flushPromises()

    expect(wrapper.text()).toContain('暂无传输任务')
    expect(wrapper.text()).toContain('transfer_queue_empty')
    expect(wrapper.text()).toContain('transfer_runner_active_public_commands_available')
    expect(wrapper.text()).toContain('SFTP / FTP / SMB')
    expect(wrapper.findAll('.transfer-table tbody tr')).toHaveLength(0)
    expect(mockedProfiles).toHaveBeenCalledOnce()
    wrapper.unmount()
  })

  it('auto-refreshes while transfers are active and stops polling when idle', async () => {
    vi.useFakeTimers()
    const running = transferTask({ status: 'running' })
    mockedList.mockResolvedValue({ kind: 'page', page: page([running]) })

    const wrapper = mount(TransfersView)
    await flushPromises()
    const afterMount = mockedList.mock.calls.length
    expect(afterMount).toBeGreaterThanOrEqual(1)

    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(mockedList.mock.calls.length).toBeGreaterThan(afterMount)

    mockedList.mockResolvedValue({
      kind: 'page',
      page: page([
        {
          ...transferTask({ status: 'running' }),
          state: {
            status: 'completed',
            completion: { verification: 'remote_identity', identity: null, completedAtUnixMs: 1_786_154_500_000 },
          },
        },
      ]),
    })
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    const afterCompletion = mockedList.mock.calls.length

    await vi.advanceTimersByTimeAsync(3_000)
    await flushPromises()
    expect(mockedList.mock.calls.length).toBe(afterCompletion)

    wrapper.unmount()
    vi.useRealTimers()
  })

  it('renders returned task facts and performs revision-checked cancel and conflict resolution', async () => {
    const running = transferTask({ status: 'running' })
    const conflict = {
      ...transferTask({ status: 'conflict', conflict: { reason: 'destination_identity_changed', checkpoint: null } }),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5573',
      revision: 4,
    }
    mockedList.mockResolvedValue({ kind: 'page', page: page([running, conflict]) })
    mockedCancel.mockResolvedValue({
      kind: 'mutation',
      result: {
        result: 'updated',
        task: { ...running, state: { status: 'cancelled', checkpoint: null, cancelledAtUnixMs: 1_786_154_500_000 }, revision: 2 },
      },
    })
    mockedResolve.mockResolvedValue({
      kind: 'mutation',
      result: { result: 'updated', task: { ...conflict, state: { status: 'queued' }, revision: 5 } },
    })

    const wrapper = mount(TransfersView)
    await flushPromises()

    expect(wrapper.text()).toContain('/reports/out.txt')
    expect(wrapper.text()).toContain('50.0%')
    expect(wrapper.text()).toContain('destination_identity_changed')

    await wrapper.findAll('.transfer-row-actions button')[0].trigger('click')
    await flushPromises()
    expect(mockedCancel).toHaveBeenCalledWith(taskId, 1)

    await wrapper.findAll('.transfer-row-actions button').find((button) => button.text() === '重命名')?.trigger('click')
    await flushPromises()
    expect(mockedResolve).toHaveBeenCalledWith(conflict.id, 4, 'rename')
    wrapper.unmount()
  })

  it('only offers retry when the failed task has a retry disposition and remaining attempts', async () => {
    const retryable = transferTask({
      status: 'failed',
      failure: { kind: 'transport', operation: 'connect', reason: 'ftp_transport_failed', retry: 'backoff' },
    })
    const exhausted = {
      ...retryable,
      id: '019fe096-aeac-7bc1-8077-6e960dbc5574',
      completedAttempts: retryable.retryPolicy.maxAttempts,
    }
    const cancelled = {
      ...transferTask({ status: 'cancelled', checkpoint: null, cancelledAtUnixMs: 1_786_154_500_000 }),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5575',
    }
    mockedList.mockResolvedValue({ kind: 'page', page: page([retryable, exhausted, cancelled]) })
    mockedRetry.mockResolvedValue({
      kind: 'mutation',
      result: { result: 'updated', task: { ...retryable, state: { status: 'queued' }, revision: 2 } },
    })

    const wrapper = mount(TransfersView)
    await flushPromises()

    const retryButtons = wrapper.findAll('.transfer-row-actions button').filter((button) => button.text() === '重试')
    expect(retryButtons).toHaveLength(1)
    expect(wrapper.text()).toContain('重试次数已用完')
    await retryButtons[0].trigger('click')
    await flushPromises()
    expect(mockedRetry).toHaveBeenCalledWith(retryable.id, retryable.revision)
    wrapper.unmount()
  })

  it('requires confirmation before overwrite conflict resolution', async () => {
    const conflict = {
      ...transferTask({ status: 'conflict', conflict: { reason: 'destination_identity_changed', checkpoint: null } }),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5573',
      revision: 4,
    }
    mockedList.mockResolvedValue({ kind: 'page', page: page([conflict]) })
    mockedResolve.mockResolvedValue({
      kind: 'mutation',
      result: { result: 'updated', task: { ...conflict, state: { status: 'queued' }, revision: 5 } },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(TransfersView)
    await flushPromises()

    const overwrite = wrapper.findAll('.transfer-row-actions button').find((button) => button.text() === '覆盖')!
    await overwrite.trigger('click')
    expect(confirm).toHaveBeenCalledWith('覆盖会替换目标“sftp:/reports/out.txt”。确定继续吗？')
    expect(mockedResolve).not.toHaveBeenCalled()

    await overwrite.trigger('click')
    await flushPromises()
    expect(mockedResolve).toHaveBeenCalledWith(conflict.id, 4, 'overwrite')
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('does not invoke a second mutation while the same task is busy', async () => {
    const running = transferTask({ status: 'running' })
    mockedList.mockResolvedValue({ kind: 'page', page: page([running]) })
    let finishCancel!: () => void
    mockedCancel.mockReturnValue(new Promise((resolve) => {
      finishCancel = () => resolve({
        kind: 'mutation',
        result: {
          result: 'updated',
          task: { ...running, state: { status: 'cancelled', checkpoint: null, cancelledAtUnixMs: 1_786_154_500_000 }, revision: 2 },
        },
      })
    }))
    const wrapper = mount(TransfersView)
    await flushPromises()
    const cancel = wrapper.get('.transfer-row-actions button')

    await cancel.trigger('click')
    await cancel.trigger('click')
    expect(mockedCancel).toHaveBeenCalledOnce()
    finishCancel()
    await flushPromises()
    wrapper.unmount()
  })

  it('does not start a task mutation while a queue refresh is pending', async () => {
    const running = transferTask({ status: 'running' })
    mockedList.mockResolvedValueOnce({ kind: 'page', page: page([running]) })
    const wrapper = mount(TransfersView)
    await flushPromises()
    let finishRefresh!: () => void
    mockedList.mockImplementationOnce(() => new Promise((resolve) => {
      finishRefresh = () => resolve({ kind: 'page', page: page([running]) })
    }))

    await wrapper.findAll('.transfer-toolbar-actions button')[1].trigger('click')
    expect(wrapper.get('.transfer-row-actions button').attributes('disabled')).toBeDefined()
    await wrapper.get('.transfer-row-actions button').trigger('click')
    expect(mockedCancel).not.toHaveBeenCalled()

    finishRefresh()
    await flushPromises()
    expect(wrapper.get('.transfer-row-actions button').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('freezes queue queries during a task mutation but keeps other task mutations independent', async () => {
    const first = transferTask({ status: 'running' })
    const second = { ...transferTask({ status: 'running' }), id: '019fe096-aeac-7bc1-8077-6e960dbc5591' }
    mockedList.mockResolvedValue({
      kind: 'page',
      page: page([first, second], {}, { hasMore: true, nextOffset: 16 }),
    })
    const finish: Array<() => void> = []
    mockedCancel.mockImplementation((id) => new Promise((resolve) => {
      const task = id === first.id ? first : second
      finish.push(() => resolve({
        kind: 'mutation',
        result: {
          result: 'updated',
          task: { ...task, state: { status: 'cancelled', checkpoint: null, cancelledAtUnixMs: 1_786_154_500_000 }, revision: 2 },
        },
      }))
    }))
    const wrapper = mount(TransfersView)
    await flushPromises()
    const actions = wrapper.findAll('.transfer-row-actions button')

    await actions[0].trigger('click')
    expect(wrapper.get('[data-transfer-filter="upload"]').attributes('disabled')).toBeDefined()
    expect(wrapper.findAll('.transfer-toolbar-actions button')[1].attributes('disabled')).toBeDefined()
    expect(wrapper.findAll('.transfer-pagination button')[1].attributes('disabled')).toBeDefined()
    await actions[1].trigger('click')
    expect(mockedCancel).toHaveBeenCalledTimes(2)
    expect(mockedList).toHaveBeenCalledTimes(1)

    finish.forEach((complete) => complete())
    await flushPromises()
    expect(wrapper.get('[data-transfer-filter="upload"]').attributes('disabled')).toBeUndefined()
    expect(wrapper.text()).toContain('已取消')
    wrapper.unmount()
  })

  it('keeps the last successful queue visible when refresh fails', async () => {
    const running = transferTask({ status: 'running' })
    mockedList.mockResolvedValueOnce({ kind: 'page', page: page([running]) })
    const wrapper = mount(TransfersView)
    await flushPromises()
    mockedList.mockResolvedValueOnce({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'transfer_query_unavailable',
        reason: 'transfer_query_unavailable',
        retryable: true,
      },
    })

    await wrapper.findAll('.transfer-toolbar-actions button')[1].trigger('click')
    await flushPromises()

    expect(wrapper.findAll('.transfer-table tbody tr')).toHaveLength(1)
    expect(wrapper.text()).toContain('/reports/out.txt')
    expect(wrapper.text()).toContain('transfer_query_unavailable')
    expect(wrapper.find('.transfer-state.is-error').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps the current queue and pagination history when the next page fails', async () => {
    const first = page([transferTask({ status: 'running' })], {}, { hasMore: true, nextOffset: 16 })
    const secondTask = {
      ...transferTask({ status: 'queued' }),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5578',
      destination: { kind: 'remote' as const, profileId, protocol: 'sftp' as const, path: '/reports/page-two.txt' },
    }
    const second = page([secondTask], { offset: 16 })
    mockedList
      .mockResolvedValueOnce({ kind: 'page', page: first })
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'transfer_query_unavailable',
          reason: 'transfer_query_unavailable',
          retryable: true,
        },
      })
      .mockResolvedValueOnce({ kind: 'page', page: second })

    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.findAll('.transfer-pagination button')[1].trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('/reports/out.txt')
    expect(wrapper.text()).toContain('请求失败，正在显示上一次成功结果')
    expect(wrapper.findAll('.transfer-pagination button')[0].attributes('disabled')).toBeDefined()

    await wrapper.findAll('.transfer-pagination button')[1].trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('/reports/page-two.txt')
    expect(wrapper.findAll('.transfer-pagination button')[0].attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('creates an upload from an opaque local grant without exposing a local path', async () => {
    mockedPickUpload.mockResolvedValue({
      kind: 'picked',
      grant: {
        handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
        purpose: 'upload_source',
        displayName: 'report.txt',
        sizeBytes: 1024,
      },
    })
    mockedEnqueue.mockResolvedValue({ kind: 'task', task: transferTask({ status: 'queued' }) })

    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-primary-button').trigger('click')
    await flushPromises()
    await wrapper.findAll('select')[1].setValue(profileId)
    await wrapper.get('input').setValue('/incoming/report.txt')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(mockedEnqueue).toHaveBeenCalledOnce()
    const draft = mockedEnqueue.mock.calls[0][0]
    expect(draft.direction).toBe('upload')
    expect(draft.source).toEqual({ kind: 'local', handle: '019fe096-aeac-7bc1-8077-6e960dbc5574' })
    expect(draft.destination).toEqual({ kind: 'remote', profileId, path: '/incoming/report.txt' })
    expect(JSON.stringify(draft)).not.toContain('/home/')
    wrapper.unmount()
  })

  it('keeps pagination history when an enqueued task cannot refresh the queue', async () => {
    const first = page([transferTask({ status: 'running' })], {}, { hasMore: true, nextOffset: 16 })
    const secondTask = {
      ...transferTask({ status: 'queued' }),
      id: '019fe096-aeac-7bc1-8077-6e960dbc5588',
      destination: { kind: 'remote' as const, profileId, protocol: 'sftp' as const, path: '/reports/page-two.txt' },
    }
    mockedList
      .mockResolvedValueOnce({ kind: 'page', page: first })
      .mockResolvedValueOnce({ kind: 'page', page: page([secondTask], { offset: 16 }) })
      .mockResolvedValueOnce({
        kind: 'error',
        error: { kind: 'transport', code: 'transfer_refresh_failed', reason: 'transfer_refresh_failed', retryable: true },
      })
      .mockResolvedValueOnce({ kind: 'page', page: first })
    mockedPickUpload.mockResolvedValue({
      kind: 'picked',
      grant: {
        handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
        purpose: 'upload_source',
        displayName: 'report.txt',
        sizeBytes: 1024,
      },
    })
    mockedEnqueue.mockResolvedValue({ kind: 'task', task: transferTask({ status: 'queued' }) })
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.findAll('.transfer-pagination button')[1].trigger('click')
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.findAll('select')[1].setValue(profileId)
    await wrapper.get('input').setValue('/incoming/report.txt')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(wrapper.text()).toContain('transfer_refresh_failed')
    expect(wrapper.text()).toContain('/reports/page-two.txt')
    expect(wrapper.findAll('.transfer-pagination button')[0].attributes('disabled')).toBeUndefined()
    await wrapper.findAll('.transfer-pagination button')[0].trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('/reports/out.txt')
    wrapper.unmount()
  })

  it('locks the transfer form while the system picker is pending', async () => {
    let finishPick: (() => void) | null = null
    mockedPickUpload.mockImplementation(() => new Promise((resolve) => {
      finishPick = () => resolve({
        kind: 'picked',
        grant: {
          handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
          purpose: 'upload_source',
          displayName: 'report.txt',
          sizeBytes: 1024,
        },
      })
    }))
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('.transfer-local-picker button').trigger('click')

    expect(wrapper.findAll('select')[0].attributes('disabled')).toBeDefined()
    expect(wrapper.findAll('select')[1].attributes('disabled')).toBeDefined()
    expect(wrapper.get('input').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[aria-label="关闭新建传输"]').attributes('disabled')).toBeDefined()

    finishPick?.()
    await flushPromises()
    expect(wrapper.findAll('select')[0].attributes('disabled')).toBeUndefined()
    expect(wrapper.text()).toContain('report.txt')
    wrapper.unmount()
  })

  it('preserves the existing local grant on picker cancellation and updates only an auto-filled upload name', async () => {
    mockedPickUpload
      .mockResolvedValueOnce({
        kind: 'picked',
        grant: {
          handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
          purpose: 'upload_source',
          displayName: 'report.txt',
          sizeBytes: 1024,
        },
      })
      .mockResolvedValueOnce({
        kind: 'picked',
        grant: {
          handle: '019fe096-aeac-7bc1-8077-6e960dbc5575',
          purpose: 'upload_source',
          displayName: 'final.txt',
          sizeBytes: 2048,
        },
      })
      .mockResolvedValueOnce({ kind: 'picked', grant: null })
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.get('input').setValue('/incoming/')

    const picker = wrapper.get('.transfer-local-picker button')
    await picker.trigger('click')
    await flushPromises()
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('/incoming/report.txt')
    expect(wrapper.text()).toContain('report.txt')

    await picker.trigger('click')
    await flushPromises()
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('/incoming/final.txt')
    expect(wrapper.text()).toContain('final.txt')

    await picker.trigger('click')
    await flushPromises()
    expect(wrapper.text()).not.toContain('portal_selection_cancelled')
    expect(wrapper.text()).toContain('final.txt')
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('/incoming/final.txt')
    wrapper.unmount()
  })

  it('renders unreadable local files as an explicit permission error', async () => {
    mockedPickUpload.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'daemon',
        code: 'transfer_local_handle_unavailable',
        reason: 'local_file_permission_denied',
        retryable: false,
      },
    })
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('无权限：无法读取所选本机文件')
    expect(wrapper.text()).toContain('local_file_permission_denied')
    wrapper.unmount()
  })

  it('does not replace a remote path the user edited after picker auto-fill', async () => {
    mockedPickUpload
      .mockResolvedValueOnce({
        kind: 'picked',
        grant: {
          handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
          purpose: 'upload_source',
          displayName: 'report.txt',
          sizeBytes: 1024,
        },
      })
      .mockResolvedValueOnce({
        kind: 'picked',
        grant: {
          handle: '019fe096-aeac-7bc1-8077-6e960dbc5575',
          purpose: 'upload_source',
          displayName: 'final.txt',
          sizeBytes: 2048,
        },
      })
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    const path = wrapper.get('input')
    await path.setValue('/incoming/')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()
    await path.setValue('/custom/kept-name.txt')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()

    expect((path.element as HTMLInputElement).value).toBe('/custom/kept-name.txt')
    expect(wrapper.text()).toContain('final.txt')
    wrapper.unmount()
  })

  it('requires confirmation before discarding a transfer draft and clears it after confirmation', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.get('input').setValue('/pending/report.txt')

    await wrapper.get('[aria-label="关闭新建传输"]').trigger('click')
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(wrapper.find('.transfer-create-form').exists()).toBe(true)
    expect(wrapper.get<HTMLInputElement>('input').element.value).toBe('/pending/report.txt')

    await wrapper.get('[aria-label="关闭新建传输"]').trigger('click')
    expect(confirm).toHaveBeenCalledTimes(2)
    expect(wrapper.find('.transfer-create-form').exists()).toBe(false)
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    expect(wrapper.get<HTMLInputElement>('input').element.value).toBe('')
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('protects a transfer draft on route and window leave', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.get('input').setValue('/pending/report.txt')

    expect(navigationTestState.leaveGuard?.()).toBe(false)
    const beforeUnload = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(beforeUnload)
    expect(beforeUnload.defaultPrevented).toBe(true)
    expect(navigationTestState.leaveGuard?.()).toBe(true)
    expect(confirm).toHaveBeenCalledTimes(2)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('requires confirmation before changing direction clears an opaque local grant', async () => {
    mockedPickUpload.mockResolvedValue({
      kind: 'picked',
      grant: {
        handle: '019fe096-aeac-7bc1-8077-6e960dbc5574',
        purpose: 'upload_source',
        displayName: 'report.txt',
        sizeBytes: 1024,
      },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-toolbar-actions .transfer-primary-button').trigger('click')
    await wrapper.get('.transfer-local-picker button').trigger('click')
    await flushPromises()

    const direction = wrapper.findAll('select')[0]
    await direction.setValue('download')
    expect((direction.element as HTMLSelectElement).value).toBe('upload')
    expect(wrapper.text()).toContain('report.txt')

    await direction.setValue('download')
    expect((direction.element as HTMLSelectElement).value).toBe('download')
    expect(wrapper.text()).not.toContain('report.txt')
    expect(confirm).toHaveBeenCalledTimes(2)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('offers FTP and SMB profiles to the same bounded transfer workflow', async () => {
    const wrapper = mount(TransfersView)
    await flushPromises()
    await wrapper.get('.transfer-primary-button').trigger('click')
    await flushPromises()

    const profileOptions = wrapper.findAll('select')[1].findAll('option').map((option) => option.text())
    expect(profileOptions).toEqual([
      'SFTP · 文件服务器 · files.local:22',
      'FTP · 旧设备 · legacy.local:21',
      'SMB · 共享文件 · nas.local:445',
    ])
    wrapper.unmount()
  })

  it('opens a transfer deep link with its direction, profile, and remote path prefilled', async () => {
    window.location.hash = `#/transfers?direction=download&profile=${smbProfileId}&path=%2Freports%2Ffinal.txt`
    const wrapper = mount(TransfersView)
    await flushPromises()

    expect(wrapper.find('.transfer-create-form').exists()).toBe(true)
    expect((wrapper.findAll('select')[0].element as HTMLSelectElement).value).toBe('download')
    expect((wrapper.findAll('select')[1].element as HTMLSelectElement).value).toBe(smbProfileId)
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('/reports/final.txt')
    wrapper.unmount()
  })

  it('renders typed bridge failure and retries the queue request', async () => {
    mockedList
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'transport',
          code: 'desktop_bridge_unavailable',
          reason: 'desktop_bridge_unavailable',
          retryable: true,
        },
      })
      .mockResolvedValueOnce({ kind: 'page', page: page([]) })

    const wrapper = mount(TransfersView)
    await flushPromises()

    expect(wrapper.text()).toContain('传输队列不可用')
    expect(wrapper.text()).toContain('desktop_bridge_unavailable')
    expect(wrapper.get('.transfer-primary-button').attributes('disabled')).toBeDefined()
    await wrapper.get('.transfer-state .transfer-secondary-button').trigger('click')
    await flushPromises()
    expect(mockedList).toHaveBeenCalledTimes(2)
    wrapper.unmount()
  })

  it('supports arrow-key navigation between direction filters', async () => {
    const wrapper = mount(TransfersView, { attachTo: document.body })
    await flushPromises()

    await wrapper.get('[data-transfer-filter="all"]').trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    await new Promise((resolve) => requestAnimationFrame(resolve))

    expect(wrapper.get('[data-transfer-filter="upload"]').attributes('aria-selected')).toBe('true')
    expect(document.activeElement).toBe(wrapper.get('[data-transfer-filter="upload"]').element)
    expect(mockedList).toHaveBeenLastCalledWith(expect.objectContaining({ direction: 'upload', offset: 0 }))
    wrapper.unmount()
  })
})
