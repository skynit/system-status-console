<script setup lang="ts">
import { computed, markRaw, nextTick, onActivated, onBeforeUnmount, onMounted, reactive, ref, shallowReactive, watch } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import { FitAddon } from '@xterm/addon-fit'
import { Terminal } from '@xterm/xterm'
import type { IDisposable } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'
import {
  CheckCircle2,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  CircleX,
  Download,
  File,
  Folder,
  FolderOpen,
  FolderPlus,
  Info,
  KeyRound,
  LoaderCircle,
  LockKeyhole,
  Plus,
  Pencil,
  RefreshCw,
  Server,
  Share2,
  ShieldCheck,
  SquareTerminal,
  Trash2,
  Unplug,
  Upload,
  X,
} from 'lucide-vue-next'

import {
  closeRemoteTerminal,
  connectRemoteSession,
  createRemoteDirectory,
  deleteRemoteSecret,
  deleteRemoteEntry,
  deleteRemoteProfile,
  disconnectRemoteSession,
  getRemoteAdapterCatalog,
  getRemoteProfiles,
  listRemoteDirectory,
  openRemoteTerminal,
  renameRemoteEntry,
  resizeRemoteTerminal,
  storeRemoteSecret,
  streamRemoteTerminal,
  upsertRemoteProfile,
  writeRemoteTerminal,
} from '../backend'
import type {
  BridgeError,
  RemoteAdapterCatalog,
  RemoteAdapterDescriptor,
  RemoteAuthentication,
  RemoteConnectionProfile,
  RemoteDirectoryPage,
  RemoteOperationCapability,
  RemoteProtocol,
  RemoteSecretKind,
  RemoteSecretReference,
  RemoteSession,
  StoredRemoteProfile,
  TerminalCapabilities,
  TerminalSize,
  TerminalStatus,
} from '../types'

type RemoteTabProtocol = 'ssh' | 'sftp' | 'ftp' | 'ftps_explicit' | 'smb'
type RemoteFormAuthentication = 'ssh_agent' | 'ssh_key' | 'anonymous' | 'password' | 'kerberos'
type SmbMinimumDialect = 'smb2' | 'smb3'
type FtpsTrustMode = 'system_tls' | 'pinned_tls_certificate'
type LoadState = 'loading' | 'ready' | 'error'
type FileAction =
  | { kind: 'create_directory' }
  | { kind: 'rename'; entry: RemoteDirectoryPage['entries'][number] }
  | { kind: 'delete'; entry: RemoteDirectoryPage['entries'][number] }
type FileTabProtocol = Exclude<RemoteTabProtocol, 'ssh'>
type FileWorkspaceState = {
  session: RemoteSession | null
  directoryPage: RemoteDirectoryPage | null
  currentPath: string
  directoryOffsetHistory: number[]
  directoryLoading: boolean
  generation: number
}

type TerminalRuntime = {
  profileId: string
  sessionId: string
  status: TerminalStatus
  capabilities: TerminalCapabilities
  host: HTMLElement | null
  terminal: Terminal | null
  fitAddon: FitAddon | null
  inputSubscription: IDisposable | null
  resizeObserver: ResizeObserver | null
  resizeTimer: number | null
  inputTimer: number | null
  inputChunks: Uint8Array[]
  inputBytes: number
  writeQueue: Promise<void>
  resizeQueue: Promise<void>
  surfaceGeneration: number
  lastSize: string
}

const tabs: Array<{ protocol: RemoteTabProtocol; label: string; icon: typeof SquareTerminal }> = [
  { protocol: 'ssh', label: 'SSH 终端', icon: SquareTerminal },
  { protocol: 'sftp', label: 'SFTP 文件', icon: FolderOpen },
  { protocol: 'ftp', label: 'FTP 文件', icon: FolderOpen },
  { protocol: 'ftps_explicit', label: 'FTPS 文件', icon: LockKeyhole },
  { protocol: 'smb', label: 'SMB2/3', icon: Share2 },
]
const footerProtocols = ['sftp', 'ftp', 'ftps_explicit', 'smb'] as const
const TERMINAL_INPUT_FLUSH_MS = 4
const TERMINAL_SCROLLBACK_LINES = 5_000

function fileWorkspaceState(): FileWorkspaceState {
  return {
    session: null,
    directoryPage: null,
    currentPath: '/',
    directoryOffsetHistory: [],
    directoryLoading: false,
    generation: 0,
  }
}

const activeProtocol = ref<RemoteTabProtocol>(initialProtocolFromHash())
const loadState = ref<LoadState>('loading')
const catalog = ref<RemoteAdapterCatalog | null>(null)
const profiles = ref<StoredRemoteProfile[]>([])
const selectedProfileIds = reactive<Record<RemoteTabProtocol, string | null>>({
  ssh: null,
  sftp: null,
  ftp: null,
  ftps_explicit: null,
  smb: null,
})
const selectedProfileId = computed<string | null>({
  get: () => selectedProfileIds[activeProtocol.value],
  set: (profileId) => { selectedProfileIds[activeProtocol.value] = profileId },
})
const loadError = ref<BridgeError | null>(null)
const operationError = ref<BridgeError | null>(null)
const refreshing = ref(false)
const showProfileForm = ref(false)
const editingProfile = ref<StoredRemoteProfile | null>(null)
const savingProfile = ref(false)
const deletingProfile = ref(false)
const cleaningSecrets = ref(false)
const profileFormDirty = ref(false)
const formLabel = ref('')
const formHost = ref('')
const formPort = ref(22)
const formUsername = ref('')
const formAuthentication = ref<RemoteFormAuthentication>('ssh_agent')
const formPassword = ref('')
const formPrivateKey = ref('')
const formKeyPassphrase = ref('')
const formRemoveKeyPassphrase = ref(false)
const formSshAskFirstUse = ref(true)
const formDomain = ref('')
const formShare = ref('')
const formSmbMinimumDialect = ref<SmbMinimumDialect>('smb3')
const formSmbRequireSigning = ref(true)
const formSmbRequireEncryption = ref(true)
const formPlainFtpAcknowledged = ref(false)
const formFtpsTrustMode = ref<FtpsTrustMode>('system_tls')
const formFtpsCertificatePem = ref('')

const connecting = ref(false)
const fileWorkspaces = reactive<Record<FileTabProtocol, FileWorkspaceState>>({
  sftp: fileWorkspaceState(),
  ftp: fileWorkspaceState(),
  ftps_explicit: fileWorkspaceState(),
  smb: fileWorkspaceState(),
})
const activeFileWorkspace = computed(() => (
  activeProtocol.value === 'ssh' ? null : fileWorkspaces[activeProtocol.value]
))
const fileSession = computed<RemoteSession | null>({
  get: () => activeFileWorkspace.value?.session ?? null,
  set: (session) => { if (activeFileWorkspace.value) activeFileWorkspace.value.session = session },
})
const directoryPage = computed<RemoteDirectoryPage | null>({
  get: () => activeFileWorkspace.value?.directoryPage ?? null,
  set: (page) => { if (activeFileWorkspace.value) activeFileWorkspace.value.directoryPage = page },
})
const currentPath = computed<string>({
  get: () => activeFileWorkspace.value?.currentPath ?? '/',
  set: (path) => { if (activeFileWorkspace.value) activeFileWorkspace.value.currentPath = path },
})
const directoryOffsetHistory = computed<number[]>({
  get: () => activeFileWorkspace.value?.directoryOffsetHistory ?? [],
  set: (history) => { if (activeFileWorkspace.value) activeFileWorkspace.value.directoryOffsetHistory = history },
})
const directoryLoading = computed<boolean>({
  get: () => activeFileWorkspace.value?.directoryLoading ?? false,
  set: (loading) => { if (activeFileWorkspace.value) activeFileWorkspace.value.directoryLoading = loading },
})
const fileAction = ref<FileAction | null>(null)
const fileActionValue = ref('')
const fileMutationBusy = ref(false)
const terminalRuntimes = reactive(new Map<string, TerminalRuntime>())
const remoteWorkspace = ref<HTMLElement | null>(null)
let active = true
let requestGeneration = 0
let protocolGeneration = 0
let mutatingProfileForm = false
const pendingSecretCleanup = ref<RemoteSecretReference[]>([])

const initialTerminalSize: TerminalSize = {
  rows: 24,
  columns: 80,
  pixelWidth: 0,
  pixelHeight: 0,
}

const activeAdapter = computed(() =>
  catalog.value?.adapters.find((adapter) => adapter.protocol === activeProtocol.value) ?? null,
)
const visibleProfiles = computed(() =>
  profiles.value.filter((stored) => stored.profile.protocol === activeProtocol.value),
)
const selectedProfile = computed(() =>
  visibleProfiles.value.find((stored) => stored.profile.id === selectedProfileId.value) ?? null,
)
const terminalRuntimeList = computed(() => [...terminalRuntimes.values()])
const currentTerminalRuntime = computed(() => (
  selectedProfileIds.ssh ? terminalRuntimes.get(selectedProfileIds.ssh) ?? null : null
))
const adapterOperable = computed(() => {
  const status = activeAdapter.value?.availability.status
  return status === 'healthy' || status === 'degraded'
})
const activeReason = computed(() =>
  activeAdapter.value?.availability.capabilityReason ?? loadError.value?.reason ?? 'remote_catalog_pending',
)
const supportedFileOperations = computed(() =>
  activeAdapter.value?.fileOperations.filter((capability) => capability.status === 'supported').length ?? 0,
)
const sshForm = computed(() => activeProtocol.value === 'ssh' || activeProtocol.value === 'sftp')
const ftpForm = computed(() => activeProtocol.value === 'ftp' || activeProtocol.value === 'ftps_explicit')
const smbForm = computed(() => activeProtocol.value === 'smb')
const passwordForm = computed(() => formAuthentication.value === 'password')
const privateKeyForm = computed(() => formAuthentication.value === 'ssh_key')
const usernameRequired = computed(() => passwordForm.value)
const canReuseExistingPassword = computed(() => (
  editingProfile.value?.profile.authentication.method === 'password'
  && formAuthentication.value === 'password'
))
const canReuseExistingPrivateKey = computed(() => (
  editingProfile.value?.profile.authentication.method === 'ssh_key'
  && formAuthentication.value === 'ssh_key'
))
const canReuseExistingKeyPassphrase = computed(() => (
  editingProfile.value?.profile.authentication.method === 'ssh_key'
  && editingProfile.value.profile.authentication.passphrase !== null
  && formAuthentication.value === 'ssh_key'
))
const authenticationFact = computed(() => {
  if (formAuthentication.value === 'ssh_agent') return 'SSH Agent · strict known_hosts'
  if (formAuthentication.value === 'ssh_key') return '私钥和可选口令 · Secret Service · strict known_hosts'
  if (formAuthentication.value === 'kerberos') return 'Kerberos · 系统凭据缓存 · SMB 协商'
  if (formAuthentication.value === 'password') {
    if (activeProtocol.value === 'smb') return '用户名和密码 · Secret Service · SMB 协商'
    if (sshForm.value) return '用户名和密码 · sealed askpass · strict known_hosts'
    return activeProtocol.value === 'ftp'
      ? '用户名和密码 · Secret Service · 明文传输'
      : '用户名和密码 · Secret Service · System TLS'
  }
  return activeProtocol.value === 'ftp' ? '匿名认证 · 明文传输' : '匿名认证 · System TLS'
})

watch([
  formLabel,
  formHost,
  formPort,
  formUsername,
  formAuthentication,
  formPassword,
  formPrivateKey,
  formKeyPassphrase,
  formRemoveKeyPassphrase,
  formSshAskFirstUse,
  formDomain,
  formShare,
  formSmbMinimumDialect,
  formSmbRequireSigning,
  formSmbRequireEncryption,
  formPlainFtpAcknowledged,
  formFtpsTrustMode,
  formFtpsCertificatePem,
], () => {
  if (showProfileForm.value && !mutatingProfileForm) profileFormDirty.value = true
}, { flush: 'sync' })

watch(visibleProfiles, (next) => {
  if (!next.some((stored) => stored.profile.id === selectedProfileId.value)) {
    selectedProfileId.value = next[0]?.profile.id ?? null
  }
}, { immediate: true })

watch(selectedProfileId, async (profileId) => {
  if (activeProtocol.value !== 'ssh' || !profileId) return
  await nextTick()
  revealSelectedProfile(profileId)
})

watch(formKeyPassphrase, (value) => {
  if (value) formRemoveKeyPassphrase.value = false
})

function protocolLabel(protocol: RemoteProtocol): string {
  return tabs.find((tab) => tab.protocol === protocol)?.label ?? protocol
}

function initialProtocolFromHash(): RemoteTabProtocol {
  const query = window.location.hash.split('?', 2)[1]
  const protocol = query ? new URLSearchParams(query).get('protocol') : null
  return tabs.some((tab) => tab.protocol === protocol) ? protocol as RemoteTabProtocol : 'ssh'
}

function statusLabel(status: string): string {
  return ({
    healthy: '可用',
    degraded: '降级',
    unsupported: '不支持',
    unreachable: '不可达',
  } as Record<string, string>)[status] ?? status
}

function statusIcon(adapter: RemoteAdapterDescriptor | null) {
  if (!adapter) return CircleAlert
  if (adapter.availability.status === 'healthy') return CheckCircle2
  if (adapter.availability.status === 'unsupported') return CircleX
  return CircleAlert
}

function defaultPort(protocol: RemoteTabProtocol): number {
  return protocol === 'ssh' || protocol === 'sftp' ? 22 : protocol === 'smb' ? 445 : 21
}

function resetProfileForm(): void {
  mutatingProfileForm = true
  formLabel.value = ''
  formHost.value = ''
  formPort.value = defaultPort(activeProtocol.value)
  formUsername.value = ''
  formAuthentication.value = activeProtocol.value === 'ssh' || activeProtocol.value === 'sftp'
    ? 'ssh_agent'
    : activeProtocol.value === 'smb'
      ? 'password'
      : 'anonymous'
  formPassword.value = ''
  formPrivateKey.value = ''
  formKeyPassphrase.value = ''
  formRemoveKeyPassphrase.value = false
  formSshAskFirstUse.value = activeProtocol.value === 'ssh'
  formDomain.value = ''
  formShare.value = ''
  formSmbMinimumDialect.value = 'smb3'
  formSmbRequireSigning.value = true
  formSmbRequireEncryption.value = true
  formPlainFtpAcknowledged.value = false
  formFtpsTrustMode.value = 'system_tls'
  formFtpsCertificatePem.value = ''
  operationError.value = null
  profileFormDirty.value = false
  mutatingProfileForm = false
}

function focusProfileLabel(): void {
  void nextTick(() => document.querySelector<HTMLInputElement>('#remote-profile-label')?.focus())
}

function openProfileForm(): void {
  if (!confirmDiscardProfileChanges()) return
  editingProfile.value = null
  resetProfileForm()
  showProfileForm.value = true
  focusProfileLabel()
}

function openEditProfileForm(stored: StoredRemoteProfile, discardConfirmed = false): void {
  if (!discardConfirmed && !confirmDiscardProfileChanges()) return
  const { profile } = stored
  mutatingProfileForm = true
  editingProfile.value = stored
  formLabel.value = profile.label
  formHost.value = profile.endpoint.host
  formPort.value = profile.endpoint.port
  formUsername.value = profile.username ?? ''
  formAuthentication.value = profile.authentication.method
  formPassword.value = ''
  formPrivateKey.value = ''
  formKeyPassphrase.value = ''
  formRemoveKeyPassphrase.value = false
  formSshAskFirstUse.value = profile.protocol === 'ssh'
    && profile.trust.kind === 'ssh_known_hosts'
    && profile.trust.first_use === 'ask_user'
  formDomain.value = profile.domain ?? ''
  formShare.value = profile.options.protocol === 'smb' ? profile.options.share ?? '' : ''
  formSmbMinimumDialect.value = profile.options.protocol === 'smb' ? profile.options.minimum_dialect : 'smb3'
  formSmbRequireSigning.value = profile.options.protocol === 'smb' ? profile.options.require_signing : true
  formSmbRequireEncryption.value = profile.options.protocol === 'smb' ? profile.options.require_encryption : true
  formPlainFtpAcknowledged.value = profile.protocol === 'ftp'
  formFtpsTrustMode.value = profile.trust.kind === 'pinned_tls_certificate'
    ? 'pinned_tls_certificate'
    : 'system_tls'
  formFtpsCertificatePem.value = profile.trust.kind === 'pinned_tls_certificate'
    ? profile.trust.certificate_pem
    : ''
  operationError.value = null
  selectedProfileId.value = profile.id
  showProfileForm.value = true
  profileFormDirty.value = false
  mutatingProfileForm = false
  focusProfileLabel()
}

function closeProfileForm(): void {
  if (!confirmDiscardProfileChanges()) return
  mutatingProfileForm = true
  formPassword.value = ''
  formPrivateKey.value = ''
  formKeyPassphrase.value = ''
  formRemoveKeyPassphrase.value = false
  editingProfile.value = null
  showProfileForm.value = false
  profileFormDirty.value = false
  mutatingProfileForm = false
}

function confirmDiscardProfileChanges(): boolean {
  if (savingProfile.value || deletingProfile.value || fileAction.value || fileMutationBusy.value) return false
  return !profileFormDirty.value || window.confirm('当前连接配置还有未保存的修改，确定放弃吗？')
}

function handleBeforeUnload(event: BeforeUnloadEvent): void {
  if (!profileFormDirty.value && !fileAction.value && !fileMutationBusy.value) return
  event.preventDefault()
  event.returnValue = ''
}

function inputError(reason: string): BridgeError {
  return {
    kind: 'protocol',
    code: reason,
    reason,
    retryable: false,
  }
}

function profilePayload(
  authentication: RemoteAuthentication,
  existing: StoredRemoteProfile | null,
): RemoteConnectionProfile | null {
  const label = formLabel.value.trim()
  const host = formHost.value.trim()
  const username = formUsername.value.trim() || null
  if (!label || !host || formPort.value < 1 || formPort.value > 65535) return null
  if (host.includes('://') || /[\s/@\\]/u.test(host)) return null
  if (usernameRequired.value && !username) return null
  const existingProfile = existing?.profile.protocol === activeProtocol.value ? existing.profile : null
  const id = existingProfile?.id ?? crypto.randomUUID()
  if (activeProtocol.value === 'ssh') {
    return {
      id,
      label,
      protocol: 'ssh',
      endpoint: { host, port: formPort.value },
      username,
      domain: null,
      authentication,
      trust: {
        kind: 'ssh_known_hosts',
        first_use: formSshAskFirstUse.value ? 'ask_user' : 'reject',
      },
      options: existingProfile?.options.protocol === 'ssh'
        ? existingProfile.options
        : { protocol: 'ssh', jump_profiles: [], agent_forwarding: false },
    }
  }
  if (activeProtocol.value === 'sftp') {
    return {
      id,
      label,
      protocol: 'sftp',
      endpoint: { host, port: formPort.value },
      username,
      domain: null,
      authentication,
      trust: existingProfile?.trust.kind === 'ssh_known_hosts'
        ? existingProfile.trust
        : { kind: 'ssh_known_hosts', first_use: 'reject' },
      options: existingProfile?.options.protocol === 'sftp'
        ? existingProfile.options
        : { protocol: 'sftp', jump_profiles: [] },
    }
  }
  if (activeProtocol.value === 'smb') {
    const share = formShare.value.trim()
    if (!share || /[\\/]/u.test(share)) return null
    return {
      id,
      label,
      protocol: 'smb',
      endpoint: { host, port: formPort.value },
      username,
      domain: formAuthentication.value === 'kerberos' ? null : formDomain.value.trim() || null,
      authentication,
      trust: existingProfile?.trust.kind === 'smb_negotiated'
        ? existingProfile.trust
        : { kind: 'smb_negotiated' },
      options: {
        protocol: 'smb',
        share,
        minimum_dialect: formSmbMinimumDialect.value,
        require_signing: formSmbRequireSigning.value,
        require_encryption: formSmbRequireEncryption.value,
      },
    }
  }
  if (activeProtocol.value === 'ftps_explicit') {
    const certificatePem = formFtpsCertificatePem.value.trim()
    if (
      formFtpsTrustMode.value === 'pinned_tls_certificate'
      && !validPinnedCertificatePem(certificatePem)
    ) return null
    return {
      id,
      label,
      protocol: 'ftps_explicit',
      endpoint: { host, port: formPort.value },
      username,
      domain: null,
      authentication,
      trust: formFtpsTrustMode.value === 'pinned_tls_certificate'
        ? { kind: 'pinned_tls_certificate', certificate_pem: `${certificatePem}\n` }
        : { kind: 'system_tls' },
      options: existingProfile?.options.protocol === 'ftps_explicit'
        ? existingProfile.options
        : {
            protocol: 'ftps_explicit',
            data_connection: 'passive',
            require_protected_data_channel: true,
          },
    }
  }
  if (activeProtocol.value === 'ftp' && formPlainFtpAcknowledged.value) {
    return {
      id,
      label,
      protocol: 'ftp',
      endpoint: { host, port: formPort.value },
      username,
      domain: null,
      authentication,
      trust: existingProfile?.trust.kind === 'plaintext_acknowledged'
        ? existingProfile.trust
        : { kind: 'plaintext_acknowledged' },
      options: existingProfile?.options.protocol === 'ftp'
        ? existingProfile.options
        : { protocol: 'ftp', data_connection: 'passive' },
    }
  }
  return null
}

function validPinnedCertificatePem(value: string): boolean {
  if (new TextEncoder().encode(value).byteLength > 16 * 1024) return false
  if (!value.startsWith('-----BEGIN CERTIFICATE-----') || !value.endsWith('-----END CERTIFICATE-----')) return false
  if ((value.match(/-----BEGIN CERTIFICATE-----/gu) ?? []).length !== 1) return false
  if ((value.match(/-----END CERTIFICATE-----/gu) ?? []).length !== 1) return false
  return /^[A-Za-z0-9+/=\- \r\n]+$/u.test(value)
}

async function storeFormSecret(
  kind: RemoteSecretKind,
  value: string,
): Promise<{ reference: RemoteSecretReference | null; error: BridgeError | null }> {
  const bytes = new TextEncoder().encode(value)
  try {
    const result = await storeRemoteSecret(kind, bytes)
    return result.kind === 'data'
      ? { reference: result.data, error: null }
      : { reference: null, error: result.error }
  } finally {
    bytes.fill(0)
  }
}

async function cleanupSecretReferences(references: RemoteSecretReference[]): Promise<BridgeError | null> {
  const attemptedItemIds = new Set(references.map((reference) => reference.item_id))
  const failed: RemoteSecretReference[] = []
  let firstError: BridgeError | null = null
  for (const reference of [...references].reverse()) {
    const result = await deleteRemoteSecret(reference)
    if (result.kind === 'error') {
      failed.push(reference)
      firstError ??= result.error
    }
  }
  const retained = pendingSecretCleanup.value.filter((reference) => !attemptedItemIds.has(reference.item_id))
  pendingSecretCleanup.value = [...retained, ...failed.reverse()].filter((reference, index, all) =>
    all.findIndex((candidate) => candidate.item_id === reference.item_id) === index,
  )
  return firstError
}

async function retryPendingSecretCleanup(): Promise<void> {
  if (cleaningSecrets.value || pendingSecretCleanup.value.length === 0) return
  cleaningSecrets.value = true
  const cleanupError = await cleanupSecretReferences(pendingSecretCleanup.value)
  cleaningSecrets.value = false
  if (active) operationError.value = cleanupError
}

async function authenticationPayload(
  existingAuthentication: RemoteAuthentication | null,
): Promise<{
  authentication: RemoteAuthentication | null
  references: RemoteSecretReference[]
  error: BridgeError | null
}> {
  if (formAuthentication.value === 'ssh_agent') {
    return { authentication: { method: 'ssh_agent' }, references: [], error: null }
  }
  if (formAuthentication.value === 'anonymous') {
    return { authentication: { method: 'anonymous' }, references: [], error: null }
  }
  if (formAuthentication.value === 'kerberos') {
    return { authentication: { method: 'kerberos' }, references: [], error: null }
  }
  if (formAuthentication.value === 'password') {
    if (!formUsername.value.trim()) {
      return { authentication: null, references: [], error: inputError('remote_password_credentials_required') }
    }
    if (!formPassword.value && existingAuthentication?.method === 'password') {
      return { authentication: existingAuthentication, references: [], error: null }
    }
    if (!formPassword.value) {
      return { authentication: null, references: [], error: inputError('remote_password_credentials_required') }
    }
    const stored = await storeFormSecret('password', formPassword.value)
    return stored.reference
      ? { authentication: { method: 'password', secret: stored.reference }, references: [stored.reference], error: null }
      : { authentication: null, references: [], error: stored.error }
  }
  if (
    !formPrivateKey.value.trim()
    && existingAuthentication?.method === 'ssh_key'
    && !formKeyPassphrase.value
    && !formRemoveKeyPassphrase.value
  ) {
    return { authentication: existingAuthentication, references: [], error: null }
  }
  const privateKey = formPrivateKey.value.trim()
    ? await storeFormSecret('private_key', formPrivateKey.value)
    : { reference: existingAuthentication?.method === 'ssh_key' ? existingAuthentication.private_key : null, error: null }
  if (!privateKey.reference) {
    return { authentication: null, references: [], error: privateKey.error ?? inputError('remote_private_key_required') }
  }
  const references = formPrivateKey.value.trim() ? [privateKey.reference] : []
  let passphrase = !formPrivateKey.value.trim() && existingAuthentication?.method === 'ssh_key'
    ? existingAuthentication.passphrase
    : null
  if (formRemoveKeyPassphrase.value) passphrase = null
  if (formKeyPassphrase.value) {
    const storedPassphrase = await storeFormSecret('key_passphrase', formKeyPassphrase.value)
    if (!storedPassphrase.reference) {
      const cleanupError = await cleanupSecretReferences(references)
      return { authentication: null, references: [], error: cleanupError ?? storedPassphrase.error }
    }
    passphrase = storedPassphrase.reference
    references.push(storedPassphrase.reference)
  }
  return {
    authentication: { method: 'ssh_key', private_key: privateKey.reference, passphrase },
    references,
    error: null,
  }
}

function authenticationReferences(authentication: RemoteAuthentication): RemoteSecretReference[] {
  if (authentication.method === 'password') return [authentication.secret]
  if (authentication.method === 'ssh_key') {
    return authentication.passphrase
      ? [authentication.private_key, authentication.passphrase]
      : [authentication.private_key]
  }
  return []
}

function profileHasActiveSession(profileId: string): boolean {
  return terminalRuntimes.has(profileId)
    || Object.values(fileWorkspaces).some((workspace) => workspace.session?.profileId === profileId)
    || (connecting.value && selectedProfileId.value === profileId)
}

async function removeEditedProfile(): Promise<void> {
  const stored = editingProfile.value
  if (!stored || refreshing.value || savingProfile.value || deletingProfile.value) return
  if (profileHasActiveSession(stored.profile.id)) {
    operationError.value = inputError('remote_profile_session_active')
    return
  }
  if (!window.confirm(`确定删除连接配置“${stored.profile.label}”吗？`)) return

  deletingProfile.value = true
  operationError.value = null
  const result = await deleteRemoteProfile(stored.profile.id, stored.revision)
  if (result.kind === 'error') {
    deletingProfile.value = false
    if (active) operationError.value = result.error
    return
  }

  profiles.value = profiles.value.filter((profile) => profile.profile.id !== stored.profile.id)
  if (editingProfile.value?.profile.id === stored.profile.id) {
    mutatingProfileForm = true
    showProfileForm.value = false
    formPassword.value = ''
    formPrivateKey.value = ''
    formKeyPassphrase.value = ''
    formRemoveKeyPassphrase.value = false
    editingProfile.value = null
    profileFormDirty.value = false
    mutatingProfileForm = false
  }
  const cleanupError = await cleanupSecretReferences(authenticationReferences(stored.profile.authentication))
  deletingProfile.value = false
  if (active && cleanupError) operationError.value = cleanupError
}

async function saveProfile(): Promise<void> {
  if (refreshing.value || savingProfile.value || deletingProfile.value) return
  const editedProfile = editingProfile.value
  savingProfile.value = true
  operationError.value = null
  if (pendingSecretCleanup.value.length > 0) {
    const cleanupError = await cleanupSecretReferences(pendingSecretCleanup.value)
    if (cleanupError) {
      savingProfile.value = false
      if (active) operationError.value = cleanupError
      return
    }
  }
  const authenticationResult = await authenticationPayload(editedProfile?.profile.authentication ?? null)
  if (!authenticationResult.authentication) {
    savingProfile.value = false
    if (active) operationError.value = authenticationResult.error ?? inputError('remote_profile_input_invalid')
    return
  }
  const profile = profilePayload(authenticationResult.authentication, editedProfile)
  if (!profile) {
    const cleanupError = await cleanupSecretReferences(authenticationResult.references)
    savingProfile.value = false
    if (active) operationError.value = cleanupError ?? inputError('remote_profile_input_invalid')
    return
  }
  const result = editedProfile
    ? await upsertRemoteProfile(profile, editedProfile.revision)
    : await upsertRemoteProfile(profile)
  if (result.kind === 'error') {
    const cleanupError = await cleanupSecretReferences(authenticationResult.references)
    savingProfile.value = false
    if (active) operationError.value = cleanupError ?? result.error
    return
  }
  let cleanupError: BridgeError | null = null
  if (editedProfile) {
    const retainedReferences = new Set(authenticationReferences(result.data.profile.authentication)
      .map((reference) => reference.item_id))
    const obsoleteReferences = authenticationReferences(editedProfile.profile.authentication)
      .filter((reference) => !retainedReferences.has(reference.item_id))
    if (obsoleteReferences.length > 0) cleanupError = await cleanupSecretReferences(obsoleteReferences)
  }
  savingProfile.value = false
  if (!active) return
  mutatingProfileForm = true
  showProfileForm.value = false
  formPassword.value = ''
  formPrivateKey.value = ''
  formKeyPassphrase.value = ''
  formRemoveKeyPassphrase.value = false
  profiles.value = [...profiles.value.filter((stored) => stored.profile.id !== result.data.profile.id), result.data]
    .sort((left, right) => left.profile.id.localeCompare(right.profile.id))
  selectedProfileId.value = result.data.profile.id
  editingProfile.value = null
  profileFormDirty.value = false
  mutatingProfileForm = false
  if (cleanupError) operationError.value = cleanupError
}

async function loadRemoteFacts(): Promise<void> {
  if (refreshing.value || savingProfile.value || deletingProfile.value || fileAction.value || fileMutationBusy.value) return
  const generation = ++requestGeneration
  const hadReadyFacts = loadState.value === 'ready' && catalog.value !== null
  refreshing.value = true
  if (!catalog.value) loadState.value = 'loading'
  const [catalogResult, profilesResult] = await Promise.all([
    getRemoteAdapterCatalog(),
    getRemoteProfiles(),
  ])
  if (!active || generation !== requestGeneration) return
  refreshing.value = false
  const refreshError = catalogResult.kind === 'error'
    ? catalogResult.error
    : profilesResult.kind === 'error'
      ? profilesResult.error
      : null
  if (refreshError && hadReadyFacts) {
    loadError.value = refreshError
    operationError.value = refreshError
    return
  }
  if (catalogResult.kind === 'error') {
    catalog.value = null
    profiles.value = []
    loadError.value = catalogResult.error
    loadState.value = 'error'
    return
  }
  catalog.value = catalogResult.data
  if (profilesResult.kind === 'error') {
    profiles.value = []
    loadError.value = profilesResult.error
    loadState.value = 'error'
    return
  }
  profiles.value = profilesResult.data.profiles
  if (operationError.value === loadError.value) operationError.value = null
  loadError.value = null
  loadState.value = 'ready'
}

async function closeCurrentSession(): Promise<void> {
  if (activeProtocol.value === 'ssh' && selectedProfileId.value) {
    await closeTerminalRuntime(selectedProfileId.value)
    return
  }
  await closeFileSession()
}

async function closeFileSession(): Promise<void> {
  const workspace = activeFileWorkspace.value
  if (!workspace) return
  workspace.generation += 1
  const session = workspace.session
  workspace.session = null
  workspace.directoryPage = null
  workspace.currentPath = '/'
  workspace.directoryOffsetHistory = []
  workspace.directoryLoading = false
  fileAction.value = null
  fileActionValue.value = ''
  fileMutationBusy.value = false
  if (session) {
    const result = await disconnectRemoteSession(session.id)
    if (active && result.kind === 'error') operationError.value = result.error
  }
}

function selectProfile(profileId: string): void {
  if (fileAction.value || fileMutationBusy.value) return
  selectedProfileId.value = profileId
  operationError.value = null
  if (activeProtocol.value !== 'ssh' && fileSession.value?.profileId !== profileId) {
    protocolGeneration += 1
    connecting.value = false
    void closeFileSession()
  }
  void nextTick(() => revealSelectedProfile(profileId))
}

function selectProtocol(protocol: RemoteTabProtocol): boolean {
  if (protocol === activeProtocol.value) return true
  if (fileAction.value || fileMutationBusy.value) return false
  if (!confirmDiscardProfileChanges()) return false
  protocolGeneration += 1
  activeProtocol.value = protocol
  connecting.value = false
  editingProfile.value = null
  showProfileForm.value = false
  profileFormDirty.value = false
  operationError.value = null
  if (protocol === 'ssh') {
    void nextTick(() => {
      if (selectedProfileId.value) revealSelectedProfile(selectedProfileId.value)
    })
  } else {
    const workspace = activeFileWorkspace.value
    if (workspace?.session) {
      void loadDirectory(workspace.currentPath, workspace.directoryPage?.offset ?? 0)
    }
  }
  return true
}

function onTabKeydown(event: KeyboardEvent): void {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const current = tabs.findIndex((tab) => tab.protocol === activeProtocol.value)
  const next = event.key === 'Home'
    ? 0
    : event.key === 'End'
      ? tabs.length - 1
      : (current + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length
  if (!selectProtocol(tabs[next].protocol)) return
  nextTick(() => {
    document.querySelector<HTMLButtonElement>(`[data-remote-tab="${tabs[next].protocol}"]`)?.focus()
  })
}

async function connectSelected(acceptNewHostKey = false): Promise<void> {
  if (!selectedProfile.value || !adapterOperable.value) return
  const profile = selectedProfile.value.profile
  const generation = protocolGeneration
  operationError.value = null
  connecting.value = true
  if (activeProtocol.value === 'ssh') {
    const result = await openRemoteTerminal(profile.id, initialTerminalSize, acceptNewHostKey)
    if (generation === protocolGeneration) connecting.value = false
    if (!active || generation !== protocolGeneration || activeProtocol.value !== 'ssh') {
      if (result.kind === 'data') await closeRemoteTerminal(result.data.sessionId)
      return
    }
    if (result.kind === 'error') {
      operationError.value = result.error
      return
    }
    const runtime = shallowReactive<TerminalRuntime>({
      profileId: profile.id,
      sessionId: result.data.sessionId,
      status: result.data.status,
      capabilities: result.data.capabilities,
      host: null,
      terminal: null,
      fitAddon: null,
      inputSubscription: null,
      resizeObserver: null,
      resizeTimer: null,
      inputTimer: null,
      inputChunks: [],
      inputBytes: 0,
      writeQueue: Promise.resolve(),
      resizeQueue: Promise.resolve(),
      surfaceGeneration: 0,
      lastSize: '',
    })
    terminalRuntimes.set(profile.id, runtime)
    await nextTick()
    mountTerminalSurface(runtime)
    void runTerminalStream(runtime)
    await nextTick()
    revealTerminal(runtime)
    return
  }
  const result = await connectRemoteSession(selectedProfile.value.profile.id)
  if (generation === protocolGeneration) connecting.value = false
  if (
    !active
    || generation !== protocolGeneration
    || activeProtocol.value !== profile.protocol
    || selectedProfileId.value !== profile.id
  ) {
    if (result.kind === 'data') await disconnectRemoteSession(result.data.id)
    return
  }
  if (result.kind === 'error') {
    operationError.value = result.error
    return
  }
  fileSession.value = result.data
  directoryOffsetHistory.value = []
  await loadDirectory('/', 0)
}

async function loadDirectory(path: string, offset = 0): Promise<boolean> {
  const workspace = activeFileWorkspace.value
  const session = workspace?.session
  if (!workspace || !session) return false
  const generation = ++workspace.generation
  workspace.directoryLoading = true
  operationError.value = null
  const result = await listRemoteDirectory(session.id, path, offset)
  if (!active || generation !== workspace.generation || workspace.session?.id !== session.id) return false
  workspace.directoryLoading = false
  if (result.kind === 'error') {
    if (result.error.reason === 'remote_session_not_found') {
      workspace.session = null
      workspace.directoryPage = null
      workspace.currentPath = '/'
      workspace.directoryOffsetHistory = []
    }
    if (activeFileWorkspace.value === workspace) operationError.value = result.error
    return false
  }
  workspace.currentPath = path
  workspace.directoryPage = result.data
  if (activeFileWorkspace.value === workspace) {
    fileAction.value = null
    fileActionValue.value = ''
  }
  return true
}

async function navigateDirectory(path: string): Promise<void> {
  if (fileAction.value || fileMutationBusy.value) return
  const workspace = activeFileWorkspace.value
  if (workspace && await loadDirectory(path, 0)) workspace.directoryOffsetHistory = []
}

async function nextDirectoryPage(): Promise<void> {
  const page = directoryPage.value
  if (!page || page.nextOffset === null || directoryLoading.value || fileAction.value || fileMutationBusy.value) return
  const workspace = activeFileWorkspace.value
  if (!workspace) return
  if (await loadDirectory(currentPath.value, page.nextOffset)) {
    workspace.directoryOffsetHistory.push(page.offset)
  }
}

async function previousDirectoryPage(): Promise<void> {
  const previous = directoryOffsetHistory.value.at(-1)
  if (previous === undefined || directoryLoading.value || fileAction.value || fileMutationBusy.value) return
  const workspace = activeFileWorkspace.value
  if (workspace && await loadDirectory(currentPath.value, previous)) workspace.directoryOffsetHistory.pop()
}

function supportsFileOperation(operation: RemoteOperationCapability['operation']): boolean {
  return fileSession.value?.capabilities.some((capability) => (
    capability.operation === operation && capability.status === 'supported'
  )) ?? false
}

function entrySupports(entry: RemoteDirectoryPage['entries'][number], operation: RemoteOperationCapability['operation']): boolean {
  return entry.capabilities.some((capability) => (
    capability.operation === operation && capability.status === 'supported'
  ))
}

function beginCreateDirectory(): void {
  if (directoryLoading.value || fileAction.value || fileMutationBusy.value) return
  fileAction.value = { kind: 'create_directory' }
  fileActionValue.value = ''
  operationError.value = null
}

function beginRename(entry: RemoteDirectoryPage['entries'][number]): void {
  if (directoryLoading.value || fileAction.value || fileMutationBusy.value) return
  fileAction.value = { kind: 'rename', entry }
  fileActionValue.value = entry.name
  operationError.value = null
}

function beginDelete(entry: RemoteDirectoryPage['entries'][number]): void {
  if (directoryLoading.value || fileAction.value || fileMutationBusy.value) return
  fileAction.value = { kind: 'delete', entry }
  fileActionValue.value = ''
  operationError.value = null
}

function cancelFileAction(): void {
  if (fileMutationBusy.value) return
  fileAction.value = null
  fileActionValue.value = ''
}

function validRemoteName(value: string): boolean {
  const name = value.trim()
  return name.length > 0
    && name !== '.'
    && name !== '..'
    && !name.includes('/')
    && !name.includes('\0')
    && new TextEncoder().encode(name).byteLength <= 255
}

function childRemotePath(parent: string, name: string): string {
  return parent === '/' ? `/${name}` : `${parent.replace(/\/+$/u, '')}/${name}`
}

function openTransfer(direction: 'upload' | 'download', path: string): void {
  if (!fileSession.value || fileAction.value || fileMutationBusy.value) return
  const query = new URLSearchParams({
    direction,
    profile: fileSession.value.profileId,
    path,
  })
  window.location.hash = `#/transfers?${query.toString()}`
}

async function submitFileAction(): Promise<void> {
  const action = fileAction.value
  const session = fileSession.value
  if (!action || !session || directoryLoading.value || fileMutationBusy.value) return
  const name = fileActionValue.value.trim()
  if (action.kind !== 'delete' && !validRemoteName(name)) {
    operationError.value = terminalProtocolError('remote_entry_name_invalid')
    return
  }
  const mutationPath = currentPath.value
  fileMutationBusy.value = true
  operationError.value = null
  const result = action.kind === 'create_directory'
    ? await createRemoteDirectory(session.id, childRemotePath(mutationPath, name))
    : action.kind === 'rename'
      ? await renameRemoteEntry(session.id, action.entry.path, childRemotePath(mutationPath, name))
      : await deleteRemoteEntry(session.id, action.entry.path)
  fileMutationBusy.value = false
  if (!active || fileSession.value?.id !== session.id || currentPath.value !== mutationPath) return
  if (result.kind === 'error') {
    operationError.value = result.error
    return
  }
  fileAction.value = null
  fileActionValue.value = ''
  if (await loadDirectory(mutationPath, 0)) directoryOffsetHistory.value = []
}

function parentPath(path: string): string {
  if (path === '/') return '/'
  const normalized = path.replace(/\/+$/u, '')
  const parent = normalized.slice(0, normalized.lastIndexOf('/'))
  return parent || '/'
}

function formatSize(value: number | null): string {
  if (value === null) return 'unknown'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let size = value
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit += 1
  }
  return `${size.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`
}

function decodeTerminalData(value: string): Uint8Array | null {
  try {
    return Uint8Array.from(atob(value), (character) => character.charCodeAt(0))
  } catch {
    return null
  }
}

function encodeTerminalData(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary)
}

function terminalProtocolError(reason: string): BridgeError {
  return { kind: 'protocol', code: reason, reason, retryable: false }
}

function runtimeIsCurrent(runtime: TerminalRuntime, generation = runtime.surfaceGeneration): boolean {
  return active
    && terminalRuntimes.get(runtime.profileId) === runtime
    && runtime.surfaceGeneration === generation
}

function queueTerminalInput(runtime: TerminalRuntime, value: string, generation: number): void {
  if (!value || runtime.status.state !== 'running' || !runtimeIsCurrent(runtime, generation)) return
  const bytes = new TextEncoder().encode(value)
  for (let offset = 0; offset < bytes.byteLength;) {
    const available = runtime.capabilities.maxInputChunkBytes - runtime.inputBytes
    const length = Math.min(available, bytes.byteLength - offset)
    runtime.inputChunks.push(bytes.slice(offset, offset + length))
    runtime.inputBytes += length
    offset += length
    if (runtime.inputBytes === runtime.capabilities.maxInputChunkBytes) {
      flushTerminalInput(runtime, generation)
    }
  }
  if (runtime.inputTimer === null && runtime.inputBytes > 0) {
    runtime.inputTimer = window.setTimeout(
      () => flushTerminalInput(runtime, generation),
      TERMINAL_INPUT_FLUSH_MS,
    )
  }
}

function flushTerminalInput(runtime: TerminalRuntime, generation: number): void {
  if (runtime.inputTimer !== null) window.clearTimeout(runtime.inputTimer)
  runtime.inputTimer = null
  if (runtime.inputBytes === 0) return
  const chunk = new Uint8Array(runtime.inputBytes)
  let offset = 0
  for (const bytes of runtime.inputChunks) {
    chunk.set(bytes, offset)
    offset += bytes.byteLength
  }
  runtime.inputChunks = []
  runtime.inputBytes = 0
  runtime.writeQueue = runtime.writeQueue.then(async () => {
    if (!runtimeIsCurrent(runtime, generation) || runtime.status.state !== 'running') return
    const result = await writeRemoteTerminal(runtime.sessionId, encodeTerminalData(chunk))
    if (!runtimeIsCurrent(runtime, generation)) return
    if (result.kind === 'error') {
      operationError.value = result.error
    } else if (result.data !== chunk.byteLength) {
      operationError.value = terminalProtocolError('remote_terminal_partial_write')
    }
  })
}

function fittedTerminalSize(runtime: TerminalRuntime): TerminalSize | null {
  if (!runtime.terminal || !runtime.host) return null
  return {
    rows: Math.min(runtime.terminal.rows, runtime.capabilities.maxRows),
    columns: Math.min(runtime.terminal.cols, runtime.capabilities.maxColumns),
    pixelWidth: Math.min(runtime.host.clientWidth, runtime.capabilities.maxPixelDimension),
    pixelHeight: Math.min(runtime.host.clientHeight, runtime.capabilities.maxPixelDimension),
  }
}

function fitAndResizeTerminal(runtime: TerminalRuntime): void {
  runtime.resizeTimer = null
  if (!runtime.terminal || !runtime.fitAddon || !runtime.host) return
  if (runtime.host.clientWidth > 0 && runtime.host.clientHeight > 0) {
    runtime.fitAddon.fit()
  }
  const size = fittedTerminalSize(runtime)
  if (!size) return
  const signature = `${size.rows}:${size.columns}:${size.pixelWidth}:${size.pixelHeight}`
  if (signature === runtime.lastSize) return
  runtime.lastSize = signature
  const generation = runtime.surfaceGeneration
  runtime.resizeQueue = runtime.resizeQueue.then(async () => {
    if (!runtimeIsCurrent(runtime, generation)) return
    const result = await resizeRemoteTerminal(runtime.sessionId, size)
    if (result.kind === 'error' && runtimeIsCurrent(runtime, generation)) {
      operationError.value = result.error
    }
  })
}

function queueTerminalFit(runtime: TerminalRuntime, delay = 50): void {
  if (runtime.resizeTimer !== null) window.clearTimeout(runtime.resizeTimer)
  runtime.resizeTimer = window.setTimeout(() => fitAndResizeTerminal(runtime), delay)
}

function mountTerminalSurface(runtime: TerminalRuntime): void {
  disposeTerminalSurface(runtime)
  if (!runtime.host) return
  const generation = runtime.surfaceGeneration
  runtime.terminal = markRaw(new Terminal({
    cols: initialTerminalSize.columns,
    rows: initialTerminalSize.rows,
    cursorBlink: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
    fontSize: 13,
    scrollback: TERMINAL_SCROLLBACK_LINES,
    theme: {
      background: '#0F141A',
      foreground: '#EEF3F8',
      cursor: '#A9C7FF',
      selectionBackground: '#2B3947',
    },
  }))
  runtime.fitAddon = markRaw(new FitAddon())
  runtime.terminal.loadAddon(runtime.fitAddon)
  runtime.terminal.open(runtime.host)
  runtime.inputSubscription = runtime.terminal.onData((data) => {
    queueTerminalInput(runtime, data, generation)
  })
  if (typeof ResizeObserver !== 'undefined') {
    runtime.resizeObserver = markRaw(new ResizeObserver(() => queueTerminalFit(runtime)))
    runtime.resizeObserver.observe(runtime.host)
  }
  queueTerminalFit(runtime, 0)
  runtime.terminal.focus()
}

function disposeTerminalSurface(runtime: TerminalRuntime): void {
  runtime.surfaceGeneration += 1
  if (runtime.resizeTimer !== null) window.clearTimeout(runtime.resizeTimer)
  runtime.resizeTimer = null
  runtime.resizeObserver?.disconnect()
  runtime.resizeObserver = null
  runtime.inputSubscription?.dispose()
  runtime.inputSubscription = null
  if (runtime.inputTimer !== null) window.clearTimeout(runtime.inputTimer)
  runtime.inputTimer = null
  runtime.inputChunks = []
  runtime.inputBytes = 0
  runtime.terminal?.dispose()
  runtime.terminal = null
  runtime.fitAddon = null
  runtime.writeQueue = Promise.resolve()
  runtime.resizeQueue = Promise.resolve()
  runtime.lastSize = ''
}

async function runTerminalStream(runtime: TerminalRuntime): Promise<void> {
  const result = await streamRemoteTerminal(
    runtime.sessionId,
    runtime.capabilities.maxOutputChunkBytes,
    (event) => {
      if (!runtimeIsCurrent(runtime) || event.sessionId !== runtime.sessionId) return
      if (event.event === 'data') {
        const bytes = decodeTerminalData(event.encodedData)
        if (bytes) runtime.terminal?.write(bytes)
        else if (selectedProfileId.value === runtime.profileId) {
          operationError.value = terminalProtocolError('invalid_terminal_output')
        }
        return
      }
      runtime.status = event.status
      if (runtime.status.state !== 'running' && runtime.inputTimer !== null) {
        window.clearTimeout(runtime.inputTimer)
        runtime.inputTimer = null
        runtime.inputChunks = []
        runtime.inputBytes = 0
      }
    },
  )
  if (result.kind === 'error' && runtimeIsCurrent(runtime)) {
    runtime.status = {
      ...runtime.status,
      state: 'disconnected',
      detail: result.error.reason,
    }
    operationError.value = result.error
  }
}

function setTerminalHost(element: unknown, runtime: TerminalRuntime): void {
  runtime.host = element instanceof HTMLElement ? element : null
}

function revealTerminal(runtime: TerminalRuntime): void {
  runtime.host
    ?.closest<HTMLElement>('.remote-terminal')
    ?.scrollIntoView?.({ block: 'start' })
  runtime.terminal?.focus()
}

function revealSelectedProfile(profileId: string): void {
  const runtime = terminalRuntimes.get(profileId)
  if (runtime) {
    queueTerminalFit(runtime, 0)
    revealTerminal(runtime)
    return
  }
  remoteWorkspace.value
    ?.querySelector<HTMLElement>('.remote-terminal-placeholder')
    ?.scrollIntoView?.({ block: 'start' })
}

function terminalProfile(runtime: TerminalRuntime): StoredRemoteProfile | null {
  return profiles.value.find((stored) => stored.profile.id === runtime.profileId) ?? null
}

function terminalProfileState(profileId: string): TerminalStatus['state'] | null {
  return terminalRuntimes.get(profileId)?.status.state ?? null
}

function terminalAuthenticationLabel(runtime: TerminalRuntime): string {
  const method = terminalProfile(runtime)?.profile.authentication.method
  if (method === 'ssh_agent') return 'SSH Agent'
  if (method === 'ssh_key') return '私钥'
  if (method === 'password') return '用户名和密码'
  return '当前认证方式'
}

async function closeTerminalRuntime(profileId: string): Promise<void> {
  const runtime = terminalRuntimes.get(profileId)
  if (!runtime) return
  terminalRuntimes.delete(profileId)
  disposeTerminalSurface(runtime)
  const result = await closeRemoteTerminal(runtime.sessionId)
  if (active && result.kind === 'error') operationError.value = result.error
}

async function trustAndReconnect(): Promise<void> {
  const profileId = selectedProfileId.value
  const profile = selectedProfile.value?.profile
  if (
    !profileId
    || connecting.value
    || profile?.protocol !== 'ssh'
    || profile.trust.kind !== 'ssh_known_hosts'
    || profile.trust.first_use !== 'ask_user'
  ) return
  await closeTerminalRuntime(profileId)
  await connectSelected(true)
}

async function editTerminalAuthentication(runtime: TerminalRuntime): Promise<void> {
  const stored = terminalProfile(runtime)
  if (!stored || connecting.value || !confirmDiscardProfileChanges()) return
  await closeTerminalRuntime(runtime.profileId)
  openEditProfileForm(stored, true)
}

async function disconnect(): Promise<void> {
  if (fileAction.value || fileMutationBusy.value) return
  connecting.value = true
  await closeCurrentSession()
  connecting.value = false
}

onBeforeRouteLeave(() => confirmDiscardProfileChanges())
onMounted(() => {
  window.addEventListener('beforeunload', handleBeforeUnload)
  void loadRemoteFacts()
})
onActivated(async () => {
  await nextTick()
  if (activeProtocol.value !== 'ssh') return
  const runtime = currentTerminalRuntime.value
  if (!runtime) return
  queueTerminalFit(runtime, 0)
  runtime.terminal?.focus()
})
onBeforeUnmount(() => {
  active = false
  requestGeneration += 1
  for (const runtime of terminalRuntimes.values()) {
    disposeTerminalSurface(runtime)
    void closeRemoteTerminal(runtime.sessionId)
  }
  terminalRuntimes.clear()
  for (const workspace of Object.values(fileWorkspaces)) {
    if (workspace.session) void disconnectRemoteSession(workspace.session.id)
  }
  if (pendingSecretCleanup.value.length > 0) void cleanupSecretReferences(pendingSecretCleanup.value)
  window.removeEventListener('beforeunload', handleBeforeUnload)
})
</script>

<template>
  <section class="remote-console" aria-labelledby="remote-heading">
    <h1 id="remote-heading" class="sr-only">远程连接</h1>

    <div class="remote-tabs" role="tablist" aria-label="远程协议" @keydown="onTabKeydown">
      <button
        v-for="tab in tabs"
        :key="tab.protocol"
        class="remote-tab"
        :class="{ 'is-active': activeProtocol === tab.protocol }"
        type="button"
        role="tab"
        :data-remote-tab="tab.protocol"
        :aria-selected="activeProtocol === tab.protocol"
        :tabindex="activeProtocol === tab.protocol ? 0 : -1"
        :disabled="savingProfile || deletingProfile || Boolean(fileAction) || fileMutationBusy"
        @click="selectProtocol(tab.protocol)"
      >
        <component :is="tab.icon" :size="18" aria-hidden="true" />
        <span>{{ tab.label }}</span>
      </button>
      <button
        class="icon-button remote-refresh"
        type="button"
        aria-label="刷新远程连接事实"
        title="刷新远程连接事实"
        :disabled="refreshing || savingProfile || deletingProfile || Boolean(fileAction) || fileMutationBusy"
        @click="loadRemoteFacts"
      >
        <RefreshCw :size="16" :class="{ 'is-spinning': refreshing }" aria-hidden="true" />
      </button>
    </div>

    <div class="remote-layout">
      <aside class="remote-profiles" aria-labelledby="remote-profiles-heading">
        <div class="remote-panel-heading">
          <div>
            <span class="remote-kicker">{{ protocolLabel(activeProtocol) }}</span>
            <h2 id="remote-profiles-heading">连接配置</h2>
          </div>
          <span class="remote-count">{{ visibleProfiles.length }}</span>
        </div>

        <button
          class="remote-new-button"
          type="button"
          :disabled="loadState !== 'ready' || !adapterOperable || refreshing || savingProfile || deletingProfile || Boolean(fileAction) || fileMutationBusy"
          @click="openProfileForm"
        >
          <Plus :size="16" aria-hidden="true" />
          <span>新建连接</span>
        </button>

        <form v-if="showProfileForm" class="remote-profile-form" @submit.prevent="saveProfile">
          <div class="remote-form-heading">
            <strong>{{ editingProfile ? '编辑' : '新建' }} {{ protocolLabel(activeProtocol) }}</strong>
            <button class="icon-button compact-button" type="button" aria-label="关闭配置表单" title="关闭" :disabled="savingProfile || deletingProfile" @click="closeProfileForm">
              <X :size="15" aria-hidden="true" />
            </button>
          </div>
          <fieldset class="remote-profile-fields" :disabled="savingProfile || deletingProfile">
          <label for="remote-profile-label">名称</label>
          <input id="remote-profile-label" v-model="formLabel" maxlength="128" autocomplete="off" required />
          <label for="remote-profile-host">{{ smbForm ? '服务器' : '主机' }}</label>
          <input id="remote-profile-host" v-model="formHost" maxlength="255" autocomplete="off" required />
          <div class="remote-form-grid">
            <div>
              <label for="remote-profile-port">端口</label>
              <input id="remote-profile-port" v-model.number="formPort" type="number" min="1" max="65535" required />
            </div>
            <div v-if="sshForm || passwordForm">
              <label for="remote-profile-user">用户</label>
              <input id="remote-profile-user" v-model="formUsername" maxlength="256" autocomplete="username" :required="usernameRequired" />
            </div>
          </div>
          <label for="remote-profile-authentication">认证</label>
          <select id="remote-profile-authentication" v-model="formAuthentication">
            <template v-if="sshForm">
              <option value="ssh_agent">SSH Agent</option>
              <option value="ssh_key">私钥</option>
              <option value="password">用户名和密码</option>
            </template>
            <template v-else-if="ftpForm">
              <option value="anonymous">匿名</option>
              <option value="password">用户名和密码</option>
            </template>
            <template v-else-if="smbForm">
              <option value="password">用户名和密码</option>
              <option value="kerberos">Kerberos</option>
            </template>
          </select>
          <template v-if="passwordForm">
            <label for="remote-profile-password">密码</label>
            <input
              id="remote-profile-password"
              v-model="formPassword"
              type="password"
              maxlength="8192"
              autocomplete="new-password"
              :placeholder="canReuseExistingPassword ? '留空则保留现有密码' : ''"
              :required="!canReuseExistingPassword"
            />
            <small v-if="canReuseExistingPassword" class="remote-saved-secret-state">
              <Check :size="14" aria-hidden="true" />
              <span>密码已保存在 Secret Service；留空将保留现有密码</span>
            </small>
          </template>
          <template v-if="privateKeyForm">
            <label for="remote-profile-private-key">私钥内容</label>
            <textarea
              id="remote-profile-private-key"
              v-model="formPrivateKey"
              rows="4"
              maxlength="8192"
              autocomplete="off"
              spellcheck="false"
              :placeholder="canReuseExistingPrivateKey ? '留空则保留现有私钥' : ''"
              :required="!canReuseExistingPrivateKey"
            ></textarea>
            <label for="remote-profile-key-passphrase">私钥口令（可选）</label>
            <input
              id="remote-profile-key-passphrase"
              v-model="formKeyPassphrase"
              type="password"
              maxlength="8192"
              autocomplete="new-password"
              :placeholder="canReuseExistingKeyPassphrase ? '留空则保留现有口令' : ''"
            />
            <label
              v-if="canReuseExistingKeyPassphrase && !formPrivateKey.trim()"
              class="remote-policy-checkbox"
            >
              <input
                v-model="formRemoveKeyPassphrase"
                type="checkbox"
                :disabled="Boolean(formKeyPassphrase)"
              />
              <span>移除已保存的私钥口令</span>
            </label>
          </template>
          <label v-if="activeProtocol === 'ssh'" class="remote-policy-checkbox">
            <input v-model="formSshAskFirstUse" type="checkbox" />
            <span>首次连接时询问信任主机密钥</span>
          </label>
          <template v-if="activeProtocol === 'ftps_explicit'">
            <label for="remote-profile-ftps-trust">证书信任</label>
            <select id="remote-profile-ftps-trust" v-model="formFtpsTrustMode">
              <option value="system_tls">系统 CA</option>
              <option value="pinned_tls_certificate">固定服务器证书</option>
            </select>
            <template v-if="formFtpsTrustMode === 'pinned_tls_certificate'">
              <label for="remote-profile-ftps-certificate">服务器证书（PEM）</label>
              <textarea
                id="remote-profile-ftps-certificate"
                v-model="formFtpsCertificatePem"
                rows="6"
                maxlength="16384"
                autocomplete="off"
                spellcheck="false"
                placeholder="-----BEGIN CERTIFICATE-----"
                required
              ></textarea>
              <small class="remote-saved-secret-state">
                <ShieldCheck :size="14" aria-hidden="true" />
                <span>仅信任这张证书；仍校验证书中的主机名</span>
              </small>
            </template>
          </template>
          <template v-if="smbForm">
            <label for="remote-profile-share">共享</label>
            <input id="remote-profile-share" v-model="formShare" maxlength="255" autocomplete="off" required />
            <template v-if="passwordForm">
              <label for="remote-profile-domain">域（可选）</label>
              <input id="remote-profile-domain" v-model="formDomain" maxlength="255" autocomplete="organization" />
            </template>
            <label for="remote-profile-smb-dialect">最低协议</label>
            <select id="remote-profile-smb-dialect" v-model="formSmbMinimumDialect">
              <option value="smb3">SMB3</option>
              <option value="smb2">SMB2</option>
            </select>
            <label class="remote-policy-checkbox">
              <input v-model="formSmbRequireSigning" type="checkbox" />
              <span>要求签名</span>
            </label>
            <label class="remote-policy-checkbox">
              <input v-model="formSmbRequireEncryption" type="checkbox" />
              <span>要求加密</span>
            </label>
          </template>
          <div class="remote-auth-fact" :class="{ 'is-warning': activeProtocol === 'ftp' }">
            <CircleAlert v-if="activeProtocol === 'ftp'" :size="14" aria-hidden="true" />
            <KeyRound v-else :size="14" aria-hidden="true" />
            <span>{{ authenticationFact }}</span>
          </div>
          <label v-if="activeProtocol === 'ftp'" class="remote-ftp-confirmation">
            <input v-model="formPlainFtpAcknowledged" type="checkbox" />
            <span>我了解 FTP 会明文传输凭据和文件内容</span>
          </label>
          <button class="remote-primary-button" type="submit" :disabled="refreshing || savingProfile || deletingProfile || (activeProtocol === 'ftp' && !formPlainFtpAcknowledged)">
            <LoaderCircle v-if="savingProfile" :size="15" class="is-spinning" aria-hidden="true" />
            <Pencil v-else-if="editingProfile" :size="15" aria-hidden="true" />
            <Plus v-else :size="15" aria-hidden="true" />
            <span>{{ editingProfile ? '保存修改' : '保存配置' }}</span>
          </button>
          <button
            v-if="editingProfile"
            class="remote-secondary-button danger-action"
            type="button"
            :disabled="refreshing || savingProfile || deletingProfile"
            @click="removeEditedProfile"
          >
            <LoaderCircle v-if="deletingProfile" :size="15" class="is-spinning" aria-hidden="true" />
            <Trash2 v-else :size="15" aria-hidden="true" />
            <span>删除配置</span>
          </button>
          </fieldset>
        </form>

        <div v-if="loadState === 'loading'" class="remote-profile-state" aria-live="polite">
          <LoaderCircle :size="24" class="is-spinning" aria-hidden="true" />
          <span>正在读取配置</span>
        </div>
        <div v-else-if="visibleProfiles.length === 0 && !showProfileForm" class="remote-profile-state">
          <SquareTerminal v-if="activeProtocol === 'ssh'" :size="34" aria-hidden="true" />
          <FolderOpen v-else :size="34" aria-hidden="true" />
          <strong>{{ activeProtocol === 'smb' ? '暂无 SMB 配置' : '暂无连接配置' }}</strong>
          <code v-if="activeProtocol !== 'smb'">remote_profile_list_empty</code>
        </div>
        <div v-else class="remote-profile-list" role="list" aria-label="连接配置">
          <div
            v-for="stored in visibleProfiles"
            :key="stored.profile.id"
            class="remote-profile-row"
            role="listitem"
          >
            <button
              class="remote-profile-item"
              :class="{ 'is-selected': selectedProfileId === stored.profile.id }"
              type="button"
              :aria-pressed="selectedProfileId === stored.profile.id"
              :disabled="refreshing || savingProfile || deletingProfile || Boolean(fileAction) || fileMutationBusy"
              @click="selectProfile(stored.profile.id)"
            >
              <Server :size="17" aria-hidden="true" />
              <span class="remote-profile-copy">
                <strong>{{ stored.profile.label }}</strong>
                <small>
                  {{ stored.profile.endpoint.host }}:{{ stored.profile.endpoint.port }}
                  <template v-if="terminalProfileState(stored.profile.id)"> · {{ terminalProfileState(stored.profile.id) }}</template>
                </small>
              </span>
            </button>
            <button
              class="icon-button remote-profile-edit"
              type="button"
              :aria-label="`编辑连接 ${stored.profile.label}`"
              :title="`编辑 ${stored.profile.label}`"
              :disabled="refreshing || savingProfile || deletingProfile || Boolean(fileAction) || fileMutationBusy"
              @click="openEditProfileForm(stored)"
            >
              <Pencil :size="15" aria-hidden="true" />
            </button>
          </div>
        </div>
      </aside>

      <main ref="remoteWorkspace" class="remote-workspace" aria-live="polite">
        <div v-if="operationError" class="remote-operation-error" role="status">
          <CircleAlert :size="17" aria-hidden="true" />
          <code>{{ operationError.reason }}</code>
          <button
            v-if="pendingSecretCleanup.length > 0"
            class="remote-secondary-button remote-error-retry"
            type="button"
            :disabled="cleaningSecrets"
            @click="retryPendingSecretCleanup"
          >
            <LoaderCircle v-if="cleaningSecrets" :size="14" class="is-spinning" aria-hidden="true" />
            <RefreshCw v-else :size="14" aria-hidden="true" />
            <span>重试清理凭据</span>
          </button>
          <button class="icon-button compact-button" type="button" aria-label="关闭错误" title="关闭错误" @click="operationError = null">
            <X :size="14" aria-hidden="true" />
          </button>
        </div>

        <div v-if="loadState === 'error'" class="remote-workspace-state is-error">
          <Unplug :size="38" aria-hidden="true" />
          <strong>远程服务不可用</strong>
          <code>{{ loadError?.reason }}</code>
          <button class="remote-secondary-button" type="button" :disabled="refreshing || savingProfile || deletingProfile" @click="loadRemoteFacts">
            <RefreshCw :size="15" aria-hidden="true" />
            <span>重试</span>
          </button>
        </div>

        <template v-else>
          <div v-show="activeProtocol === 'ssh'" class="remote-protocol-workspace remote-ssh-workspace">
          <div
            v-if="!currentTerminalRuntime"
            class="remote-workspace-state remote-ssh-surface remote-terminal-placeholder"
          >
            <SquareTerminal :size="42" aria-hidden="true" />
            <strong>{{ selectedProfile ? '终端尚未打开' : '选择或创建 SSH 配置后打开终端' }}</strong>
            <code v-if="!selectedProfile">remote_profile_required</code>
            <button
              v-else
              class="remote-primary-button"
              type="button"
              :disabled="connecting || !adapterOperable"
              @click="connectSelected()"
            >
              <LoaderCircle v-if="connecting" :size="15" class="is-spinning" aria-hidden="true" />
              <SquareTerminal v-else :size="15" aria-hidden="true" />
              <span>打开终端</span>
            </button>
          </div>
          <div
            v-for="runtime in terminalRuntimeList"
            v-show="runtime.profileId === selectedProfileId"
            :key="runtime.profileId"
            class="remote-terminal remote-ssh-surface"
            :class="{ 'has-terminal-action': runtime.status.detail === 'host_key_unknown' || runtime.status.detail === 'authentication_failed' }"
          >
            <button
              class="icon-button remote-terminal-close"
              type="button"
              aria-label="关闭 SSH 终端"
              title="关闭终端"
              :disabled="connecting"
              @click="closeTerminalRuntime(runtime.profileId)"
            >
              <Unplug :size="15" aria-hidden="true" />
            </button>
            <div v-if="runtime.status.detail === 'host_key_unknown' && terminalProfile(runtime)?.profile.trust.kind === 'ssh_known_hosts' && terminalProfile(runtime)?.profile.trust.first_use === 'ask_user'" class="remote-host-key-confirmation" role="alert">
              <ShieldCheck :size="18" aria-hidden="true" />
              <div>
                <strong>确认首次信任此 SSH 主机</strong>
                <span>{{ terminalProfile(runtime)?.profile.endpoint.host }}:{{ terminalProfile(runtime)?.profile.endpoint.port }} 尚未记录在本机控制台 known_hosts 中。确认后仅接纳新密钥；密钥变化仍会拒绝连接。</span>
              </div>
              <button class="remote-primary-button" type="button" :disabled="connecting" @click="trustAndReconnect">
                <LoaderCircle v-if="connecting" :size="15" class="is-spinning" aria-hidden="true" />
                <ShieldCheck v-else :size="15" aria-hidden="true" />
                <span>信任并重新连接</span>
              </button>
            </div>
            <div v-if="runtime.status.detail === 'authentication_failed'" class="remote-terminal-action" role="alert">
              <KeyRound :size="18" aria-hidden="true" />
              <div>
                <strong>SSH 认证失败</strong>
                <span>服务器拒绝了 {{ terminalAuthenticationLabel(runtime) }}。请编辑连接，选择“用户名和密码”并保存密码，或配置服务器认可的公钥。</span>
              </div>
              <button class="remote-primary-button" type="button" :disabled="connecting" @click="editTerminalAuthentication(runtime)">
                <Pencil :size="15" aria-hidden="true" />
                <span>编辑认证信息</span>
              </button>
            </div>
            <div
              :ref="(element) => setTerminalHost(element, runtime)"
              class="remote-terminal-output"
              role="application"
              :aria-label="`${terminalProfile(runtime)?.profile.label ?? 'SSH'} 交互式终端`"
            ></div>
          </div>
          </div>

          <div v-show="activeProtocol !== 'ssh'" class="remote-protocol-workspace">
          <div v-if="!fileSession" class="remote-workspace-state remote-file-connect-placeholder">
            <FolderOpen :size="48" aria-hidden="true" />
            <strong>{{ selectedProfile ? `浏览 ${selectedProfile.profile.label}` : activeProtocol === 'smb' ? '创建或选择 SMB 配置' : `选择或创建 ${protocolLabel(activeProtocol)} 配置` }}</strong>
            <span v-if="activeProtocol === 'smb' && !selectedProfile">连接后浏览共享与文件</span>
            <code v-else-if="!selectedProfile">remote_profile_required</code>
            <button
              v-else
              class="remote-primary-button"
              type="button"
              :disabled="connecting || !adapterOperable"
              @click="connectSelected"
            >
              <LoaderCircle v-if="connecting" :size="15" class="is-spinning" aria-hidden="true" />
              <FolderOpen v-else :size="15" aria-hidden="true" />
              <span>连接并浏览</span>
            </button>
          </div>
          <div v-else class="remote-file-browser" :class="{ 'has-file-action': fileAction }">
            <header class="remote-session-bar">
              <div class="remote-path-bar">
                <button class="icon-button compact-button" type="button" aria-label="返回上级目录" title="返回上级目录" :disabled="currentPath === '/' || Boolean(fileAction) || fileMutationBusy" @click="navigateDirectory(parentPath(currentPath))">
                  <ChevronLeft :size="15" aria-hidden="true" />
                </button>
                <Folder :size="15" aria-hidden="true" />
                <code :title="currentPath">{{ currentPath }}</code>
              </div>
              <div class="remote-session-actions">
                <button
                  v-if="supportsFileOperation('write')"
                  class="icon-button compact-button"
                  type="button"
                  aria-label="上传到当前目录"
                  title="上传到当前目录"
                  :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy"
                  @click="openTransfer('upload', currentPath === '/' ? '/' : `${currentPath.replace(/\/+$/u, '')}/`)"
                >
                  <Upload :size="15" aria-hidden="true" />
                </button>
                <button
                  v-if="supportsFileOperation('create_directory')"
                  class="icon-button compact-button"
                  type="button"
                  aria-label="新建文件夹"
                  title="新建文件夹"
                  :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy"
                  @click="beginCreateDirectory"
                >
                  <FolderPlus :size="15" aria-hidden="true" />
                </button>
                <button class="remote-secondary-button" type="button" :disabled="connecting || Boolean(fileAction) || fileMutationBusy" @click="disconnect">
                  <Unplug :size="14" aria-hidden="true" />
                  <span>断开</span>
                </button>
              </div>
            </header>
            <form v-if="fileAction" class="remote-file-action" @submit.prevent="submitFileAction">
              <label v-if="fileAction.kind !== 'delete'">
                <span>{{ fileAction.kind === 'create_directory' ? '文件夹名称' : '新名称' }}</span>
                <input v-model="fileActionValue" maxlength="255" autocomplete="off" required autofocus />
              </label>
              <span v-else class="remote-delete-confirmation">
                确定删除 <code :title="fileAction.entry.path">{{ fileAction.entry.name }}</code>？删除后无法撤销。
              </span>
              <button :class="fileAction.kind === 'delete' ? 'remote-secondary-button danger-action' : 'remote-primary-button'" type="submit" :disabled="directoryLoading || fileMutationBusy">
                <LoaderCircle v-if="fileMutationBusy" :size="14" class="is-spinning" aria-hidden="true" />
                <Trash2 v-else-if="fileAction.kind === 'delete'" :size="14" aria-hidden="true" />
                <Check v-else :size="14" aria-hidden="true" />
                <span>{{ fileAction.kind === 'delete' ? '确认删除' : '确认' }}</span>
              </button>
              <button class="icon-button compact-button" type="button" aria-label="取消文件操作" title="取消" :disabled="fileMutationBusy" @click="cancelFileAction">
                <X :size="14" aria-hidden="true" />
              </button>
            </form>
            <div v-if="!directoryPage && directoryLoading" class="remote-workspace-state compact">
              <LoaderCircle :size="25" class="is-spinning" aria-hidden="true" />
              <span>正在读取目录</span>
            </div>
            <div v-else-if="!directoryPage" class="remote-workspace-state compact is-error">
              <CircleAlert :size="30" aria-hidden="true" />
              <strong>目录不可用</strong>
              <code>{{ operationError?.reason ?? 'remote_directory_unavailable' }}</code>
              <button class="remote-secondary-button" type="button" :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy" @click="navigateDirectory(currentPath)">
                <RefreshCw :size="15" aria-hidden="true" />
                <span>重试</span>
              </button>
            </div>
            <div v-else-if="directoryPage.entries.length === 0" class="remote-workspace-state compact">
              <FolderOpen :size="30" aria-hidden="true" />
              <strong>目录为空</strong>
              <code>{{ currentPath }}</code>
            </div>
            <div v-else class="remote-file-table-wrap">
              <table class="remote-file-table">
                <thead><tr><th>名称</th><th>类型</th><th>大小</th><th>操作</th></tr></thead>
                <tbody>
                  <tr v-for="entry in directoryPage.entries" :key="entry.path">
                    <th scope="row">
                      <button v-if="entry.kind === 'directory'" type="button" :disabled="Boolean(fileAction) || fileMutationBusy" @click="navigateDirectory(entry.path)">
                        <Folder :size="15" aria-hidden="true" /><span>{{ entry.name }}</span>
                      </button>
                      <span v-else><File :size="15" aria-hidden="true" />{{ entry.name }}</span>
                    </th>
                    <td>{{ entry.kind }}</td>
                    <td>{{ formatSize(entry.sizeBytes) }}</td>
                    <td>
                      <div class="remote-entry-actions">
                        <button
                          v-if="entry.kind !== 'directory' && entrySupports(entry, 'read')"
                          class="icon-button compact-button"
                          type="button"
                          aria-label="下载"
                          title="下载"
                          :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy"
                          @click="openTransfer('download', entry.path)"
                        >
                          <Download :size="14" aria-hidden="true" />
                        </button>
                        <button
                          v-if="entrySupports(entry, 'rename')"
                          class="icon-button compact-button"
                          type="button"
                          aria-label="重命名"
                          title="重命名"
                          :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy"
                          @click="beginRename(entry)"
                        >
                          <Pencil :size="14" aria-hidden="true" />
                        </button>
                        <button
                          v-if="entrySupports(entry, 'delete')"
                          class="icon-button compact-button danger-action"
                          type="button"
                          aria-label="删除"
                          title="删除"
                          :disabled="directoryLoading || Boolean(fileAction) || fileMutationBusy"
                          @click="beginDelete(entry)"
                        >
                          <Trash2 :size="14" aria-hidden="true" />
                        </button>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
              <div v-if="directoryPage.offset > 0 || directoryPage.nextOffset !== null" class="remote-pagination">
                <button class="icon-button compact-button" type="button" aria-label="上一页" title="上一页" :disabled="directoryOffsetHistory.length === 0 || directoryLoading || Boolean(fileAction) || fileMutationBusy" @click="previousDirectoryPage">
                  <ChevronLeft :size="15" aria-hidden="true" />
                </button>
                <span>{{ directoryPage.offset + 1 }}-{{ directoryPage.offset + directoryPage.entries.length }}</span>
                <button class="icon-button compact-button" type="button" aria-label="下一页" title="下一页" :disabled="directoryPage.nextOffset === null || directoryLoading || Boolean(fileAction) || fileMutationBusy" @click="nextDirectoryPage">
                  <ChevronRight :size="15" aria-hidden="true" />
                </button>
              </div>
            </div>
          </div>
          </div>
        </template>
      </main>

      <aside class="remote-inspector" aria-labelledby="remote-inspector-heading">
        <div class="remote-panel-heading inspector">
          <div>
            <span class="remote-kicker">能力事实</span>
            <h2 id="remote-inspector-heading">{{ protocolLabel(activeProtocol) }} 能力</h2>
          </div>
        </div>
        <dl class="remote-facts">
          <div class="remote-fact-row">
            <dt><component :is="statusIcon(activeAdapter)" :size="17" aria-hidden="true" />状态</dt>
            <dd :class="`is-${activeAdapter?.availability.status ?? 'unreachable'}`">
              {{ statusLabel(activeAdapter?.availability.status ?? 'unreachable') }}
            </dd>
          </div>
          <div class="remote-fact-row stacked">
            <dt><Info :size="17" aria-hidden="true" />原因</dt>
            <dd><code>{{ activeReason }}</code></dd>
          </div>
          <div class="remote-fact-row">
            <dt><ShieldCheck :size="17" aria-hidden="true" />{{ activeProtocol === 'ssh' ? '终端' : '文件操作' }}</dt>
            <dd>{{ activeProtocol === 'ssh' ? statusLabel(activeAdapter?.terminal.status ?? 'unsupported') : `${supportedFileOperations} / 11` }}</dd>
          </div>
          <div class="remote-fact-row stacked">
            <dt><ShieldCheck :size="17" aria-hidden="true" />边界</dt>
            <dd>{{ activeProtocol === 'ssh' ? '固定 OpenSSH · 无任意命令参数' : activeProtocol === 'smb' ? '未完成授权端点互操作验证' : activeProtocol === 'ftp' ? '明文 FTP · 需显式确认' : '受控 I/O · typed 状态' }}</dd>
          </div>
        </dl>
      </aside>
    </div>

    <footer class="remote-status-strip" aria-label="协议能力摘要">
      <div v-for="protocol in footerProtocols" :key="protocol" class="remote-status-item">
        <component
          :is="statusIcon(catalog?.adapters.find((adapter) => adapter.protocol === protocol) ?? null)"
          :size="16"
          :class="`is-${catalog?.adapters.find((adapter) => adapter.protocol === protocol)?.availability.status ?? 'unreachable'}`"
          aria-hidden="true"
        />
        <span>{{ protocol === 'ftps_explicit' ? 'FTPS' : protocol.toUpperCase() }}</span>
        <strong>{{ statusLabel(catalog?.adapters.find((adapter) => adapter.protocol === protocol)?.availability.status ?? 'unreachable') }}</strong>
      </div>
      <div class="remote-status-reason">
        <span>原因</span>
        <code>{{ catalog?.adapters.find((adapter) => adapter.protocol === 'smb')?.availability.capabilityReason ?? 'remote_catalog_pending' }}</code>
      </div>
    </footer>
  </section>
</template>
