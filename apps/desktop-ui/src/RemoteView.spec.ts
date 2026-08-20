import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  closeRemoteTerminal,
  connectRemoteSession,
  createRemoteDirectory,
  deleteRemoteEntry,
  deleteRemoteProfile,
  deleteRemoteSecret,
  disconnectRemoteSession,
  getRemoteAdapterCatalog,
  getRemoteProfiles,
  listRemoteDirectory,
  openRemoteTerminal,
  pollRemoteTerminal,
  readRemoteTerminal,
  renameRemoteEntry,
  resizeRemoteTerminal,
  storeRemoteSecret,
  streamRemoteTerminal,
  upsertRemoteProfile,
  writeRemoteTerminal,
} from './backend'
import RemoteView from './views/RemoteView.vue'
import type {
  RemoteAdapterDescriptor,
  RemoteOperationCapability,
  RemoteProtocol,
  StoredRemoteProfile,
} from './types'

const terminalTestState = vi.hoisted(() => ({
  terminals: [] as any[],
  fitAddons: [] as any[],
  resizeObservers: [] as any[],
}))
const navigationTestState = vi.hoisted(() => ({
  leaveGuard: null as null | (() => boolean),
}))

vi.mock('vue-router', () => ({
  onBeforeRouteLeave: vi.fn((guard: () => boolean) => {
    navigationTestState.leaveGuard = guard
  }),
}))

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80
    rows = 24
    write = vi.fn()
    focus = vi.fn()
    open = vi.fn()
    loadAddon = vi.fn()
    dispose = vi.fn()
    inputDispose = vi.fn()
    dataHandler: ((data: string) => void) | null = null
    options: Record<string, unknown>

    constructor(options: Record<string, unknown> = {}) {
      terminalTestState.terminals.push(this)
      this.options = options
    }

    onData(handler: (data: string) => void) {
      this.dataHandler = handler
      return { dispose: this.inputDispose }
    }
  },
}))

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn()

    constructor() {
      terminalTestState.fitAddons.push(this)
    }
  },
}))

vi.mock('./backend', () => ({
  closeRemoteTerminal: vi.fn(),
  connectRemoteSession: vi.fn(),
  createRemoteDirectory: vi.fn(),
  deleteRemoteEntry: vi.fn(),
  deleteRemoteProfile: vi.fn(),
  deleteRemoteSecret: vi.fn(),
  disconnectRemoteSession: vi.fn(),
  getRemoteAdapterCatalog: vi.fn(),
  getRemoteProfiles: vi.fn(),
  listRemoteDirectory: vi.fn(),
  openRemoteTerminal: vi.fn(),
  pollRemoteTerminal: vi.fn(),
  readRemoteTerminal: vi.fn(),
  renameRemoteEntry: vi.fn(),
  resizeRemoteTerminal: vi.fn(),
  storeRemoteSecret: vi.fn(),
  streamRemoteTerminal: vi.fn(),
  upsertRemoteProfile: vi.fn(),
  writeRemoteTerminal: vi.fn(),
}))

const mockedCatalog = vi.mocked(getRemoteAdapterCatalog)
const mockedProfiles = vi.mocked(getRemoteProfiles)
const mockedConnect = vi.mocked(connectRemoteSession)
const mockedCreateDirectory = vi.mocked(createRemoteDirectory)
const mockedDeleteEntry = vi.mocked(deleteRemoteEntry)
const mockedDeleteProfile = vi.mocked(deleteRemoteProfile)
const mockedDeleteSecret = vi.mocked(deleteRemoteSecret)
const mockedList = vi.mocked(listRemoteDirectory)
const mockedDisconnect = vi.mocked(disconnectRemoteSession)
const mockedOpenTerminal = vi.mocked(openRemoteTerminal)
const mockedCloseTerminal = vi.mocked(closeRemoteTerminal)
const mockedPollTerminal = vi.mocked(pollRemoteTerminal)
const mockedReadTerminal = vi.mocked(readRemoteTerminal)
const mockedRenameEntry = vi.mocked(renameRemoteEntry)
const mockedResizeTerminal = vi.mocked(resizeRemoteTerminal)
const mockedStoreSecret = vi.mocked(storeRemoteSecret)
const mockedStreamTerminal = vi.mocked(streamRemoteTerminal)
const mockedUpsert = vi.mocked(upsertRemoteProfile)
const mockedWriteTerminal = vi.mocked(writeRemoteTerminal)

const operations = [
  'list',
  'stat',
  'read',
  'write',
  'create_directory',
  'rename',
  'delete',
  'resume_read',
  'resume_write',
  'atomic_rename',
  'set_permissions',
] as const

function capabilities(supported: boolean): RemoteOperationCapability[] {
  return operations.map((operation) => ({
    operation,
    status: supported ? 'supported' : 'unsupported',
    reason: supported ? null : 'operation_not_available',
  }))
}

function adapter(protocol: RemoteProtocol): RemoteAdapterDescriptor {
  const fileProtocol = protocol === 'sftp' || protocol === 'ftp' || protocol === 'ftps_explicit' || protocol === 'smb'
  const fileCapabilities = capabilities(fileProtocol)
  if (protocol === 'smb') {
    fileCapabilities[fileCapabilities.length - 1] = {
      operation: 'set_permissions',
      status: 'unsupported',
      reason: 'smb_set_permissions_not_implemented',
    }
  }
  return {
    protocol,
    availability: protocol === 'ftp'
      ? { status: 'degraded', capabilityReason: 'plain_ftp_explicitly_enabled' }
      : protocol === 'smb'
      ? { status: 'healthy', capabilityReason: 'available' }
      : { status: 'healthy', capabilityReason: 'available' },
    terminal: protocol === 'ssh'
      ? { status: 'supported', reason: null }
      : { status: 'unsupported', reason: 'terminal_not_applicable' },
    fileOperations: fileCapabilities,
  }
}

function storedProfile(protocol: 'ssh' | 'sftp', id: string, label: string): StoredRemoteProfile {
  return {
    profile: {
      id,
      label,
      protocol,
      endpoint: { host: `${protocol}.local`, port: 22 },
      username: 'operator',
      domain: null,
      authentication: { method: 'ssh_agent' },
      trust: { kind: 'ssh_known_hosts', first_use: 'ask_user' },
      options: protocol === 'ssh'
        ? { protocol: 'ssh', jump_profiles: [], agent_forwarding: false }
        : { protocol: 'sftp', jump_profiles: [] },
    },
    revision: 0,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  }
}

const sshProfile = storedProfile('ssh', '019fe096-aeac-7bc1-8077-6e960dbc5570', '生产终端')
const backupSshProfile = storedProfile('ssh', '019fe096-aeac-7bc1-8077-6e960dbc5573', '备份终端')
backupSshProfile.profile.endpoint.host = 'backup.local'
const sftpProfile = storedProfile('sftp', '019fe096-aeac-7bc1-8077-6e960dbc5571', '文件服务器')
const backupSftpProfile = storedProfile('sftp', '019fe096-aeac-7bc1-8077-6e960dbc5574', '备份文件')
backupSftpProfile.profile.endpoint.host = 'backup-files.local'
const smbProfile: StoredRemoteProfile = {
  profile: {
    id: '019fe096-aeac-7bc1-8077-6e960dbc5572',
    label: '共享文件',
    protocol: 'smb',
    endpoint: { host: 'nas.internal', port: 445 },
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
const ftpsPasswordReference = {
  backend: 'secret_service' as const,
  item_id: '019fe096-aeac-7bc1-8077-6e960dbc5594',
}
const ftpsPasswordProfile: StoredRemoteProfile = {
  profile: {
    id: '019fe096-aeac-7bc1-8077-6e960dbc5574',
    label: '安全文件站',
    protocol: 'ftps_explicit',
    endpoint: { host: 'files.internal', port: 21 },
    username: 'operator',
    domain: null,
    authentication: { method: 'password', secret: ftpsPasswordReference },
    trust: { kind: 'system_tls' },
    options: {
      protocol: 'ftps_explicit',
      data_connection: 'active_restricted',
      require_protected_data_channel: true,
    },
  },
  revision: 4,
  createdAtUnixMs: 1,
  updatedAtUnixMs: 2,
}
const sshPrivateKeyReference = {
  backend: 'secret_service' as const,
  item_id: '019fe096-aeac-7bc1-8077-6e960dbc5597',
}
const sshPassphraseReference = {
  backend: 'secret_service' as const,
  item_id: '019fe096-aeac-7bc1-8077-6e960dbc5598',
}
const encryptedSshProfile: StoredRemoteProfile = {
  ...storedProfile('ssh', '019fe096-aeac-7bc1-8077-6e960dbc5575', '加密密钥终端'),
  profile: {
    ...storedProfile('ssh', '019fe096-aeac-7bc1-8077-6e960dbc5575', '加密密钥终端').profile,
    authentication: {
      method: 'ssh_key',
      private_key: sshPrivateKeyReference,
      passphrase: sshPassphraseReference,
    },
  },
  revision: 3,
}

describe('RemoteView', () => {
  beforeEach(() => {
    window.location.hash = ''
    for (const mock of [
      mockedCatalog,
      mockedProfiles,
      mockedConnect,
      mockedCreateDirectory,
      mockedDeleteEntry,
      mockedDeleteProfile,
      mockedDeleteSecret,
      mockedList,
      mockedDisconnect,
      mockedOpenTerminal,
      mockedCloseTerminal,
      mockedPollTerminal,
      mockedReadTerminal,
      mockedRenameEntry,
      mockedResizeTerminal,
      mockedStoreSecret,
      mockedStreamTerminal,
      mockedUpsert,
      mockedWriteTerminal,
    ]) mock.mockReset()

    terminalTestState.terminals.length = 0
    terminalTestState.fitAddons.length = 0
    terminalTestState.resizeObservers.length = 0
    navigationTestState.leaveGuard = null
    vi.stubGlobal('ResizeObserver', class {
      observe = vi.fn()
      disconnect = vi.fn()
      callback: ResizeObserverCallback

      constructor(callback: ResizeObserverCallback) {
        this.callback = callback
        terminalTestState.resizeObservers.push(this)
      }
    })

    mockedCatalog.mockResolvedValue({
      kind: 'data',
      data: {
        schemaVersion: 1,
        snapshotId: '019fe096-aeac-7bc1-8077-6e960dbc5570',
        capturedAtUnixMs: 1,
        adapters: ['ssh', 'sftp', 'ftp', 'ftps_explicit', 'smb'].map((protocol) => adapter(protocol as RemoteProtocol)),
      },
    })
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [sshProfile, sftpProfile], nextAfter: null },
    })
    mockedDisconnect.mockResolvedValue({ kind: 'data', data: 'session-id' })
    mockedDeleteSecret.mockResolvedValue({ kind: 'data', data: '019fe096-aeac-7bc1-8077-6e960dbc5590' })
    mockedCloseTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'closed_by_client', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedResizeTerminal.mockResolvedValue({ kind: 'data', data: '019fe096-aeac-7bc1-8077-6e960dbc5588' })
    mockedStreamTerminal.mockImplementation(async (sessionId, maxBytes, onEvent) => {
      const statusResult = await mockedPollTerminal(sessionId)
      if (statusResult.kind === 'error') return statusResult
      onEvent({ event: 'started', sessionId, maxBytes, status: statusResult.data })
      const readResult = await mockedReadTerminal(sessionId, maxBytes)
      if (readResult.kind === 'error') return readResult
      if (readResult.data.status === 'data') {
        onEvent({ event: 'data', sessionId, encodedData: readResult.data.encodedData })
      }
      if (statusResult.data.state !== 'running') {
        onEvent({ event: 'ended', sessionId, status: statusResult.data })
      }
      return { kind: 'data', data: undefined }
    })
  })

  it('renders only backend-provided profiles and capability facts', async () => {
    const wrapper = mount(RemoteView)
    await flushPromises()

    expect(wrapper.text()).toContain('生产终端')
    expect(wrapper.text()).toContain('ssh.local:22')
    expect(wrapper.text()).toContain('固定 OpenSSH')
    expect(wrapper.text()).not.toContain('smb_transfer_endpoint_unverified')
    expect(wrapper.text()).not.toContain('示例服务器')
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps the last successful profiles visible when a refresh fails', async () => {
    const wrapper = mount(RemoteView)
    await flushPromises()
    mockedCatalog.mockResolvedValueOnce({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'remote_catalog_unavailable',
        reason: 'remote_catalog_unavailable',
        retryable: true,
      },
    })

    await wrapper.get('.remote-refresh').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('生产终端')
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(1)
    expect(wrapper.text()).toContain('remote_catalog_unavailable')
    expect(wrapper.find('.remote-workspace-state.is-error').exists()).toBe(false)
    wrapper.unmount()
  })

  it('edits a profile in place with its revision and preserves unexposed SSH policy', async () => {
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 1, createdAtUnixMs: 1, updatedAtUnixMs: 2 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()

    const editButton = wrapper.get('.remote-profile-edit')
    expect(editButton.attributes('aria-label')).toBe('编辑连接 生产终端')
    await editButton.trigger('click')

    expect(wrapper.get('.remote-form-heading strong').text()).toBe('编辑 SSH 终端')
    expect((wrapper.get('#remote-profile-label').element as HTMLInputElement).value).toBe('生产终端')
    expect((wrapper.get('#remote-profile-host').element as HTMLInputElement).value).toBe('ssh.local')

    await wrapper.get('#remote-profile-label').setValue('生产终端 A')
    await wrapper.get('#remote-profile-host').setValue('ssh-a.internal')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      id: sshProfile.profile.id,
      label: '生产终端 A',
      endpoint: { host: 'ssh-a.internal', port: 22 },
      authentication: { method: 'ssh_agent' },
      trust: sshProfile.profile.trust,
      options: sshProfile.profile.options,
    }), sshProfile.revision)
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(1)
    expect(wrapper.text()).toContain('生产终端 A')
    expect(wrapper.text()).not.toContain('生产终端ssh.local')
    wrapper.unmount()
  })

  it('keeps an existing password reference when the edit password is blank', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [ftpsPasswordProfile], nextAfter: null },
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 5, createdAtUnixMs: 1, updatedAtUnixMs: 3 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-profile-edit').trigger('click')

    const password = wrapper.get('#remote-profile-password')
    expect(password.attributes('placeholder')).toBe('留空则保留现有密码')
    expect(password.attributes('required')).toBeUndefined()
    expect(wrapper.get('.remote-saved-secret-state').text()).toContain('密码已保存在 Secret Service')
    await wrapper.get('#remote-profile-label').setValue('安全文件站 A')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret).not.toHaveBeenCalled()
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      id: ftpsPasswordProfile.profile.id,
      label: '安全文件站 A',
      authentication: { method: 'password', secret: ftpsPasswordReference },
      options: ftpsPasswordProfile.profile.options,
    }), ftpsPasswordProfile.revision)
    wrapper.unmount()
  })

  it('replaces an edited password and deletes the old reference only after profile storage succeeds', async () => {
    const replacement = {
      backend: 'secret_service' as const,
      item_id: '019fe096-aeac-7bc1-8077-6e960dbc5595',
    }
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [ftpsPasswordProfile], nextAfter: null },
    })
    mockedStoreSecret.mockResolvedValue({ kind: 'data', data: replacement })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 5, createdAtUnixMs: 1, updatedAtUnixMs: 3 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('#remote-profile-password').setValue('replacement secret')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      id: ftpsPasswordProfile.profile.id,
      authentication: { method: 'password', secret: replacement },
    }), ftpsPasswordProfile.revision)
    expect(mockedDeleteSecret).toHaveBeenCalledWith(ftpsPasswordReference)
    expect(mockedUpsert.mock.invocationCallOrder[0]).toBeLessThan(mockedDeleteSecret.mock.invocationCallOrder[0])
    wrapper.unmount()
  })

  it('keeps SMB factual while allowing profile creation', async () => {
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="smb"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('创建或选择 SMB 配置')
    expect(wrapper.text()).toContain('连接后浏览共享与文件')
    expect(wrapper.text()).toContain('可用')
    expect(wrapper.text()).toContain('10 / 11')
    expect(wrapper.text()).toContain('未完成授权端点互操作验证')
    expect(wrapper.get('.remote-new-button').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('creates an SMB password profile using only an opaque Secret Service reference', async () => {
    const reference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5593' }
    const capturedSecretBytes: number[][] = []
    mockedStoreSecret.mockImplementation(async (_kind, value) => {
      capturedSecretBytes.push(Array.from(value))
      return { kind: 'data', data: reference }
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="smb"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('文件共享')
    await wrapper.get('#remote-profile-host').setValue('nas.internal')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-password').setValue('smb secret value')
    await wrapper.get('#remote-profile-share').setValue('documents')
    await wrapper.get('#remote-profile-domain').setValue('WORKGROUP')
    await wrapper.get('#remote-profile-smb-dialect').setValue('smb3')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(capturedSecretBytes).toEqual([Array.from(new TextEncoder().encode('smb secret value'))])
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      protocol: 'smb',
      endpoint: { host: 'nas.internal', port: 445 },
      username: 'operator',
      domain: 'WORKGROUP',
      authentication: { method: 'password', secret: reference },
      trust: { kind: 'smb_negotiated' },
      options: {
        protocol: 'smb',
        share: 'documents',
        minimum_dialect: 'smb3',
        require_signing: true,
        require_encryption: true,
      },
    }))
    expect(JSON.stringify(mockedUpsert.mock.calls)).not.toContain('smb secret value')
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('creates an SMB Kerberos profile without storing a secret or realm override', async () => {
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="smb"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('Kerberos 共享')
    await wrapper.get('#remote-profile-host').setValue('nas.internal')
    await wrapper.get('#remote-profile-authentication').setValue('kerberos')
    await wrapper.get('#remote-profile-share').setValue('documents')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret).not.toHaveBeenCalled()
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      protocol: 'smb',
      username: null,
      domain: null,
      authentication: { method: 'kerberos' },
    }))
    wrapper.unmount()
  })

  it('requires explicit plaintext acknowledgement before storing an FTP profile', async () => {
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: {
        profile,
        revision: 0,
        createdAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftp"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')

    expect(wrapper.text()).toContain('匿名认证 · 明文传输')
    expect(wrapper.text()).toContain('我了解 FTP 会明文传输凭据和文件内容')
    expect(wrapper.get('.remote-profile-form .remote-primary-button').attributes('disabled')).toBeDefined()

    await wrapper.get('#remote-profile-label').setValue('旧设备')
    await wrapper.get('#remote-profile-host').setValue('legacy.local')
    await wrapper.get('.remote-ftp-confirmation input').setValue(true)
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      protocol: 'ftp',
      endpoint: { host: 'legacy.local', port: 21 },
      authentication: { method: 'anonymous' },
      trust: { kind: 'plaintext_acknowledged' },
      options: { protocol: 'ftp', data_connection: 'passive' },
    }))
    wrapper.unmount()
  })

  it('stores an FTP password in Secret Service and upserts only its opaque reference', async () => {
    const reference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5590' }
    const capturedSecretBytes: number[][] = []
    mockedStoreSecret.mockImplementation(async (_kind, value) => {
      capturedSecretBytes.push(Array.from(value))
      return { kind: 'data', data: reference }
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('安全文件站')
    await wrapper.get('#remote-profile-host').setValue('files.internal')
    await wrapper.get('#remote-profile-authentication').setValue('password')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-password').setValue('correct horse battery staple')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(capturedSecretBytes).toEqual([Array.from(new TextEncoder().encode('correct horse battery staple'))])
    expect(mockedStoreSecret.mock.calls[0]?.[0]).toBe('password')
    expect(Array.from(mockedStoreSecret.mock.calls[0]?.[1] ?? [])).toEqual(new Array(28).fill(0))
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      protocol: 'ftps_explicit',
      username: 'operator',
      authentication: { method: 'password', secret: reference },
      trust: { kind: 'system_tls' },
    }))
    expect(JSON.stringify(mockedUpsert.mock.calls)).not.toContain('correct horse battery staple')
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('updates FTPS to one pinned certificate while retaining hostname verification', async () => {
    const certificatePem = '-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n'
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [ftpsPasswordProfile], nextAfter: null },
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 5, createdAtUnixMs: 1, updatedAtUnixMs: 2 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('#remote-profile-host').setValue('sea')
    await wrapper.get('#remote-profile-ftps-trust').setValue('pinned_tls_certificate')
    await wrapper.get('#remote-profile-ftps-certificate').setValue(certificatePem)
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      endpoint: { host: 'sea', port: 21 },
      trust: { kind: 'pinned_tls_certificate', certificate_pem: certificatePem },
    }), 4)
    expect(mockedStoreSecret).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('stores an unencrypted SSH private key by reference and requires first-use confirmation', async () => {
    const reference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5591' }
    mockedStoreSecret.mockResolvedValue({ kind: 'data', data: reference })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('生产终端')
    await wrapper.get('#remote-profile-host').setValue('ssh.internal')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-authentication').setValue('ssh_key')
    await wrapper.get('#remote-profile-private-key').setValue('-----BEGIN OPENSSH PRIVATE KEY-----\nfixture\n-----END OPENSSH PRIVATE KEY-----')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret.mock.calls[0]?.[0]).toBe('private_key')
    expect(Array.from(mockedStoreSecret.mock.calls[0]?.[1] ?? []).every((byte) => byte === 0)).toBe(true)
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      authentication: { method: 'ssh_key', private_key: reference, passphrase: null },
      trust: { kind: 'ssh_known_hosts', first_use: 'ask_user' },
    }))
    expect(JSON.stringify(mockedUpsert.mock.calls)).not.toContain('BEGIN OPENSSH PRIVATE KEY')
    wrapper.unmount()
  })

  it('can enable first-use confirmation when editing a strict SSH profile', async () => {
    const strictProfile = structuredClone(sshProfile)
    strictProfile.profile.trust = { kind: 'ssh_known_hosts', first_use: 'reject' }
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [strictProfile], nextAfter: null },
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 1, createdAtUnixMs: 1, updatedAtUnixMs: 2 },
    }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')

    const trustInput = wrapper.get<HTMLInputElement>('.remote-policy-checkbox input[type="checkbox"]')
    expect(trustInput.element.checked).toBe(false)
    await trustInput.setValue(true)
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      id: strictProfile.profile.id,
      trust: { kind: 'ssh_known_hosts', first_use: 'ask_user' },
    }), strictProfile.revision)
    wrapper.unmount()
  })

  it('requires confirmation before closing a dirty profile form', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('未保存连接')

    await wrapper.get('[aria-label="关闭配置表单"]').trigger('click')
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(wrapper.find('.remote-profile-form').exists()).toBe(true)
    expect(wrapper.get<HTMLInputElement>('#remote-profile-label').element.value).toBe('未保存连接')

    await wrapper.get('[aria-label="关闭配置表单"]').trigger('click')
    expect(confirm).toHaveBeenCalledTimes(2)
    expect(wrapper.find('.remote-profile-form').exists()).toBe(false)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('protects a dirty profile form on protocol, route, and window leave', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(false).mockReturnValueOnce(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-host').setValue('pending.local')

    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(wrapper.get('[data-remote-tab="ssh"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.get<HTMLInputElement>('#remote-profile-host').element.value).toBe('pending.local')

    expect(navigationTestState.leaveGuard?.()).toBe(false)
    expect(confirm).toHaveBeenCalledTimes(2)
    const beforeUnload = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(beforeUnload)
    expect(beforeUnload.defaultPrevented).toBe(true)
    expect(navigationTestState.leaveGuard?.()).toBe(true)
    expect(confirm).toHaveBeenCalledTimes(3)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('stores an SSH password by opaque reference without passing plaintext to the profile bridge', async () => {
    const reference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5596' }
    mockedStoreSecret.mockResolvedValue({ kind: 'data', data: reference })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('密码终端')
    await wrapper.get('#remote-profile-host').setValue('ssh.internal')
    await wrapper.get('#remote-profile-authentication').setValue('password')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-password').setValue('ssh password secret')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret.mock.calls[0]?.[0]).toBe('password')
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      protocol: 'ssh',
      authentication: { method: 'password', secret: reference },
    }))
    expect(JSON.stringify(mockedUpsert.mock.calls)).not.toContain('ssh password secret')
    wrapper.unmount()
  })

  it('locks profile queries and duplicate submits while saving, then restores input after failure', async () => {
    let finishUpsert!: (result: {
      kind: 'error'
      error: { kind: 'transport'; code: string; reason: string; retryable: true }
    }) => void
    mockedUpsert.mockReturnValue(new Promise((resolve) => {
      finishUpsert = resolve
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('待保存终端')
    await wrapper.get('#remote-profile-host').setValue('pending.local')

    await wrapper.get('.remote-profile-form').trigger('submit')
    await wrapper.vm.$nextTick()
    expect(wrapper.get('fieldset.remote-profile-fields').attributes('disabled')).toBeDefined()
    expect(wrapper.get<HTMLInputElement>('#remote-profile-label').element.matches(':disabled')).toBe(true)
    expect(wrapper.get('.remote-new-button').attributes('disabled')).toBeDefined()
    expect(wrapper.get('.remote-refresh').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-remote-tab="sftp"]').attributes('disabled')).toBeDefined()
    await wrapper.get('.remote-profile-form').trigger('submit')
    await wrapper.get('.remote-refresh').trigger('click')
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    expect(mockedUpsert).toHaveBeenCalledTimes(1)
    expect(mockedCatalog).toHaveBeenCalledTimes(1)
    expect(mockedProfiles).toHaveBeenCalledTimes(1)
    expect(wrapper.get('[data-remote-tab="ssh"]').attributes('aria-selected')).toBe('true')

    finishUpsert({
      kind: 'error',
      error: { kind: 'transport', code: 'profile_store_unavailable', reason: 'profile_store_unavailable', retryable: true },
    })
    await flushPromises()
    expect(wrapper.get('fieldset.remote-profile-fields').attributes('disabled')).toBeUndefined()
    expect(wrapper.get('.remote-new-button').attributes('disabled')).toBeUndefined()
    expect(wrapper.get('.remote-refresh').attributes('disabled')).toBeUndefined()
    expect(wrapper.get('[data-remote-tab="sftp"]').attributes('disabled')).toBeUndefined()
    expect(wrapper.get<HTMLInputElement>('#remote-profile-label').element.value).toBe('待保存终端')
    expect(wrapper.get<HTMLInputElement>('#remote-profile-host').element.value).toBe('pending.local')
    expect(wrapper.text()).toContain('profile_store_unavailable')
    wrapper.unmount()
  })

  it('stores an encrypted SSH key and passphrase as separate opaque references', async () => {
    const privateKeyReference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5597' }
    const passphraseReference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5598' }
    mockedStoreSecret
      .mockResolvedValueOnce({ kind: 'data', data: privateKeyReference })
      .mockResolvedValueOnce({ kind: 'data', data: passphraseReference })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('加密密钥终端')
    await wrapper.get('#remote-profile-host').setValue('ssh.internal')
    await wrapper.get('#remote-profile-authentication').setValue('ssh_key')
    await wrapper.get('#remote-profile-private-key').setValue('encrypted private key fixture')
    await wrapper.get('#remote-profile-key-passphrase').setValue('key passphrase secret')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret.mock.calls.map(([kind]) => kind)).toEqual(['private_key', 'key_passphrase'])
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      authentication: {
        method: 'ssh_key',
        private_key: privateKeyReference,
        passphrase: passphraseReference,
      },
    }))
    expect(JSON.stringify(mockedUpsert.mock.calls)).not.toContain('key passphrase secret')
    wrapper.unmount()
  })

  it('removes an existing SSH key passphrase only after profile storage succeeds', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [encryptedSshProfile], nextAfter: null },
    })
    mockedUpsert.mockImplementation(async (profile) => ({
      kind: 'data',
      data: { profile, revision: 4, createdAtUnixMs: 1, updatedAtUnixMs: 2 },
    }))
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')

    const remove = wrapper.get('input[type="checkbox"]')
    expect(wrapper.text()).toContain('移除已保存的私钥口令')
    await remove.setValue(true)
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedStoreSecret).not.toHaveBeenCalled()
    expect(mockedUpsert).toHaveBeenCalledWith(expect.objectContaining({
      authentication: {
        method: 'ssh_key',
        private_key: sshPrivateKeyReference,
        passphrase: null,
      },
    }), encryptedSshProfile.revision)
    expect(mockedDeleteSecret).toHaveBeenCalledWith(sshPassphraseReference)
    expect(mockedUpsert.mock.invocationCallOrder[0]).toBeLessThan(mockedDeleteSecret.mock.invocationCallOrder[0])
    expect(mockedDeleteSecret).not.toHaveBeenCalledWith(sshPrivateKeyReference)
    wrapper.unmount()
  })

  it('keeps an existing SSH key passphrase when profile storage fails', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [encryptedSshProfile], nextAfter: null },
    })
    mockedUpsert.mockResolvedValue({
      kind: 'error',
      error: { kind: 'transport', code: 'profile_store_unavailable', reason: 'profile_store_unavailable', retryable: true },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('input[type="checkbox"]').setValue(true)
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('profile_store_unavailable')
    expect(wrapper.get('input[type="checkbox"]').element).toHaveProperty('checked', true)
    wrapper.unmount()
  })

  it('deletes a revision-matched password profile before removing its secret', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [ftpsPasswordProfile], nextAfter: null },
    })
    mockedDeleteProfile.mockResolvedValue({ kind: 'data', data: ftpsPasswordProfile.profile.id })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('.remote-profile-form .danger-action').trigger('click')
    await flushPromises()

    expect(confirm).toHaveBeenCalledWith('确定删除连接配置“安全文件站”吗？')
    expect(mockedDeleteProfile).toHaveBeenCalledWith(ftpsPasswordProfile.profile.id, ftpsPasswordProfile.revision)
    expect(mockedDeleteSecret).toHaveBeenCalledWith(ftpsPasswordReference)
    expect(mockedDeleteProfile.mock.invocationCallOrder[0]).toBeLessThan(mockedDeleteSecret.mock.invocationCallOrder[0])
    expect(wrapper.find('.remote-profile-form').exists()).toBe(false)
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(0)
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('keeps the profile and its secrets when profile deletion is rejected', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [ftpsPasswordProfile], nextAfter: null },
    })
    mockedDeleteProfile.mockResolvedValue({
      kind: 'error',
      error: { kind: 'daemon', code: 'remote_profile_in_use', reason: 'remote_profile_in_use', retryable: true },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('.remote-profile-form .danger-action').trigger('click')
    await flushPromises()

    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(wrapper.find('.remote-profile-form').exists()).toBe(true)
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(1)
    expect(wrapper.text()).toContain('remote_profile_in_use')
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('keeps a secret-cleanup error visible after the profile was deleted', async () => {
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [encryptedSshProfile], nextAfter: null },
    })
    mockedDeleteProfile.mockResolvedValue({ kind: 'data', data: encryptedSshProfile.profile.id })
    mockedDeleteSecret
      .mockResolvedValueOnce({ kind: 'data', data: sshPassphraseReference.item_id })
      .mockResolvedValueOnce({
        kind: 'error',
        error: { kind: 'permission', code: 'secret_service_locked', reason: 'secret_service_locked', retryable: true },
      })
      .mockResolvedValueOnce({ kind: 'data', data: sshPrivateKeyReference.item_id })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('.remote-profile-form .danger-action').trigger('click')
    await flushPromises()

    expect(mockedDeleteProfile).toHaveBeenCalledOnce()
    expect(mockedDeleteSecret).toHaveBeenCalledTimes(2)
    expect(wrapper.findAll('.remote-profile-item')).toHaveLength(0)
    expect(wrapper.text()).toContain('secret_service_locked')
    expect(wrapper.text()).toContain('重试清理凭据')
    await wrapper.get('.remote-error-retry').trigger('click')
    await flushPromises()
    expect(mockedDeleteSecret).toHaveBeenCalledTimes(3)
    expect(mockedDeleteSecret).toHaveBeenLastCalledWith(sshPrivateKeyReference)
    expect(wrapper.find('.remote-operation-error').exists()).toBe(false)
    confirm.mockRestore()
    wrapper.unmount()
    await flushPromises()
  })

  it('blocks profile deletion while its SSH terminal is active', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'no_data' } })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('.remote-profile-form .danger-action').trigger('click')
    await flushPromises()

    expect(confirm).not.toHaveBeenCalled()
    expect(mockedDeleteProfile).not.toHaveBeenCalled()
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('remote_profile_session_active')
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('blocks profile deletion while its file session is active', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({
      kind: 'data',
      data: { sessionId, path: '/', offset: 0, entries: [], nextOffset: null },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-profile-edit').trigger('click')
    await wrapper.get('.remote-profile-form .danger-action').trigger('click')
    await flushPromises()

    expect(confirm).not.toHaveBeenCalled()
    expect(mockedDeleteProfile).not.toHaveBeenCalled()
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('remote_profile_session_active')
    confirm.mockRestore()
    wrapper.unmount()
  })

  it('deletes a newly stored secret when profile upsert fails', async () => {
    const reference = { backend: 'secret_service' as const, item_id: '019fe096-aeac-7bc1-8077-6e960dbc5592' }
    mockedStoreSecret.mockResolvedValue({ kind: 'data', data: reference })
    mockedUpsert.mockResolvedValue({
      kind: 'error',
      error: { kind: 'transport', code: 'profile_store_unavailable', reason: 'profile_store_unavailable', retryable: true },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('失败配置')
    await wrapper.get('#remote-profile-host').setValue('files.internal')
    await wrapper.get('#remote-profile-authentication').setValue('password')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-password').setValue('not persisted')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedDeleteSecret).toHaveBeenCalledWith(reference)
    expect(wrapper.text()).toContain('profile_store_unavailable')
    expect(wrapper.get('#remote-profile-password').element).toHaveProperty('value', 'not persisted')
    wrapper.unmount()
  })

  it('does not upsert a profile when Secret Service rejects the value', async () => {
    mockedStoreSecret.mockResolvedValue({
      kind: 'error',
      error: { kind: 'permission', code: 'secret_service_locked', reason: 'secret_service_locked', retryable: true },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="ftps_explicit"]').trigger('click')
    await wrapper.get('.remote-new-button').trigger('click')
    await wrapper.get('#remote-profile-label').setValue('锁定凭据')
    await wrapper.get('#remote-profile-host').setValue('files.internal')
    await wrapper.get('#remote-profile-authentication').setValue('password')
    await wrapper.get('#remote-profile-user').setValue('operator')
    await wrapper.get('#remote-profile-password').setValue('still in form')
    await wrapper.get('.remote-profile-form').trigger('submit')
    await flushPromises()

    expect(mockedUpsert).not.toHaveBeenCalled()
    expect(mockedDeleteSecret).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('secret_service_locked')
    expect(wrapper.get('#remote-profile-password').element).toHaveProperty('value', 'still in form')
    wrapper.unmount()
  })

  it('opens the FTP subview directly from its startup deep link', async () => {
    window.location.hash = '#/remote?protocol=ftp'
    const wrapper = mount(RemoteView)
    await flushPromises()

    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.text()).toContain('FTP 文件 能力')
    expect(wrapper.text()).toContain('plain_ftp_explicitly_enabled')
    wrapper.unmount()
  })

  it('moves selection and focus across protocol tabs with arrow, Home, and End keys', async () => {
    const wrapper = mount(RemoteView, { attachTo: document.body })
    await flushPromises()

    await wrapper.get('[data-remote-tab="ssh"]').trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    expect(wrapper.get('[data-remote-tab="sftp"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.get('[data-remote-tab="sftp"]').attributes('tabindex')).toBe('0')
    expect(document.activeElement).toBe(wrapper.get('[data-remote-tab="sftp"]').element)

    await wrapper.get('[data-remote-tab="sftp"]').trigger('keydown', { key: 'End' })
    await flushPromises()
    expect(document.activeElement).toBe(wrapper.get('[data-remote-tab="smb"]').element)

    await wrapper.get('[data-remote-tab="smb"]').trigger('keydown', { key: 'Home' })
    await flushPromises()
    expect(document.activeElement).toBe(wrapper.get('[data-remote-tab="ssh"]').element)
    wrapper.unmount()
  })

  it('connects a selected SFTP profile and renders only returned directory entries', async () => {
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: '019fe096-aeac-7bc1-8077-6e960dbc5580',
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId: '019fe096-aeac-7bc1-8077-6e960dbc5580',
        path: '/',
        offset: 0,
        entries: [{
          name: 'reports',
          path: '/reports',
          kind: 'directory',
          sizeBytes: null,
          modifiedAtUnixMs: null,
          unixMode: null,
          capabilities: capabilities(true),
        }],
        nextOffset: null,
      },
    })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(mockedConnect).toHaveBeenCalledWith(sftpProfile.profile.id)
    expect(mockedList).toHaveBeenCalledWith('019fe096-aeac-7bc1-8077-6e960dbc5580', '/', 0)
    expect(wrapper.text()).toContain('reports')
    expect(wrapper.findAll('.remote-file-table tbody tr')).toHaveLength(1)

    await wrapper.get('[data-remote-tab="ftp"]').trigger('click')
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    expect(mockedDisconnect).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('reports')
    expect(wrapper.findAll('.remote-file-table tbody tr')).toHaveLength(1)
    wrapper.unmount()
    expect(mockedDisconnect).toHaveBeenCalledWith('019fe096-aeac-7bc1-8077-6e960dbc5580')
  })

  it('clears a cached file workspace when its 15 minute backend lease has expired', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList
      .mockResolvedValueOnce({ kind: 'data', data: { sessionId, path: '/', offset: 0, entries: [], nextOffset: null } })
      .mockResolvedValueOnce({
        kind: 'error',
        error: {
          kind: 'remote',
          code: 'remote_session_not_found',
          reason: 'remote_session_not_found',
          retryable: false,
        },
      })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.get('[data-remote-tab="ftp"]').trigger('click')
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('remote_session_not_found')
    expect(wrapper.text()).toContain('连接并浏览')
    expect(wrapper.get('.remote-workspace-state.remote-file-connect-placeholder').exists()).toBe(true)
    expect(wrapper.find('.remote-file-browser').exists()).toBe(false)
    wrapper.unmount()
  })

  it('disconnects the old file session when selecting another profile', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [sftpProfile, backupSftpProfile], nextAfter: null },
    })
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({
      kind: 'data',
      data: { sessionId, path: '/', offset: 0, entries: [], nextOffset: null },
    })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.findAll('.remote-profile-item')[1].trigger('click')
    await flushPromises()

    expect(mockedDisconnect).toHaveBeenCalledWith(sessionId)
    expect(wrapper.text()).toContain('浏览 备份文件')
    expect(wrapper.find('.remote-file-browser').exists()).toBe(false)
    wrapper.unmount()
  })

  it('removes a file session locally and reports a typed disconnect cleanup failure', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5590'
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({ kind: 'data', data: { sessionId, path: '/', offset: 0, entries: [], nextOffset: null } })
    mockedDisconnect.mockResolvedValue({
      kind: 'error',
      error: { kind: 'remote', code: 'remote_disconnect_failed', reason: 'remote_disconnect_failed', retryable: true },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-session-actions .remote-secondary-button').trigger('click')
    await flushPromises()

    expect(wrapper.find('.remote-file-browser').exists()).toBe(false)
    expect(wrapper.text()).toContain('remote_disconnect_failed')
    wrapper.unmount()
  })

  it('disconnects a late file-session result after selecting another profile', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    let finishConnect: (() => void) | null = null
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [sftpProfile, backupSftpProfile], nextAfter: null },
    })
    mockedConnect.mockImplementation(() => new Promise((resolve) => {
      finishConnect = () => resolve({
        kind: 'data',
        data: {
          id: sessionId,
          profileId: sftpProfile.profile.id,
          protocol: 'sftp',
          state: 'ready',
          stateReason: null,
          capabilities: capabilities(true),
          openedAtUnixMs: 1,
          updatedAtUnixMs: 1,
        },
      })
    }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await wrapper.findAll('.remote-profile-item')[1].trigger('click')
    finishConnect?.()
    await flushPromises()

    expect(mockedDisconnect).toHaveBeenCalledWith(sessionId)
    expect(mockedList).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('浏览 备份文件')
    expect(wrapper.find('.remote-file-browser').exists()).toBe(false)
    wrapper.unmount()
  })

  it('ignores a stale directory response after a newer navigation succeeds', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    let finishReports: (() => void) | null = null
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId,
          path: '/',
          offset: 0,
          entries: [
            { name: 'reports', path: '/reports', kind: 'directory', sizeBytes: null, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) },
            { name: 'archive', path: '/archive', kind: 'directory', sizeBytes: null, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) },
          ],
          nextOffset: null,
        },
      })
      .mockImplementationOnce(() => new Promise((resolve) => {
        finishReports = () => resolve({
          kind: 'data',
          data: {
            sessionId,
            path: '/reports',
            offset: 0,
            entries: [{ name: 'old.txt', path: '/reports/old.txt', kind: 'file', sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) }],
            nextOffset: null,
          },
        })
      }))
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId,
          path: '/archive',
          offset: 0,
          entries: [{ name: 'latest.txt', path: '/archive/latest.txt', kind: 'file', sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) }],
          nextOffset: null,
        },
      })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    const directoryButtons = wrapper.findAll('.remote-file-table tbody th button')
    await directoryButtons[0].trigger('click')
    await directoryButtons[1].trigger('click')
    await flushPromises()
    finishReports?.()
    await flushPromises()

    expect(wrapper.text()).toContain('/archive')
    expect(wrapper.text()).toContain('latest.txt')
    expect(wrapper.text()).not.toContain('old.txt')
    wrapper.unmount()
  })

  it('commits directory pagination history only after a successful page request', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    const firstEntries = [
      { name: 'one.txt', path: '/one.txt', kind: 'file' as const, sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) },
      { name: 'two.txt', path: '/two.txt', kind: 'file' as const, sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) },
    ]
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList
      .mockResolvedValueOnce({
        kind: 'data',
        data: { sessionId, path: '/', offset: 0, entries: firstEntries, nextOffset: 2 },
      })
      .mockResolvedValueOnce({
        kind: 'error',
        error: { kind: 'transport', code: 'directory_query_unavailable', reason: 'directory_query_unavailable', retryable: true },
      })
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId,
          path: '/',
          offset: 2,
          entries: [{ name: 'three.txt', path: '/three.txt', kind: 'file', sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) }],
          nextOffset: null,
        },
      })
      .mockResolvedValueOnce({
        kind: 'data',
        data: { sessionId, path: '/', offset: 0, entries: firstEntries, nextOffset: 2 },
      })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.get('[aria-label="下一页"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('one.txt')
    expect(wrapper.text()).toContain('directory_query_unavailable')
    expect(wrapper.get('[aria-label="上一页"]').attributes('disabled')).toBeDefined()

    await wrapper.get('[aria-label="下一页"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('three.txt')
    expect(wrapper.get('[aria-label="上一页"]').attributes('disabled')).toBeUndefined()

    await wrapper.get('[aria-label="上一页"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('one.txt')
    expect(mockedList).toHaveBeenLastCalledWith(sessionId, '/', 0)
    wrapper.unmount()
  })

  it('shows a retryable state when the initial directory request fails', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList
      .mockResolvedValueOnce({
        kind: 'error',
        error: { kind: 'transport', code: 'directory_query_unavailable', reason: 'directory_query_unavailable', retryable: true },
      })
      .mockResolvedValueOnce({
        kind: 'data',
        data: { sessionId, path: '/', offset: 0, entries: [], nextOffset: null },
      })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('目录不可用')
    expect(wrapper.text()).toContain('directory_query_unavailable')
    await wrapper.get('.remote-file-browser .remote-workspace-state .remote-secondary-button').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('目录为空')
    wrapper.unmount()
  })

  it('creates, renames, and deletes entries through typed file-session commands', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5580'
    const draftEntry = {
      name: 'draft.txt',
      path: '/draft.txt',
      kind: 'file' as const,
      sizeBytes: 4,
      modifiedAtUnixMs: null,
      unixMode: null,
      capabilities: capabilities(true),
    }
    const directoryPage = {
      sessionId,
      path: '/',
      offset: 0,
      entries: [draftEntry],
      nextOffset: null,
    }
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({ kind: 'data', data: directoryPage })
    mockedCreateDirectory.mockResolvedValue({
      kind: 'data',
      data: { ...draftEntry, name: 'reports', path: '/reports', kind: 'directory', sizeBytes: null },
    })
    mockedRenameEntry.mockResolvedValue({
      kind: 'data',
      data: { ...draftEntry, name: 'final.txt', path: '/final.txt' },
    })
    mockedDeleteEntry.mockResolvedValue({ kind: 'data', data: sessionId })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.get('[aria-label="新建文件夹"]').trigger('click')
    await wrapper.get('.remote-file-action input').setValue('reports')
    await wrapper.get('.remote-file-action').trigger('submit')
    await flushPromises()
    expect(mockedCreateDirectory).toHaveBeenCalledWith(sessionId, '/reports')

    await wrapper.get('[aria-label="重命名"]').trigger('click')
    await wrapper.get('.remote-file-action input').setValue('final.txt')
    await wrapper.get('.remote-file-action').trigger('submit')
    await flushPromises()
    expect(mockedRenameEntry).toHaveBeenCalledWith(sessionId, '/draft.txt', '/final.txt')

    await wrapper.get('[aria-label="删除"]').trigger('click')
    expect(wrapper.text()).toContain('删除 draft.txt')
    await wrapper.get('.remote-file-action').trigger('submit')
    await flushPromises()
    expect(mockedDeleteEntry).toHaveBeenCalledWith(sessionId, '/draft.txt')
    expect(mockedList).toHaveBeenCalledTimes(4)

    await wrapper.get('[aria-label="下载"]').trigger('click')
    expect(window.location.hash).toBe(`#/transfers?direction=download&profile=${sftpProfile.profile.id}&path=%2Fdraft.txt`)
    wrapper.unmount()
  })

  it('makes remote deletion explicit and freezes directory navigation while it is pending', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5581'
    const draftEntry = {
      name: 'draft.txt',
      path: '/draft.txt',
      kind: 'file' as const,
      sizeBytes: 4,
      modifiedAtUnixMs: null,
      unixMode: null,
      capabilities: capabilities(true),
    }
    const directoryEntry = {
      ...draftEntry,
      name: 'reports',
      path: '/reports',
      kind: 'directory' as const,
      sizeBytes: null,
      capabilities: capabilities(false),
    }
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        path: '/',
        offset: 0,
        entries: [draftEntry, directoryEntry],
        nextOffset: 2,
      },
    })
    let failDelete!: () => void
    mockedDeleteEntry.mockImplementation(() => new Promise((resolve) => {
      failDelete = () => resolve({
        kind: 'error',
        error: {
          kind: 'remote',
          code: 'remote_delete_failed',
          reason: 'remote_delete_failed',
          retryable: true,
        },
      })
    }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.get('[aria-label="删除"]').trigger('click')
    const action = wrapper.get('.remote-file-action')
    expect(action.text()).toContain('确定删除 draft.txt？删除后无法撤销。')
    expect(action.get('button[type="submit"]').classes()).toContain('danger-action')
    expect(action.get('button[type="submit"]').text()).toContain('确认删除')
    expect(wrapper.get('tbody th button').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[aria-label="下一页"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('.remote-profile-item').attributes('disabled')).toBeDefined()
    expect(wrapper.get('.remote-refresh').attributes('disabled')).toBeDefined()
    expect(navigationTestState.leaveGuard?.()).toBe(false)

    await action.trigger('submit')
    await action.trigger('submit')
    expect(mockedDeleteEntry).toHaveBeenCalledTimes(1)
    expect(wrapper.get('tbody th button').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[aria-label="下一页"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('.remote-profile-item').attributes('disabled')).toBeDefined()
    expect(wrapper.get('.remote-refresh').attributes('disabled')).toBeDefined()
    expect(navigationTestState.leaveGuard?.()).toBe(false)
    const unloadEvent = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(unloadEvent)
    expect(unloadEvent.defaultPrevented).toBe(true)
    await wrapper.get('tbody th button').trigger('click')
    await wrapper.get('[aria-label="下一页"]').trigger('click')
    expect(mockedList).toHaveBeenCalledTimes(1)

    failDelete()
    await flushPromises()
    expect(wrapper.text()).toContain('remote_delete_failed')
    expect(wrapper.find('.remote-file-action').exists()).toBe(true)
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('disabled')).toBeDefined()
    expect(navigationTestState.leaveGuard?.()).toBe(false)
    expect(mockedList).toHaveBeenCalledTimes(1)
    await wrapper.get('[aria-label="取消文件操作"]').trigger('click')
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('disabled')).toBeUndefined()
    expect(navigationTestState.leaveGuard?.()).toBe(true)
    wrapper.unmount()
  })

  it('keeps pagination history when a successful mutation cannot refresh the directory', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5582'
    const firstEntry = { name: 'one.txt', path: '/one.txt', kind: 'file' as const, sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) }
    const secondEntry = { name: 'two.txt', path: '/two.txt', kind: 'file' as const, sizeBytes: 1, modifiedAtUnixMs: null, unixMode: null, capabilities: capabilities(true) }
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: sessionId,
        profileId: sftpProfile.profile.id,
        protocol: 'sftp',
        state: 'ready',
        stateReason: null,
        capabilities: capabilities(true),
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList
      .mockResolvedValueOnce({ kind: 'data', data: { sessionId, path: '/', offset: 0, entries: [firstEntry], nextOffset: 1 } })
      .mockResolvedValueOnce({ kind: 'data', data: { sessionId, path: '/', offset: 1, entries: [secondEntry], nextOffset: null } })
      .mockResolvedValueOnce({
        kind: 'error',
        error: { kind: 'transport', code: 'directory_refresh_failed', reason: 'directory_refresh_failed', retryable: true },
      })
      .mockResolvedValueOnce({ kind: 'data', data: { sessionId, path: '/', offset: 0, entries: [firstEntry], nextOffset: 1 } })
    mockedRenameEntry.mockResolvedValue({ kind: 'data', data: { ...secondEntry, name: 'renamed.txt', path: '/renamed.txt' } })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('[aria-label="下一页"]').trigger('click')
    await flushPromises()
    await wrapper.get('[aria-label="重命名"]').trigger('click')
    await wrapper.get('.remote-file-action input').setValue('renamed.txt')
    await wrapper.get('.remote-file-action').trigger('submit')
    await flushPromises()

    expect(wrapper.text()).toContain('directory_refresh_failed')
    expect(wrapper.text()).toContain('two.txt')
    expect(wrapper.get('[aria-label="上一页"]').attributes('disabled')).toBeUndefined()
    await wrapper.get('[aria-label="上一页"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('one.txt')
    wrapper.unmount()
  })

  it('uses a raw interactive terminal and disposes its resources with the session', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const output = new Uint8Array([0x1b, 0x5b, 0x33, 0x31, 0x6d, 0x4f, 0x4b, 0x1b, 0x5b, 0x30, 0x6d])
    const scrollSpy = vi.fn()
    Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: scrollSpy })
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'running', detail: null, transcriptRetainedBytes: output.byteLength, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({
      kind: 'data',
      data: { status: 'data', encodedData: btoa(String.fromCharCode(...output)) },
    })
    mockedWriteTerminal.mockImplementation(async (_sessionId, encodedData) => ({
      kind: 'data',
      data: Uint8Array.from(atob(encodedData), (character) => character.charCodeAt(0)).byteLength,
    }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    expect(wrapper.get('.remote-terminal-placeholder').classes()).toContain('remote-ssh-surface')
    expect(wrapper.get('.remote-terminal-placeholder').text()).toContain('终端尚未打开')
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(mockedOpenTerminal).toHaveBeenCalledWith(sshProfile.profile.id, {
      rows: 24,
      columns: 80,
      pixelWidth: 0,
      pixelHeight: 0,
    }, false)
    expect(wrapper.get('.remote-terminal').classes()).toContain('remote-ssh-surface')
    expect(wrapper.find('.remote-terminal .remote-session-bar').exists()).toBe(false)
    expect(wrapper.get('[aria-label="关闭 SSH 终端"]').exists()).toBe(true)
    expect(wrapper.get('.remote-terminal-output').attributes('aria-label')).toBe('生产终端 交互式终端')
    const xterm = terminalTestState.terminals[0]
    expect(Array.from(xterm.write.mock.calls[0][0] as Uint8Array)).toEqual(Array.from(output))
    expect(mockedReadTerminal).toHaveBeenCalledWith(sessionId, 45_056)
    expect(xterm.options.scrollback).toBe(5_000)
    expect(xterm.options.theme).toMatchObject({
      background: '#0F141A',
      foreground: '#EEF3F8',
      red: '#E27878',
      green: '#55B98B',
      yellow: '#D2A354',
      blue: '#4D8DFF',
      magenta: '#C792EA',
      cyan: '#56B6C2',
      brightBlack: '#9AA8B5',
    })
    expect(scrollSpy).toHaveBeenCalledWith({ block: 'start' })
    expect(xterm.focus).toHaveBeenCalled()

    scrollSpy.mockClear()
    xterm.focus.mockClear()
    await wrapper.get('.remote-profile-item').trigger('click')
    await flushPromises()
    expect(scrollSpy).toHaveBeenCalledWith({ block: 'start' })
    expect(scrollSpy.mock.contexts[0]).toBe(wrapper.get('.remote-terminal').element)
    expect(xterm.focus).toHaveBeenCalled()

    xterm.dataHandler?.('\u001b[A')
    xterm.dataHandler?.('\u0003')
    await vi.waitFor(() => {
      expect(mockedWriteTerminal).toHaveBeenCalledOnce()
      expect(mockedWriteTerminal).toHaveBeenCalledWith(sessionId, btoa('\u001b[A\u0003'))
    })

    wrapper.unmount()
    expect(xterm.inputDispose).toHaveBeenCalledOnce()
    expect(xterm.dispose).toHaveBeenCalledOnce()
    expect(terminalTestState.resizeObservers[0].disconnect).toHaveBeenCalledOnce()
    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionId)
  })

  it('stops terminal input after the stream disconnects even when output fails', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5598'
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'disconnected', detail: 'connection_closed', transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({
      kind: 'error',
      error: { kind: 'remote', code: 'terminal_read_failed', reason: 'terminal_read_failed', retryable: false },
    })
    vi.useFakeTimers()

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    const xterm = terminalTestState.terminals[0]
    expect(wrapper.text()).toContain('disconnected')
    expect(wrapper.text()).toContain('terminal_read_failed')
    expect(wrapper.find('.remote-profile-copy small').text()).toContain('disconnected')
    xterm.dataHandler?.('ignored')
    await flushPromises()
    expect(mockedWriteTerminal).not.toHaveBeenCalled()
    expect(mockedPollTerminal).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(2_000)
    expect(mockedPollTerminal).toHaveBeenCalledTimes(1)

    wrapper.unmount()
    vi.useRealTimers()
  })

  it('keeps SSH sessions isolated while switching profiles and closes only the active one', async () => {
    const sessionA = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const sessionB = '019fe096-aeac-7bc1-8077-6e960dbc5589'
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [sshProfile, backupSshProfile], nextAfter: null },
    })
    mockedOpenTerminal
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId: sessionA,
          capabilities: {
            maxOutputChunkBytes: 45_056,
            maxInputChunkBytes: 45_056,
            maxTranscriptBytes: 65_536,
            maxRows: 1_000,
            maxColumns: 1_000,
            maxPixelDimension: 32_767,
            nonblockingOutput: true,
            fixedOpenSshProgram: true,
          },
          status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
        },
      })
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId: sessionB,
          capabilities: {
            maxOutputChunkBytes: 45_056,
            maxInputChunkBytes: 45_056,
            maxTranscriptBytes: 65_536,
            maxRows: 1_000,
            maxColumns: 1_000,
            maxPixelDimension: 32_767,
            nonblockingOutput: true,
            fixedOpenSshProgram: true,
          },
          status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
        },
      })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })
    mockedWriteTerminal.mockImplementation(async () => ({ kind: 'data', data: 1 }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    await wrapper.findAll('.remote-profile-item')[1].trigger('click')
    await flushPromises()
    expect(wrapper.get('.remote-terminal-placeholder').text()).toContain('终端尚未打开')
    expect(wrapper.get('.remote-terminal-placeholder').classes()).toContain('remote-ssh-surface')
    expect(mockedCloseTerminal).not.toHaveBeenCalled()

    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    expect(mockedOpenTerminal).toHaveBeenNthCalledWith(1, sshProfile.profile.id, expect.any(Object), false)
    expect(mockedOpenTerminal).toHaveBeenNthCalledWith(2, backupSshProfile.profile.id, expect.any(Object), false)
    expect(mockedPollTerminal).toHaveBeenCalledWith(sessionA)
    expect(mockedPollTerminal).toHaveBeenCalledWith(sessionB)
    await vi.waitFor(() => {
      expect(mockedResizeTerminal).toHaveBeenCalledWith(sessionA, expect.any(Object))
      expect(mockedResizeTerminal).toHaveBeenCalledWith(sessionB, expect.any(Object))
    })
    expect(terminalTestState.terminals).toHaveLength(2)
    expect(wrapper.findAll('.remote-profile-copy small')[0].text()).toContain('running')
    expect(wrapper.findAll('.remote-profile-copy small')[1].text()).toContain('running')

    terminalTestState.terminals[0].dataHandler?.('a')
    terminalTestState.terminals[1].dataHandler?.('b')
    await vi.waitFor(() => {
      expect(mockedWriteTerminal).toHaveBeenCalledWith(sessionA, btoa('a'))
      expect(mockedWriteTerminal).toHaveBeenCalledWith(sessionB, btoa('b'))
    })

    await wrapper.findAll('[aria-label="关闭 SSH 终端"]')[1].trigger('click')
    await flushPromises()
    expect(mockedCloseTerminal).toHaveBeenCalledTimes(1)
    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionB)

    await wrapper.findAll('.remote-profile-item')[0].trigger('click')
    await flushPromises()
    expect(wrapper.get('.remote-terminal-output').attributes('aria-label')).toBe('生产终端 交互式终端')
    expect(terminalTestState.terminals[0].dispose).not.toHaveBeenCalled()
    wrapper.unmount()
    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionA)
  })

  it('removes a terminal locally and reports a typed close cleanup failure', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5599'
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({ kind: 'data', data: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 } })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })
    mockedCloseTerminal.mockResolvedValue({
      kind: 'error',
      error: { kind: 'remote', code: 'terminal_close_failed', reason: 'terminal_close_failed', retryable: true },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    await wrapper.get('[aria-label="关闭 SSH 终端"]').trigger('click')
    await flushPromises()

    expect(wrapper.find('.remote-terminal').exists()).toBe(false)
    expect(wrapper.text()).toContain('terminal_close_failed')
    wrapper.unmount()
  })

  it('keeps the SSH terminal mounted while switching protocol tabs', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })
    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()
    const terminal = terminalTestState.terminals[0]

    await wrapper.get('[data-remote-tab="sftp"]').trigger('click')
    expect(wrapper.get('[data-remote-tab="sftp"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.text()).toContain('SFTP 文件 能力')
    expect(mockedCloseTerminal).not.toHaveBeenCalled()
    expect(terminal.dispose).not.toHaveBeenCalled()

    await wrapper.get('[data-remote-tab="ssh"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('.remote-terminal-output').attributes('aria-label')).toBe('生产终端 交互式终端')
    expect(terminalTestState.terminals).toHaveLength(1)
    expect(terminal.focus).toHaveBeenCalled()
    wrapper.unmount()
    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionId)
  })

  it('closes a late SSH open result after the user switches to another protocol', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    let finishOpen: (() => void) | null = null
    mockedOpenTerminal.mockImplementation(() => new Promise((resolve) => {
      finishOpen = () => resolve({
        kind: 'data',
        data: {
          sessionId,
          capabilities: {
            maxOutputChunkBytes: 45_056,
            maxInputChunkBytes: 45_056,
            maxTranscriptBytes: 65_536,
            maxRows: 1_000,
            maxColumns: 1_000,
            maxPixelDimension: 32_767,
            nonblockingOutput: true,
            fixedOpenSshProgram: true,
          },
          status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
        },
      })
    }))

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    expect(wrapper.get('.remote-workspace .remote-primary-button').attributes('disabled')).toBeDefined()

    await wrapper.get('[data-remote-tab="ftp"]').trigger('click')
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('aria-selected')).toBe('true')
    finishOpen?.()
    await flushPromises()

    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionId)
    expect(wrapper.get('[data-remote-tab="ftp"]').attributes('aria-selected')).toBe('true')
    expect(wrapper.find('.remote-terminal').exists()).toBe(false)
    wrapper.unmount()
  })

  it('requires explicit confirmation before accepting a new SSH host key', async () => {
    const rejectedSession = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    const trustedSession = '019fe096-aeac-7bc1-8077-6e960dbc5589'
    const capabilities = {
      maxOutputChunkBytes: 45_056,
      maxInputChunkBytes: 45_056,
      maxTranscriptBytes: 65_536,
      maxRows: 1_000,
      maxColumns: 1_000,
      maxPixelDimension: 32_767,
      nonblockingOutput: true as const,
      fixedOpenSshProgram: true as const,
    }
    mockedOpenTerminal
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId: rejectedSession,
          capabilities,
          status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
        },
      })
      .mockResolvedValueOnce({
        kind: 'data',
        data: {
          sessionId: trustedSession,
          capabilities,
          status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
        },
      })
    mockedPollTerminal
      .mockResolvedValueOnce({
        kind: 'data',
        data: { state: 'disconnected', detail: 'host_key_unknown', transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      })
      .mockResolvedValue({
        kind: 'data',
        data: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(wrapper.get('.remote-host-key-confirmation').text()).toContain('ssh.local:22')
    expect(wrapper.get('.remote-host-key-confirmation').text()).toContain('密钥变化仍会拒绝连接')
    expect(mockedOpenTerminal).toHaveBeenCalledTimes(1)
    expect(mockedOpenTerminal).toHaveBeenLastCalledWith(sshProfile.profile.id, expect.any(Object), false)

    await wrapper.get('.remote-host-key-confirmation .remote-primary-button').trigger('click')
    await flushPromises()
    expect(mockedCloseTerminal).toHaveBeenCalledWith(rejectedSession)
    expect(mockedOpenTerminal).toHaveBeenLastCalledWith(sshProfile.profile.id, expect.any(Object), true)
    wrapper.unmount()
  })

  it('never offers first-use acceptance for a reject-policy profile', async () => {
    const strictProfile = structuredClone(sshProfile)
    strictProfile.profile.trust = { kind: 'ssh_known_hosts', first_use: 'reject' }
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [strictProfile], nextAfter: null },
    })
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId: '019fe096-aeac-7bc1-8077-6e960dbc5588',
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'disconnected', detail: 'host_key_unknown', transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(wrapper.find('.remote-host-key-confirmation').exists()).toBe(false)
    expect(mockedOpenTerminal).toHaveBeenCalledTimes(1)
    expect(mockedOpenTerminal).toHaveBeenCalledWith(strictProfile.profile.id, expect.any(Object), false)
    wrapper.unmount()
  })

  it('offers an edit path when SSH authentication fails', async () => {
    const sessionId = '019fe096-aeac-7bc1-8077-6e960dbc5588'
    mockedOpenTerminal.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId,
        capabilities: {
          maxOutputChunkBytes: 45_056,
          maxInputChunkBytes: 45_056,
          maxTranscriptBytes: 65_536,
          maxRows: 1_000,
          maxColumns: 1_000,
          maxPixelDimension: 32_767,
          nonblockingOutput: true,
          fixedOpenSshProgram: true,
        },
        status: { state: 'running', detail: null, transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
      },
    })
    mockedPollTerminal.mockResolvedValue({
      kind: 'data',
      data: { state: 'disconnected', detail: 'authentication_failed', transcriptRetainedBytes: 0, transcriptDroppedBytes: 0 },
    })
    mockedReadTerminal.mockResolvedValue({ kind: 'data', data: { status: 'pending' } })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(wrapper.get('.remote-terminal-action').text()).toContain('服务器拒绝了 SSH Agent')
    await wrapper.get('.remote-terminal-action .remote-primary-button').trigger('click')
    await flushPromises()

    expect(mockedCloseTerminal).toHaveBeenCalledWith(sessionId)
    expect(wrapper.get('.remote-form-heading').text()).toContain('编辑 SSH 终端')
    expect((wrapper.get('#remote-profile-authentication').element as HTMLSelectElement).value).toBe('ssh_agent')
    wrapper.unmount()
  })

  it('connects an SMB profile through the shared typed file-session browser', async () => {
    window.location.hash = '#/remote?protocol=smb'
    mockedProfiles.mockResolvedValue({
      kind: 'data',
      data: { profiles: [smbProfile], nextAfter: null },
    })
    mockedConnect.mockResolvedValue({
      kind: 'data',
      data: {
        id: '019fe096-aeac-7bc1-8077-6e960dbc5581',
        profileId: smbProfile.profile.id,
        protocol: 'smb',
        state: 'ready',
        stateReason: null,
        capabilities: adapter('smb').fileOperations,
        openedAtUnixMs: 1,
        updatedAtUnixMs: 1,
      },
    })
    mockedList.mockResolvedValue({
      kind: 'data',
      data: {
        sessionId: '019fe096-aeac-7bc1-8077-6e960dbc5581',
        path: '/',
        offset: 0,
        entries: [],
        nextOffset: null,
      },
    })

    const wrapper = mount(RemoteView)
    await flushPromises()
    await wrapper.get('.remote-workspace .remote-primary-button').trigger('click')
    await flushPromises()

    expect(mockedConnect).toHaveBeenCalledWith(smbProfile.profile.id)
    expect(mockedList).toHaveBeenCalledWith('019fe096-aeac-7bc1-8077-6e960dbc5581', '/', 0)
    expect(wrapper.text()).toContain('目录为空')
    wrapper.unmount()
  })

  it('renders a typed bridge failure and retries both factual requests', async () => {
    mockedCatalog.mockResolvedValue({
      kind: 'error',
      error: {
        kind: 'transport',
        code: 'appd_socket_unavailable',
        reason: 'appd_socket_unavailable',
        retryable: true,
      },
    })
    const wrapper = mount(RemoteView)
    await flushPromises()

    expect(wrapper.text()).toContain('远程服务不可用')
    expect(wrapper.text()).toContain('appd_socket_unavailable')
    await wrapper.get('.remote-workspace-state .remote-secondary-button').trigger('click')
    await flushPromises()
    expect(mockedCatalog).toHaveBeenCalledTimes(2)
    expect(mockedProfiles).toHaveBeenCalledTimes(2)
    wrapper.unmount()
  })
})
