export const backendStatuses = ['healthy', 'degraded', 'unsupported', 'unreachable'] as const

export type BackendStatus = (typeof backendStatuses)[number]

export interface BackendHealth {
  status: BackendStatus
  capabilityReason: string
}

export interface BackendCapability {
  id: string
  status: BackendStatus
  reason: string
}

export interface BackendCapabilityReport {
  daemonVersion: string
  health: BackendHealth
  capabilities: BackendCapability[]
}

export type BackendCapabilityFetchResult =
  | { kind: 'report'; report: BackendCapabilityReport }
  | { kind: 'error'; error: BridgeError }

export interface CapabilityState {
  status: 'unsupported'
  capabilityReason: string
}

export const bridgeErrorKinds = ['transport', 'protocol', 'daemon'] as const
export type BridgeErrorKind = (typeof bridgeErrorKinds)[number]

export interface BridgeError {
  kind: BridgeErrorKind
  code: string
  reason: string
  retryable: boolean
}

export const telemetryFreshnessValues = ['fresh', 'warming_up', 'stale', 'unknown'] as const
export type TelemetryFreshness = (typeof telemetryFreshnessValues)[number]

export const telemetryStatusValues = ['complete', 'partial', 'unavailable'] as const
export type TelemetryStatus = (typeof telemetryStatusValues)[number]

export const metricStateValues = [
  'known',
  'unknown',
  'permission_denied',
  'raced',
  'unbounded',
  'warming_up',
  'sampling_gap',
] as const
export type MetricState = (typeof metricStateValues)[number]

export const groupingResolutionValues = [
  'desktop_entry_exact',
  'cgroup_scope',
  'inherited_parent',
  'unknown',
] as const
export type GroupingResolution = (typeof groupingResolutionValues)[number]

export interface MetricValue {
  value: number | null
  state: MetricState
  reason: string | null
}

export interface IssueCount {
  code: string
  count: number
}

export interface ApplicationTelemetry {
  applicationKey: string
  desktopEntryId: string | null
  displayLabel: string
  groupingResolution: GroupingResolution
  processCount: number
  processScope: string
  cgroupScope: string
  cpuPercentTotalCapacity: MetricValue
  cgroupCpuPercentTotalCapacity: MetricValue
  rssBytes: MetricValue
  pssBytes: MetricValue
  memoryCurrentBytes: MetricValue
  cgroupProcessCount: MetricValue
  fdUsed: MetricValue
  fdSoftLimit: MetricValue
  fdPercentOfAttributed: MetricValue
  fdPercentOfSoftLimit: MetricValue
  fdMaxProcessPercentOfSoftLimit: MetricValue
}

export interface SystemFdTelemetry {
  scope: string
  fileNrAllocated: MetricValue
  fileNrMax: MetricValue
  fileMax: MetricValue
  pressurePercent: MetricValue
}

export interface TelemetrySnapshot {
  schemaVersion: 4
  snapshotId: string
  capturedAtUnixMs: number | null
  sampleIntervalMs: number | null
  logicalCpuCount: number | null
  freshness: TelemetryFreshness
  status: TelemetryStatus
  reason: string
  retryable: boolean
  scope: string
  lastSuccessAtUnixMs: number | null
  permissionDeniedCounts: IssueCount[]
  issues: IssueCount[]
  systemFd: SystemFdTelemetry
  applications: ApplicationTelemetry[]
}

export type TelemetryFetchResult =
  | { kind: 'snapshot'; snapshot: TelemetrySnapshot }
  | { kind: 'error'; error: BridgeError }

export const networkFreshnessValues = ['fresh', 'warming_up', 'stale', 'unknown'] as const
export type NetworkFreshness = (typeof networkFreshnessValues)[number]

export const networkInterfaceKindValues = ['physical', 'loopback', 'tunnel', 'virtual'] as const
export type NetworkInterfaceKind = (typeof networkInterfaceKindValues)[number]

export const networkRateStateValues = [
  'known',
  'warming_up',
  'sampling_gap',
  'counter_reset_or_wrap',
  'counters_unavailable',
] as const
export type NetworkRateState = (typeof networkRateStateValues)[number]

export const networkInterfaceTransitionValues = [
  'stable',
  'first_observation',
  'hotplug_added',
  'sampling_gap',
  'counter_reset_or_wrap',
  'counters_unavailable',
] as const
export type NetworkInterfaceTransition = (typeof networkInterfaceTransitionValues)[number]

export const networkLayeredAccountingValues = [
  'not_detected',
  'possible_vpn_underlay_double_counting',
] as const
export type NetworkLayeredAccounting = (typeof networkLayeredAccountingValues)[number]

export interface NetworkCapabilityState {
  status: BackendStatus
  reason: string
}

export interface NetworkByteTotals {
  rxBytes: number
  txBytes: number
}

export interface NetworkTrafficTotals {
  scope: 'inclusive_interfaces'
  allInterfaces: NetworkByteTotals
  physical: NetworkByteTotals
  loopback: NetworkByteTotals
  tunnel: NetworkByteTotals
  otherVirtual: NetworkByteTotals
}

export interface NetworkRate {
  rxBytesPerSecond: number | null
  txBytesPerSecond: number | null
  state: NetworkRateState
  reason: string
}

export interface NetworkInterfaceSample {
  index: number
  name: string
  kind: NetworkInterfaceKind
  kernelKind: string | null
  isUp: boolean
  carrierUp: boolean
  counters: NetworkByteTotals | null
  rate: NetworkRate
  transition: NetworkInterfaceTransition
}

export interface NetworkApplicationTraffic {
  applicationKey: string
  rxBytes: number
  txBytes: number
  rxSharePercent: number | null
  txSharePercent: number | null
}

export interface NetworkCoverage {
  reportedInterfaces: number
  interfacesWithCounters: number
  includesLoopback: boolean
  includesTunnels: boolean
  layeredAccounting: NetworkLayeredAccounting
  reason: string
}

export interface NetworkSnapshot {
  schemaVersion: 1
  snapshotId: string
  capturedAtUnixMs: number | null
  observedBoottimeMs: number | null
  sampleIntervalMs: number | null
  lastSuccessAtUnixMs: number | null
  freshness: NetworkFreshness
  retryable: boolean
  systemTraffic: NetworkCapabilityState
  perApplication: NetworkCapabilityState
  coverage: NetworkCoverage
  totals: NetworkTrafficTotals | null
  aggregateRate: NetworkRate
  interfaces: NetworkInterfaceSample[]
  applications: NetworkApplicationTraffic[]
}

export type NetworkFetchResult =
  | { kind: 'snapshot'; snapshot: NetworkSnapshot }
  | { kind: 'error'; error: BridgeError }

export const usagePeriods = ['daily', 'weekly'] as const
export type UsagePeriod = (typeof usagePeriods)[number]

export interface UsageSummaryQuery {
  period: UsagePeriod
  bucketKey: string
}

export interface UsageApplicationDuration {
  appId: string
  bucketKey: string
  timezoneId: string
  utcOffsetSeconds: number
  durationNs: number
  lastWallUtcMs: number
}

export interface UsageCoverage {
  status: BackendStatus
  reason: string
  niriEventStreamConnected: boolean
  logindSessionAvailable: boolean
  eventGapCount: number
  lastCheckpointUnixMs: number | null
  trackingStartedUnixMs: number | null
  bucketStartCovered: boolean
  definition: 'foreground_unlocked_input_active_300s_monotonic'
}

export interface UsageSummary {
  schemaVersion: 3
  snapshotId: string
  capturedAtUnixMs: number | null
  query: UsageSummaryQuery
  status: BackendStatus
  reason: string
  retryable: boolean
  coverage: UsageCoverage
  applications: UsageApplicationDuration[]
}

export type UsageFetchResult =
  | { kind: 'summary'; summary: UsageSummary }
  | { kind: 'error'; error: BridgeError }

export interface SystemInfoEntry {
  key: string
  value: string
}

export interface SystemInfoGroup {
  title: string | null
  entries: SystemInfoEntry[]
}

export interface SystemInfoSection {
  id: string
  groups: SystemInfoGroup[]
}

export interface SystemInfoReport {
  schemaVersion: number
  capturedAtUnixMs: number | null
  toolVersion: string | null
  status: BackendStatus
  reason: string
  retryable: boolean
  sections: SystemInfoSection[]
}

export type SystemInfoFetchResult =
  | { kind: 'systemInfo'; report: SystemInfoReport }
  | { kind: 'error'; error: BridgeError }

export const remoteProtocols = ['ssh', 'sftp', 'ftp', 'ftps_explicit', 'smb'] as const
export type RemoteProtocol = (typeof remoteProtocols)[number]

export const fileOperations = [
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
export type FileOperation = (typeof fileOperations)[number]

export interface RemoteCapabilityStatus {
  status: 'supported' | 'unsupported'
  reason: string | null
}

export interface RemoteOperationCapability extends RemoteCapabilityStatus {
  operation: FileOperation
}

export interface RemoteAdapterDescriptor {
  protocol: RemoteProtocol
  availability: BackendHealth
  terminal: RemoteCapabilityStatus
  fileOperations: RemoteOperationCapability[]
}

export interface RemoteAdapterCatalog {
  schemaVersion: 1
  snapshotId: string
  capturedAtUnixMs: number
  adapters: RemoteAdapterDescriptor[]
}

export type TerminalStreamEvent =
  | { event: 'started'; sessionId: string; maxBytes: number; status: TerminalStatus }
  | { event: 'data'; sessionId: string; encodedData: string }
  | { event: 'status'; sessionId: string; status: TerminalStatus }
  | { event: 'ended'; sessionId: string; status: TerminalStatus }

export interface RemoteEndpoint {
  host: string
  port: number
}

export const remoteSecretKinds = ['password', 'private_key', 'key_passphrase'] as const
export type RemoteSecretKind = (typeof remoteSecretKinds)[number]

export interface RemoteSecretReference {
  backend: 'secret_service'
  item_id: string
}

export type RemoteAuthentication =
  | { method: 'anonymous' }
  | { method: 'ssh_agent' }
  | { method: 'kerberos' }
  | { method: 'password'; secret: RemoteSecretReference }
  | {
      method: 'ssh_key'
      private_key: RemoteSecretReference
      passphrase: RemoteSecretReference | null
    }

export type RemoteTrustPolicy =
  | { kind: 'ssh_known_hosts'; first_use: 'reject' | 'ask_user' }
  | { kind: 'system_tls' }
  | { kind: 'pinned_tls_certificate'; certificate_pem: string }
  | { kind: 'plaintext_acknowledged' }
  | { kind: 'smb_negotiated' }

export type RemoteProfileOptions =
  | { protocol: 'ssh'; jump_profiles: string[]; agent_forwarding: boolean }
  | { protocol: 'sftp'; jump_profiles: string[] }
  | { protocol: 'ftp'; data_connection: 'passive' | 'active_restricted' }
  | {
      protocol: 'ftps_explicit'
      data_connection: 'passive' | 'active_restricted'
      require_protected_data_channel: boolean
    }
  | {
      protocol: 'smb'
      share: string | null
      minimum_dialect: 'smb2' | 'smb3'
      require_signing: boolean
      require_encryption: boolean
    }

export interface RemoteConnectionProfile {
  id: string
  label: string
  protocol: RemoteProtocol
  endpoint: RemoteEndpoint
  username: string | null
  domain: string | null
  authentication: RemoteAuthentication
  trust: RemoteTrustPolicy
  options: RemoteProfileOptions
}

export interface StoredRemoteProfile {
  profile: RemoteConnectionProfile
  revision: number
  createdAtUnixMs: number
  updatedAtUnixMs: number
}

export interface RemoteProfilePage {
  profiles: StoredRemoteProfile[]
  nextAfter: string | null
}

export type RemoteEntryKind = 'file' | 'directory' | 'symlink' | 'other'

export interface RemoteEntry {
  name: string
  path: string
  kind: RemoteEntryKind
  sizeBytes: number | null
  modifiedAtUnixMs: number | null
  unixMode: number | null
  capabilities: RemoteOperationCapability[]
}

export interface RemoteSession {
  id: string
  profileId: string
  protocol: RemoteProtocol
  state: string
  stateReason: string | null
  capabilities: RemoteOperationCapability[]
  openedAtUnixMs: number
  updatedAtUnixMs: number
}

export interface RemoteDirectoryPage {
  sessionId: string
  path: string
  offset: number
  entries: RemoteEntry[]
  nextOffset: number | null
}

export interface TerminalCapabilities {
  maxOutputChunkBytes: number
  maxInputChunkBytes: number
  maxTranscriptBytes: number
  maxRows: number
  maxColumns: number
  maxPixelDimension: number
  nonblockingOutput: boolean
  fixedOpenSshProgram: boolean
}

export interface TerminalSize {
  rows: number
  columns: number
  pixelWidth: number
  pixelHeight: number
}

export interface TerminalStatus {
  state: 'running' | 'exited' | 'disconnected' | 'closed_by_client'
  detail: string | null
  transcriptRetainedBytes: number
  transcriptDroppedBytes: number
}

export interface OpenedTerminal {
  sessionId: string
  capabilities: TerminalCapabilities
  status: TerminalStatus
}

export type TerminalRead =
  | { status: 'pending' }
  | { status: 'data'; encodedData: string }
  | { status: 'end_of_stream' }

export type RemoteFetchResult<T> =
  | { kind: 'data'; data: T }
  | { kind: 'error'; error: BridgeError }

export const noteStatuses = ['draft', 'active', 'completed', 'archived'] as const
export type NoteStatus = (typeof noteStatuses)[number]

export const noteDeletedFilters = ['exclude', 'include', 'only'] as const
export type NoteDeletedFilter = (typeof noteDeletedFilters)[number]

export const noteSorts = ['updated_desc', 'created_desc', 'title_asc', 'diary_date_desc'] as const
export type NoteSort = (typeof noteSorts)[number]

export const noteExportFormats = ['markdown', 'json'] as const
export type NoteExportFormat = (typeof noteExportFormats)[number]

export interface NoteDraftMeta {
  title: string
  diaryDate: string | null
  tags: string[]
  status: NoteStatus
  pinned: boolean
}

export interface NoteSummary {
  id: string
  title: string
  diaryDate: string | null
  tags: string[]
  status: NoteStatus
  pinned: boolean
  createdAtMs: number
  updatedAtMs: number
  deletedAtMs: number | null
  revision: number
  bodyBytes: number
  bodySha256: string
}

export interface NoteDocument {
  summary: NoteSummary
  bodyMarkdown: string
}

export interface NoteQuery {
  search: string | null
  diaryDateFrom: string | null
  diaryDateTo: string | null
  tags: string[]
  status: NoteStatus | null
  deleted: NoteDeletedFilter
  sort: NoteSort
  limit: number
  offset: number
}

export interface NotePage {
  query: NoteQuery
  notes: NoteSummary[]
  hasMore: boolean
  nextOffset: number | null
}

export interface NoteExport {
  format: NoteExportFormat
  content: string
  contentBytes: number
  contentSha256: string
}

export type NoteWriteInput =
  | { kind: 'create'; meta: NoteDraftMeta; bodyMarkdown: string }
  | {
      kind: 'save'
      id: string
      expectedRevision: number
      autosave: boolean
      meta: NoteDraftMeta
      bodyMarkdown: string
    }

export type NoteMutationResult =
  | { kind: 'stored'; note: NoteSummary }
  | { kind: 'deleted'; note: NoteSummary }
  | { kind: 'restored'; note: NoteSummary }
  | { kind: 'conflict'; expectedRevision: number; current: NoteSummary }
  | { kind: 'upload_begun'; uploadId: string; maxChunkRawBytes: number }
  | { kind: 'upload_accepted'; uploadId: string; nextSequence: number; nextOffset: number }
  | { kind: 'upload_aborted'; uploadId: string }

export type NoteListResult =
  | { kind: 'page'; page: NotePage }
  | { kind: 'error'; error: BridgeError }

export type NoteGetResult =
  | { kind: 'document'; document: NoteDocument }
  | { kind: 'error'; error: BridgeError }

export type NoteMutationFetchResult =
  | { kind: 'mutation'; result: NoteMutationResult }
  | { kind: 'error'; error: BridgeError }

export type NoteExportResult =
  | { kind: 'export'; export: NoteExport }
  | { kind: 'error'; error: BridgeError }

export const transferStateKinds = [
  'queued',
  'running',
  'pausing',
  'paused',
  'cancelling',
  'retry_scheduled',
  'conflict',
  'completed',
  'failed',
  'cancelled',
] as const
export type TransferStateKind = (typeof transferStateKinds)[number]

export const transferDirections = ['upload', 'download'] as const
export type TransferDirection = (typeof transferDirections)[number]

export const conflictPolicies = ['fail', 'overwrite', 'rename', 'resume'] as const
export type ConflictPolicy = (typeof conflictPolicies)[number]

export const verificationLevels = ['size', 'remote_identity', 'checksum', 'unverified'] as const
export type VerificationLevel = (typeof verificationLevels)[number]

export const remoteErrorKinds = [
  'transport',
  'trust',
  'authentication',
  'permission_denied',
  'not_found',
  'conflict',
  'unsupported',
  'rate_limited',
  'timeout',
  'remote_protocol',
  'cancelled',
  'invalid_input',
  'secret_store',
] as const
export type RemoteErrorKind = (typeof remoteErrorKinds)[number]

export const remoteOperations = [
  'connect',
  'disconnect',
  'list',
  'stat',
  'read',
  'write',
  'create_directory',
  'rename',
  'delete',
  'resume',
  'resolve_secret',
  'delete_secret',
] as const
export type RemoteOperation = (typeof remoteOperations)[number]

export const retryDispositions = ['never', 'backoff', 'reauthenticate', 'user_action'] as const
export type RetryDisposition = (typeof retryDispositions)[number]

export const resumeValidationValues = ['remote_identity', 'size_only'] as const
export type ResumeValidation = (typeof resumeValidationValues)[number]

export const localHandlePurposes = ['upload_source', 'download_destination'] as const
export type TransferLocalHandlePurpose = (typeof localHandlePurposes)[number]

export type TransferId = string
export type LocalFileHandle = string

export interface ObjectIdentity {
  sizeBytes: number | null
  modifiedAtUnixMs: number | null
  etag: string | null
}

export interface TransferRetryPolicy {
  maxAttempts: number
  initialBackoffMs: number
  maxBackoffMs: number
}

export type TransferDraftEndpoint =
  | { kind: 'local'; handle: LocalFileHandle }
  | { kind: 'remote'; profileId: string; path: string }

export interface TransferDraft {
  id: TransferId
  source: TransferDraftEndpoint
  destination: TransferDraftEndpoint
  direction: TransferDirection
  expectedSource: ObjectIdentity | null
  expectedDestination: ObjectIdentity | null
  retryPolicy: TransferRetryPolicy
  bandwidthLimit: number | null
  conflictPolicy: ConflictPolicy
}

export type TransferFeatureSupport =
  | { status: 'supported' }
  | { status: 'unsupported'; reason: string }

export interface TransferFeatureSet {
  pause: TransferFeatureSupport
  resume: TransferFeatureSupport
  resumeValidation: ResumeValidation | null
}

export interface TransferProgress {
  bytesTransferred: number
  totalBytes: number | null
  bytesPerSecond: number | null
  sampledAtUnixMs: number | null
}

export interface TransferCheckpoint {
  offset: number
  sourceIdentity: ObjectIdentity | null
  destinationIdentity: ObjectIdentity | null
  verification: VerificationLevel
  verifiedAtUnixMs: number
}

export interface TransferFailure {
  kind: RemoteErrorKind
  operation: RemoteOperation
  reason: string
  retry: RetryDisposition
}

export interface TransferCompletion {
  verification: VerificationLevel
  identity: ObjectIdentity | null
  completedAtUnixMs: number
}

export interface TransferConflict {
  reason: string
  checkpoint: TransferCheckpoint | null
}

export type TransferState =
  | { status: 'queued' }
  | { status: 'running' }
  | { status: 'pausing' }
  | { status: 'paused'; checkpoint: TransferCheckpoint }
  | { status: 'cancelling' }
  | { status: 'retry_scheduled'; notBeforeUnixMs: number; failure: TransferFailure }
  | { status: 'conflict'; conflict: TransferConflict }
  | { status: 'completed'; completion: TransferCompletion }
  | { status: 'failed'; failure: TransferFailure }
  | { status: 'cancelled'; checkpoint: TransferCheckpoint | null; cancelledAtUnixMs: number }

export type TransferEndpoint =
  | { kind: 'local'; handle: LocalFileHandle }
  | { kind: 'remote'; profileId: string; protocol: RemoteProtocol; path: string }

export interface TransferTask {
  id: TransferId
  source: TransferEndpoint
  destination: TransferEndpoint
  direction: TransferDirection
  expectedSource: ObjectIdentity | null
  expectedDestination: ObjectIdentity | null
  state: TransferState
  progress: TransferProgress
  retryPolicy: TransferRetryPolicy
  completedAttempts: number
  bandwidthLimit: number | null
  conflictPolicy: ConflictPolicy
  features: TransferFeatureSet
  revision: number
  createdAtMs: number
  updatedAtMs: number
}

export interface TransferQuery {
  limit: number
  offset: number
  states: TransferStateKind[]
  direction: TransferDirection | null
  profileId: string | null
}

export interface TransferPage {
  query: TransferQuery
  tasks: TransferTask[]
  hasMore: boolean
  nextOffset: number | null
}

export type TransferMutationResult =
  | { result: 'updated'; task: TransferTask }
  | { result: 'conflict'; expectedRevision: number; current: TransferTask }

export interface TransferLocalHandleGrant {
  handle: LocalFileHandle
  purpose: TransferLocalHandlePurpose
  displayName: string
  sizeBytes: number | null
}

export type TransferListResult =
  | { kind: 'page'; page: TransferPage }
  | { kind: 'error'; error: BridgeError }

export type TransferTaskResult =
  | { kind: 'task'; task: TransferTask }
  | { kind: 'error'; error: BridgeError }

export type TransferMutationFetchResult =
  | { kind: 'mutation'; result: TransferMutationResult }
  | { kind: 'error'; error: BridgeError }

export type TransferPickResult =
  | { kind: 'picked'; grant: TransferLocalHandleGrant | null }
  | { kind: 'error'; error: BridgeError }

// ---- 网络测速 ----

export const speedTestStageValues = ['latency', 'bandwidth', 'ip_purity'] as const
export type SpeedTestStage = (typeof speedTestStageValues)[number]

export interface LatencyProbe {
  connectMs: number | null
  ttfbMs: number | null
  httpCode: number | null
  error: string | null
}

export interface LatencyTargetResult {
  host: string
  probes: LatencyProbe[]
  avgTtfbMs: number | null
}

export const bandwidthKindValues = ['international', 'domestic'] as const
export type BandwidthKind = (typeof bandwidthKindValues)[number]

export interface BandwidthMeasurement {
  kind: BandwidthKind
  label: string
  source: string
  downloadBitsPerSecond: number | null
  uploadBitsPerSecond: number | null
  httpCode: number | null
  error: string | null
}

export interface IpRiskSource {
  source: string
  risk: number | null
  weight: number | null
}

export interface IpPurityResult {
  source: string
  ip: string | null
  country: string | null
  region: string | null
  city: string | null
  isp: string | null
  org: string | null
  asn: string | null
  asname: string | null
  proxy: boolean | null
  hosting: boolean | null
  mobile: boolean | null
  riskScore: number | null
  ipType: string | null
  signals: string[]
  riskSources: IpRiskSource[]
  blocklistChecked: number | null
  blocklistListed: string[]
  riskError: string | null
  error: string | null
}

export type SpeedTestStageData =
  | { stage: 'latency'; payload: { targets: LatencyTargetResult[] } }
  | { stage: 'bandwidth'; payload: { measurements: BandwidthMeasurement[] } }
  | { stage: 'ip_purity'; payload: { purity: IpPurityResult } }

export interface SpeedTestBasicEnd {
  schemaVersion: number
  startedAtUnixMs: number
  endedAtUnixMs: number
  stages: SpeedTestStageData[]
  cancelled: boolean
  error: string | null
}

export const iperf3DirectionValues = ['download', 'upload', 'bidirectional'] as const
export type Iperf3Direction = (typeof iperf3DirectionValues)[number]

export type SpeedTestDeepCommand =
  | {
      command: 'iperf3_start'
      params: {
        server: string
        port: number
        direction: Iperf3Direction
        duration_secs: number
        parallel: number
      }
    }
  | { command: 'iperf3_stop'; params: null }
  | { command: 'wifi_scan'; params: null }
  | { command: 'linssid_launch'; params: null }

export interface Iperf3Result {
  server: string
  port: number
  direction: Iperf3Direction
  durationSecs: number
  parallel: number
  startedAtUnixMs: number
  endedAtUnixMs: number
  downloadBitsPerSecond: number | null
  uploadBitsPerSecond: number | null
  retransmits: number | null
  jitterMs: number | null
  error: string | null
}

export interface WifiNetwork {
  ssid: string
  signalPercent: number | null
  signalDbm: number | null
  signalBars: string | null
  channel: number | null
  band: string | null
  security: string | null
}

export interface WifiScanResult {
  scannedAtUnixMs: number
  source: string
  networks: WifiNetwork[]
  error: string | null
}

export interface LinssidLaunchResult {
  launched: boolean
  executable: string | null
  reason: string
}

export type SpeedTestDeepOutput =
  | { type: 'iperf3'; payload: Iperf3Result }
  | { type: 'wifi_scan'; payload: WifiScanResult }
  | { type: 'linssid'; payload: LinssidLaunchResult }

export interface SpeedTestCancelResult {
  cancelled: boolean
  reason: string
}

export type SpeedTestBasicFetchResult =
  | { kind: 'end'; end: SpeedTestBasicEnd }
  | { kind: 'error'; error: BridgeError }

export type SpeedTestCancelFetchResult =
  | { kind: 'cancelled'; result: SpeedTestCancelResult }
  | { kind: 'error'; error: BridgeError }

export type SpeedTestDeepFetchResult =
  | { kind: 'output'; output: SpeedTestDeepOutput }
  | { kind: 'error'; error: BridgeError }
