use crate::native::{
    NativeAuth, NativeDialect, NativeEntry, NativeEntryKind, NativeError, NativeErrorKind,
    NativeReadChunk, NativeSmbConfig, NativeSmbConnector, NativeWriteChunk, SmbClient,
    SmbConnector,
};
use crate::{
    Authentication as DiagnosticAuthentication, Authority, CapabilityReport, CapabilityStatus,
    CredentialRevision, DiagnosticOperation, DiagnosticPlan, DiagnosticRequest, ErrorKind,
    MinimumDialect, Protection, Secret, build_plan, probe_smbclient,
};
use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, BeginWriteRequest, CapabilityMatrix,
    CapabilityStatus as CoreCapabilityStatus, ConnectionState, FILE_OPERATIONS, FileOperation,
    MAX_REMOTE_CHUNK_BYTES, OperationCapability, ProfileOptions, RemoteConnectionProfile,
    RemoteEntry, RemoteError, RemoteErrorKind, RemoteFileAdapter, RemoteFileSession,
    RemoteIoControl, RemoteIoControlSupport, RemoteOperation, RemotePath, RemoteProtocol,
    RemoteReadChunk, RemoteReadRequest, RemoteSession, RemoteWriteHandle, RemoteWriteReceipt,
    RetryDisposition, SafeReason, SecretStore, SecretStoreError, SessionId, SmbDialect,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DIAGNOSTIC_ONLY: &str = "smb_file_adapter_diagnostic_only";
const CLIENT_NOT_INSTALLED: &str = "smbclient_not_installed";
const CLIENT_PROBE_FAILED: &str = "smbclient_probe_failed";
const CLIENT_UNREACHABLE: &str = "smbclient_executable_unreachable";
const NATIVE_NOT_INSTALLED: &str = "libsmbclient_not_installed";
const NATIVE_INCOMPATIBLE: &str = "libsmbclient_api_incompatible";
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct SmbRemoteFileAdapter {
    program: PathBuf,
    report: CapabilityReport,
    capabilities: CapabilityMatrix,
    connector: Option<Arc<dyn SmbConnector>>,
    availability: AdapterAvailability,
}

impl SmbRemoteFileAdapter {
    pub fn system() -> Self {
        Self::with_program("/usr/sbin/smbclient")
    }

    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let report = probe_smbclient(&program);
        match NativeSmbConnector::load() {
            Ok(connector) => Self {
                program,
                report,
                capabilities: production_capability_matrix(),
                connector: Some(Arc::new(connector)),
                availability: AdapterAvailability::Healthy,
            },
            Err(error) => {
                let availability = match error.kind {
                    NativeErrorKind::LibraryMissing => {
                        AdapterAvailability::Unsupported(reason(NATIVE_NOT_INSTALLED))
                    }
                    _ => AdapterAvailability::Unsupported(reason(NATIVE_INCOMPATIBLE)),
                };
                Self {
                    program,
                    report,
                    capabilities: unsupported_capability_matrix(),
                    connector: None,
                    availability,
                }
            }
        }
    }

    pub fn from_report(program: impl Into<PathBuf>, report: CapabilityReport) -> Self {
        let availability = match report.status {
            CapabilityStatus::Healthy => AdapterAvailability::Degraded(reason(DIAGNOSTIC_ONLY)),
            CapabilityStatus::Degraded => {
                AdapterAvailability::Degraded(reason(CLIENT_PROBE_FAILED))
            }
            CapabilityStatus::Unsupported => {
                AdapterAvailability::Unsupported(reason(CLIENT_NOT_INSTALLED))
            }
            CapabilityStatus::Unreachable => {
                AdapterAvailability::Unreachable(reason(CLIENT_UNREACHABLE))
            }
        };
        Self {
            program: program.into(),
            report,
            capabilities: unsupported_capability_matrix(),
            connector: None,
            availability,
        }
    }

    pub fn diagnostic_report(&self) -> &CapabilityReport {
        &self.report
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn prepare_diagnostic<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
    ) -> AdapterFuture<'a, Result<DiagnosticPlan, RemoteError>> {
        Box::pin(async move {
            self.ensure_diagnostic_available()?;
            profile.validate().map_err(|_| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "invalid_smb_profile",
                    RetryDisposition::UserAction,
                )
            })?;
            if profile.protocol != RemoteProtocol::Smb {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "profile_protocol_is_not_smb",
                    RetryDisposition::Never,
                ));
            }

            let ProfileOptions::Smb {
                share,
                minimum_dialect,
                require_signing,
                require_encryption,
            } = &profile.options
            else {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "profile_options_are_not_smb",
                    RetryDisposition::Never,
                ));
            };

            let authentication = match &profile.authentication {
                localdesk_remote_core::Authentication::Password { secret } => {
                    let username = profile.username.clone().ok_or_else(|| {
                        remote_error(
                            RemoteErrorKind::InvalidInput,
                            RemoteOperation::Connect,
                            "smb_password_username_missing",
                            RetryDisposition::UserAction,
                        )
                    })?;
                    let value = secrets
                        .resolve(secret)
                        .await
                        .map_err(map_secret_store_error)?;
                    let password =
                        String::from_utf8(value.expose_secret().to_vec()).map_err(|_| {
                            remote_error(
                                RemoteErrorKind::SecretStore,
                                RemoteOperation::ResolveSecret,
                                "smb_password_not_utf8",
                                RetryDisposition::UserAction,
                            )
                        })?;
                    DiagnosticAuthentication::Password {
                        username,
                        password: Secret::new(password),
                        authority: profile
                            .domain
                            .clone()
                            .map(Authority::Domain)
                            .unwrap_or(Authority::Default),
                    }
                }
                localdesk_remote_core::Authentication::Kerberos => {
                    DiagnosticAuthentication::Kerberos {
                        realm: profile.domain.clone(),
                        ccache: None,
                    }
                }
                _ => {
                    return Err(remote_error(
                        RemoteErrorKind::InvalidInput,
                        RemoteOperation::Connect,
                        "unsupported_smb_authentication",
                        RetryDisposition::UserAction,
                    ));
                }
            };

            let operation = match share {
                Some(share) => DiagnosticOperation::InspectShare {
                    server: profile.endpoint.host().to_owned(),
                    share: share.clone(),
                },
                None => DiagnosticOperation::BrowseShares {
                    server: profile.endpoint.host().to_owned(),
                },
            };
            let protection = if *require_encryption {
                Protection::Encryption
            } else if *require_signing {
                Protection::Signing
            } else {
                Protection::Negotiated
            };
            let mut request = DiagnosticRequest::new(
                authentication,
                protection,
                CredentialRevision {
                    expected: 0,
                    active: 0,
                },
                operation,
            );
            request.port = profile.endpoint.port;
            request.minimum_dialect = match minimum_dialect {
                SmbDialect::Smb2 => MinimumDialect::Smb2,
                SmbDialect::Smb3 => MinimumDialect::Smb3,
            };

            build_plan(&self.program, request).map_err(map_diagnostic_error)
        })
    }

    fn ensure_diagnostic_available(&self) -> Result<(), RemoteError> {
        match self.report.status {
            CapabilityStatus::Healthy => Ok(()),
            CapabilityStatus::Degraded => Err(remote_error(
                RemoteErrorKind::Transport,
                RemoteOperation::Connect,
                CLIENT_PROBE_FAILED,
                RetryDisposition::UserAction,
            )),
            CapabilityStatus::Unsupported => Err(remote_error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                CLIENT_NOT_INSTALLED,
                RetryDisposition::UserAction,
            )),
            CapabilityStatus::Unreachable => Err(remote_error(
                RemoteErrorKind::Transport,
                RemoteOperation::Connect,
                CLIENT_UNREACHABLE,
                RetryDisposition::UserAction,
            )),
        }
    }
}

impl RemoteFileAdapter for SmbRemoteFileAdapter {
    fn protocol(&self) -> RemoteProtocol {
        RemoteProtocol::Smb
    }

    fn availability(&self) -> AdapterAvailability {
        self.availability.clone()
    }

    fn capabilities(&self) -> &CapabilityMatrix {
        &self.capabilities
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn connect<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        self.connect_controlled(profile, secrets, default_operation_control())
    }

    fn connect_controlled<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Connect)?;
            let timeout_ms = control_timeout_ms(&control, RemoteOperation::Connect)?;
            let connector = self
                .connector
                .as_ref()
                .ok_or_else(|| unavailable_connect_error(&self.availability))?;
            profile.validate().map_err(|_| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "invalid_smb_profile",
                    RetryDisposition::UserAction,
                )
            })?;
            if profile.protocol != RemoteProtocol::Smb {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "profile_protocol_is_not_smb",
                    RetryDisposition::Never,
                ));
            }
            let ProfileOptions::Smb {
                share,
                minimum_dialect,
                require_signing,
                require_encryption,
            } = &profile.options
            else {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "profile_options_are_not_smb",
                    RetryDisposition::Never,
                ));
            };
            let share = share.clone().ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "smb_share_required_for_file_session",
                    RetryDisposition::UserAction,
                )
            })?;
            let auth = match &profile.authentication {
                localdesk_remote_core::Authentication::Password { secret } => {
                    let username = profile.username.clone().ok_or_else(|| {
                        remote_error(
                            RemoteErrorKind::InvalidInput,
                            RemoteOperation::Connect,
                            "smb_password_username_missing",
                            RetryDisposition::UserAction,
                        )
                    })?;
                    let password = secrets
                        .resolve(secret)
                        .await
                        .map_err(map_secret_store_error)?;
                    std::str::from_utf8(password.expose_secret()).map_err(|_| {
                        remote_error(
                            RemoteErrorKind::InvalidInput,
                            RemoteOperation::ResolveSecret,
                            "smb_password_not_utf8",
                            RetryDisposition::UserAction,
                        )
                    })?;
                    NativeAuth::Password {
                        username,
                        domain: profile.domain.clone(),
                        password,
                    }
                }
                localdesk_remote_core::Authentication::Kerberos => {
                    if profile.domain.is_some() {
                        return Err(remote_error(
                            RemoteErrorKind::Unsupported,
                            RemoteOperation::Connect,
                            "smb_kerberos_realm_not_configurable_per_session",
                            RetryDisposition::UserAction,
                        ));
                    }
                    NativeAuth::Kerberos
                }
                _ => {
                    return Err(remote_error(
                        RemoteErrorKind::InvalidInput,
                        RemoteOperation::Connect,
                        "unsupported_smb_authentication",
                        RetryDisposition::UserAction,
                    ));
                }
            };
            let config = NativeSmbConfig {
                host: profile.endpoint.host().to_owned(),
                port: profile.endpoint.port,
                share,
                dialect: match minimum_dialect {
                    SmbDialect::Smb2 => NativeDialect::Smb2,
                    SmbDialect::Smb3 => NativeDialect::Smb3,
                },
                // libsmbclient has no context-local signing-only setter. Requiring
                // encryption is a stronger per-session policy and also guarantees signing.
                require_protection: *require_signing || *require_encryption,
                timeout_ms,
                auth,
            };
            let mut client = connector
                .connect(config)
                .map_err(|error| map_native_error(error, RemoteOperation::Connect))?;
            if let Err(error) = control.check(RemoteOperation::Connect) {
                client.disconnect();
                return Err(error);
            }
            let now = unix_time_ms();
            let snapshot = RemoteSession {
                id: SessionId::new(),
                profile_id: profile.id,
                protocol: RemoteProtocol::Smb,
                state: ConnectionState::Ready,
                capabilities: self.capabilities.clone(),
                opened_at_unix_ms: now,
                updated_at_unix_ms: now,
            };
            Ok(Box::new(SmbFileSession {
                id: snapshot.id,
                state: Mutex::new(SmbSessionState {
                    snapshot,
                    client: Some(client),
                    writes: HashMap::new(),
                }),
            }) as Box<dyn RemoteFileSession>)
        })
    }
}

struct SmbFileSession {
    id: SessionId,
    state: Mutex<SmbSessionState>,
}

struct SmbSessionState {
    snapshot: RemoteSession,
    client: Option<Box<dyn SmbClient>>,
    writes: HashMap<RemoteWriteHandle, SmbWriteState>,
}

#[derive(Clone)]
struct SmbWriteState {
    final_path: RemotePath,
    temporary_path: RemotePath,
    expected_size_bytes: Option<u64>,
    expected_destination: Option<localdesk_remote_core::ObjectIdentity>,
    next_offset: u64,
    identity: localdesk_remote_core::ObjectIdentity,
}

impl SmbSessionState {
    fn client(
        &mut self,
        operation: RemoteOperation,
    ) -> Result<&mut (dyn SmbClient + 'static), RemoteError> {
        if self.snapshot.state != ConnectionState::Ready {
            return Err(remote_error(
                RemoteErrorKind::Cancelled,
                operation,
                "smb_session_not_ready",
                RetryDisposition::Never,
            ));
        }
        self.client.as_deref_mut().ok_or_else(|| {
            remote_error(
                RemoteErrorKind::Cancelled,
                operation,
                "smb_session_not_ready",
                RetryDisposition::Never,
            )
        })
    }
}

impl RemoteFileSession for SmbFileSession {
    fn id(&self) -> SessionId {
        self.id
    }

    fn snapshot(&self) -> RemoteSession {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot
            .clone()
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn list<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .client(RemoteOperation::List)?
                .list(path.as_str())
                .map(|entries| entries.into_iter().map(remote_entry).collect())
                .map_err(|error| map_native_error(error, RemoteOperation::List))
        })
    }

    fn stat<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .client(RemoteOperation::Stat)?
                .stat(path.as_str())
                .map(remote_entry)
                .map_err(|error| map_native_error(error, RemoteOperation::Stat))
        })
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .client(RemoteOperation::CreateDirectory)?
                .create_directory(path.as_str())
                .map(remote_entry)
                .map_err(|error| map_native_error(error, RemoteOperation::CreateDirectory))
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a RemotePath,
        to: &'a RemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .client(RemoteOperation::Rename)?
                .rename(from.as_str(), to.as_str())
                .map(remote_entry)
                .map_err(|error| map_native_error(error, RemoteOperation::Rename))
        })
    }

    fn delete<'a>(&'a self, path: &'a RemotePath) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .client(RemoteOperation::Delete)?
                .delete(path.as_str())
                .map_err(|error| map_native_error(error, RemoteOperation::Delete))
        })
    }

    fn read_chunk<'a>(
        &'a self,
        request: RemoteReadRequest,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        self.read_chunk_controlled(request, default_operation_control())
    }

    fn begin_write<'a>(
        &'a self,
        request: BeginWriteRequest,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.begin_write_controlled(request, default_operation_control())
    }

    fn write_chunk<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        self.write_chunk_controlled(handle, offset, bytes, default_operation_control())
    }

    fn commit_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<localdesk_remote_core::ObjectIdentity>,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        self.commit_write_controlled(handle, expected_identity, default_operation_control())
    }

    fn abort_write<'a>(
        &'a self,
        handle: RemoteWriteHandle,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        self.abort_write_controlled(handle, default_operation_control())
    }

    fn read_chunk_controlled<'a>(
        &'a self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteReadChunk, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Read)?;
            if !request.is_bounded() {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Read,
                    "smb_read_chunk_unbounded",
                    RetryDisposition::Never,
                ));
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let client = state.client(RemoteOperation::Read)?;
            client.set_timeout_ms(control_timeout_ms(&control, RemoteOperation::Read)?);
            let NativeReadChunk {
                before,
                bytes,
                after,
            } = client
                .read_chunk(request.path.as_str(), request.offset, request.max_bytes)
                .map_err(|error| map_native_error(error, RemoteOperation::Read))?;
            let before = native_identity(&before);
            let after = native_identity(&after);
            ensure_identity(
                request.expected_identity.as_ref(),
                Some(&before),
                RemoteOperation::Read,
            )?;
            ensure_identity(Some(&before), Some(&after), RemoteOperation::Read)?;
            control.check(RemoteOperation::Read)?;
            let next_offset = request
                .offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| {
                    remote_error(
                        RemoteErrorKind::InvalidInput,
                        RemoteOperation::Read,
                        "smb_read_offset_overflow",
                        RetryDisposition::Never,
                    )
                })?;
            Ok(RemoteReadChunk {
                offset: request.offset,
                eof: after.size_bytes.is_some_and(|size| next_offset >= size),
                bytes,
                identity: after,
            })
        })
    }

    fn begin_write_controlled<'a>(
        &'a self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let offset = request.resume_from.unwrap_or(0);
            if request.final_path == request.temporary_path
                || !request.temporary_path.as_str().ends_with(".part")
            {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_temporary_path_must_be_part",
                    RetryDisposition::Never,
                ));
            }
            if request
                .expected_size_bytes
                .is_some_and(|size| offset > size)
            {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Resume,
                    "smb_resume_offset_exceeds_expected_size",
                    RetryDisposition::Never,
                ));
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let client = state.client(RemoteOperation::Write)?;
            client.set_timeout_ms(control_timeout_ms(&control, RemoteOperation::Write)?);
            let destination = optional_native_entry(client.stat(request.final_path.as_str()))
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?
                .as_ref()
                .map(native_identity);
            ensure_identity(
                request.expected_destination.as_ref(),
                destination.as_ref(),
                RemoteOperation::Write,
            )?;
            let temporary = client
                .prepare_write(
                    request.temporary_path.as_str(),
                    request.resume_from.is_some(),
                )
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?;
            let identity = native_identity(&temporary);
            if identity.size_bytes != Some(offset) {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "smb_resume_offset_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            control.check(RemoteOperation::Write)?;
            let handle = RemoteWriteHandle::new();
            state.writes.insert(
                handle,
                SmbWriteState {
                    final_path: request.final_path,
                    temporary_path: request.temporary_path,
                    expected_size_bytes: request.expected_size_bytes,
                    expected_destination: request.expected_destination,
                    next_offset: offset,
                    identity: identity.clone(),
                },
            );
            Ok(RemoteWriteReceipt {
                handle,
                next_offset: offset,
                identity: Some(identity),
            })
        })
    }

    fn write_chunk_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            if bytes.is_empty() || bytes.len() > MAX_REMOTE_CHUNK_BYTES as usize {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_chunk_invalid_size",
                    RetryDisposition::Never,
                ));
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let pending = state.writes.get(&handle).cloned().ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_handle_not_found",
                    RetryDisposition::Never,
                )
            })?;
            if pending.next_offset != offset {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "smb_write_offset_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            let next_offset = offset.checked_add(bytes.len() as u64).ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_offset_overflow",
                    RetryDisposition::Never,
                )
            })?;
            if pending
                .expected_size_bytes
                .is_some_and(|size| next_offset > size)
            {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_exceeds_expected_size",
                    RetryDisposition::Never,
                ));
            }
            let client = state.client(RemoteOperation::Write)?;
            client.set_timeout_ms(control_timeout_ms(&control, RemoteOperation::Write)?);
            let NativeWriteChunk { before, after } = client
                .write_chunk(pending.temporary_path.as_str(), offset, &bytes)
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?;
            let before = native_identity(&before);
            let after = native_identity(&after);
            ensure_identity(
                Some(&pending.identity),
                Some(&before),
                RemoteOperation::Write,
            )?;
            if after.size_bytes != Some(next_offset) {
                return Err(remote_error(
                    RemoteErrorKind::RemoteProtocol,
                    RemoteOperation::Write,
                    "smb_written_size_mismatch",
                    RetryDisposition::Never,
                ));
            }
            control.check(RemoteOperation::Write)?;
            let current = state.writes.get_mut(&handle).ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_handle_not_found",
                    RetryDisposition::Never,
                )
            })?;
            if current.next_offset != offset {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Resume,
                    "smb_write_state_changed",
                    RetryDisposition::UserAction,
                ));
            }
            current.next_offset = next_offset;
            current.identity = after.clone();
            Ok(RemoteWriteReceipt {
                handle,
                next_offset,
                identity: Some(after),
            })
        })
    }

    fn commit_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        expected_identity: Option<localdesk_remote_core::ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let pending = state.writes.get(&handle).cloned().ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Write,
                    "smb_write_handle_not_found",
                    RetryDisposition::Never,
                )
            })?;
            if pending
                .expected_size_bytes
                .is_some_and(|size| size != pending.next_offset)
            {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "smb_commit_size_incomplete",
                    RetryDisposition::UserAction,
                ));
            }
            let expected_destination = expected_identity.or(pending.expected_destination.clone());
            let client = state.client(RemoteOperation::Write)?;
            client.set_timeout_ms(control_timeout_ms(&control, RemoteOperation::Write)?);
            let destination = optional_native_entry(client.stat(pending.final_path.as_str()))
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?
                .as_ref()
                .map(native_identity);
            ensure_identity(
                expected_destination.as_ref(),
                destination.as_ref(),
                RemoteOperation::Write,
            )?;
            let temporary = client
                .stat(pending.temporary_path.as_str())
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?;
            ensure_identity(
                Some(&pending.identity),
                Some(&native_identity(&temporary)),
                RemoteOperation::Write,
            )?;
            let committed = client
                .rename(pending.temporary_path.as_str(), pending.final_path.as_str())
                .map_err(|error| map_native_error(error, RemoteOperation::Write))?;
            if committed.size_bytes != Some(pending.next_offset) {
                return Err(remote_error(
                    RemoteErrorKind::RemoteProtocol,
                    RemoteOperation::Write,
                    "smb_committed_size_mismatch",
                    RetryDisposition::Never,
                ));
            }
            state.writes.remove(&handle);
            Ok(remote_entry(committed))
        })
    }

    fn abort_write_controlled<'a>(
        &'a self,
        handle: RemoteWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Delete)?;
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let pending = state.writes.get(&handle).cloned().ok_or_else(|| {
                remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Delete,
                    "smb_write_handle_not_found",
                    RetryDisposition::Never,
                )
            })?;
            let client = state.client(RemoteOperation::Delete)?;
            client.set_timeout_ms(control_timeout_ms(&control, RemoteOperation::Delete)?);
            match client.delete(pending.temporary_path.as_str()) {
                Ok(()) => {}
                Err(error) if error.kind == NativeErrorKind::NotFound => {}
                Err(error) => return Err(map_native_error(error, RemoteOperation::Delete)),
            }
            state.writes.remove(&handle);
            Ok(())
        })
    }

    fn disconnect<'a>(&'a self) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.snapshot.state == ConnectionState::Disconnected {
                return Ok(());
            }
            let now = unix_time_ms().max(state.snapshot.updated_at_unix_ms);
            state
                .snapshot
                .transition(ConnectionState::Closing, now)
                .map_err(|_| {
                    remote_error(
                        RemoteErrorKind::RemoteProtocol,
                        RemoteOperation::Disconnect,
                        "smb_session_disconnect_transition_failed",
                        RetryDisposition::Never,
                    )
                })?;
            if let Some(mut client) = state.client.take() {
                client.disconnect();
            }
            state.writes.clear();
            state
                .snapshot
                .transition(ConnectionState::Disconnected, now)
                .map_err(|_| {
                    remote_error(
                        RemoteErrorKind::RemoteProtocol,
                        RemoteOperation::Disconnect,
                        "smb_session_disconnect_transition_failed",
                        RetryDisposition::Never,
                    )
                })?;
            Ok(())
        })
    }

    fn disconnect_controlled<'a>(
        &'a self,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Disconnect)?;
            self.disconnect().await
        })
    }
}

fn unavailable_connect_error(availability: &AdapterAvailability) -> RemoteError {
    match availability {
        AdapterAvailability::Healthy => remote_error(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            DIAGNOSTIC_ONLY,
            RetryDisposition::Never,
        ),
        AdapterAvailability::Degraded(reason_value)
        | AdapterAvailability::Unsupported(reason_value) => RemoteError::new(
            RemoteErrorKind::Unsupported,
            RemoteOperation::Connect,
            reason_value.clone(),
            RetryDisposition::Never,
        ),
        AdapterAvailability::Unreachable(reason_value) => RemoteError::new(
            RemoteErrorKind::Transport,
            RemoteOperation::Connect,
            reason_value.clone(),
            RetryDisposition::Backoff,
        ),
    }
}

fn unsupported_capability_matrix() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: CoreCapabilityStatus::Unsupported(reason(capability_reason(operation))),
        }
    }))
    .expect("all remote-core file operations have an SMB capability answer")
}

fn production_capability_matrix() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        let status = match operation {
            FileOperation::List
            | FileOperation::Stat
            | FileOperation::Read
            | FileOperation::Write
            | FileOperation::CreateDirectory
            | FileOperation::Rename
            | FileOperation::Delete
            | FileOperation::ResumeRead
            | FileOperation::ResumeWrite
            | FileOperation::AtomicRename => CoreCapabilityStatus::Supported,
            _ => CoreCapabilityStatus::Unsupported(reason(capability_reason(operation))),
        };
        OperationCapability { operation, status }
    }))
    .expect("all remote-core file operations have a production SMB capability answer")
}

fn optional_native_entry(
    result: Result<NativeEntry, NativeError>,
) -> Result<Option<NativeEntry>, NativeError> {
    match result {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if error.kind == NativeErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn native_identity(entry: &NativeEntry) -> localdesk_remote_core::ObjectIdentity {
    localdesk_remote_core::ObjectIdentity {
        size_bytes: entry.size_bytes,
        modified_at_unix_ms: entry.modified_at_unix_ms,
        etag: None,
    }
}

fn identities_match(
    expected: Option<&localdesk_remote_core::ObjectIdentity>,
    actual: Option<&localdesk_remote_core::ObjectIdentity>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    actual.is_some_and(|actual| {
        expected
            .size_bytes
            .is_none_or(|value| actual.size_bytes == Some(value))
            && expected
                .modified_at_unix_ms
                .is_none_or(|value| actual.modified_at_unix_ms == Some(value))
            && expected
                .etag
                .as_ref()
                .is_none_or(|value| actual.etag.as_ref() == Some(value))
    })
}

fn ensure_identity(
    expected: Option<&localdesk_remote_core::ObjectIdentity>,
    actual: Option<&localdesk_remote_core::ObjectIdentity>,
    operation: RemoteOperation,
) -> Result<(), RemoteError> {
    if identities_match(expected, actual) {
        Ok(())
    } else {
        Err(remote_error(
            RemoteErrorKind::Conflict,
            operation,
            "smb_object_identity_changed",
            RetryDisposition::UserAction,
        ))
    }
}

fn default_operation_control() -> RemoteIoControl {
    RemoteIoControl::new(Instant::now() + DEFAULT_OPERATION_TIMEOUT)
}

fn control_timeout_ms(
    control: &RemoteIoControl,
    operation: RemoteOperation,
) -> Result<u32, RemoteError> {
    control.check(operation)?;
    let remaining = control.deadline().saturating_duration_since(Instant::now());
    let milliseconds = remaining
        .as_millis()
        .max(1)
        .min(DEFAULT_OPERATION_TIMEOUT.as_millis())
        .min(u128::from(u32::MAX));
    Ok(u32::try_from(milliseconds).expect("timeout was clamped to u32"))
}

fn capability_reason(operation: FileOperation) -> &'static str {
    match operation {
        FileOperation::List => "smb_list_requires_structured_remote_entry",
        FileOperation::Stat => "smb_stat_requires_structured_remote_entry",
        FileOperation::Read => "smb_read_requires_object_identity",
        FileOperation::Write => "smb_write_requires_object_identity",
        FileOperation::CreateDirectory => "smb_create_directory_not_implemented",
        FileOperation::Rename => "smb_rename_not_implemented",
        FileOperation::Delete => "smb_delete_not_implemented",
        FileOperation::ResumeRead => "smb_resume_read_not_identity_safe",
        FileOperation::ResumeWrite => "smb_resume_write_not_implemented",
        FileOperation::AtomicRename => "smb_atomic_rename_not_implemented",
        FileOperation::SetPermissions => "smb_set_permissions_not_implemented",
    }
}

fn remote_entry(entry: NativeEntry) -> RemoteEntry {
    RemoteEntry {
        name: entry.name,
        path: RemotePath::new(entry.path).expect("native SMB paths are validated before return"),
        kind: match entry.kind {
            NativeEntryKind::File => localdesk_remote_core::EntryKind::File,
            NativeEntryKind::Directory => localdesk_remote_core::EntryKind::Directory,
            NativeEntryKind::Symlink => localdesk_remote_core::EntryKind::Symlink,
            NativeEntryKind::Other => localdesk_remote_core::EntryKind::Other,
        },
        identity: localdesk_remote_core::ObjectIdentity {
            size_bytes: entry.size_bytes,
            modified_at_unix_ms: entry.modified_at_unix_ms,
            etag: None,
        },
        unix_mode: entry.unix_mode,
        capabilities: production_capability_matrix(),
    }
}

fn map_native_error(error: NativeError, operation: RemoteOperation) -> RemoteError {
    let (kind, retry) = match error.kind {
        NativeErrorKind::LibraryMissing | NativeErrorKind::ApiIncompatible => {
            (RemoteErrorKind::Unsupported, RetryDisposition::UserAction)
        }
        NativeErrorKind::InvalidInput => {
            (RemoteErrorKind::InvalidInput, RetryDisposition::UserAction)
        }
        NativeErrorKind::Authentication => (
            RemoteErrorKind::Authentication,
            RetryDisposition::Reauthenticate,
        ),
        NativeErrorKind::PermissionDenied => (
            RemoteErrorKind::PermissionDenied,
            RetryDisposition::UserAction,
        ),
        NativeErrorKind::NotFound => (RemoteErrorKind::NotFound, RetryDisposition::Never),
        NativeErrorKind::Conflict => (RemoteErrorKind::Conflict, RetryDisposition::UserAction),
        NativeErrorKind::Timeout => (RemoteErrorKind::Timeout, RetryDisposition::Backoff),
        NativeErrorKind::Transport => (RemoteErrorKind::Transport, RetryDisposition::Backoff),
        NativeErrorKind::Protocol | NativeErrorKind::Limit => {
            (RemoteErrorKind::RemoteProtocol, RetryDisposition::Backoff)
        }
    };
    RemoteError::new(
        kind,
        operation,
        SafeReason::new(error.reason).expect("native SMB reasons are static safe codes"),
        retry,
    )
}

fn map_secret_store_error(error: SecretStoreError) -> RemoteError {
    let (reason, retry) = match error {
        SecretStoreError::Locked(reason)
        | SecretStoreError::PermissionDenied(reason)
        | SecretStoreError::NotFound(reason) => (reason, RetryDisposition::UserAction),
        SecretStoreError::Unavailable(reason) | SecretStoreError::Backend(reason) => {
            (reason, RetryDisposition::Backoff)
        }
    };
    RemoteError::new(
        RemoteErrorKind::SecretStore,
        RemoteOperation::ResolveSecret,
        reason,
        retry,
    )
}

fn map_diagnostic_error(error: crate::DiagnosticError) -> RemoteError {
    let (kind, reason_code, retry) = match error.kind {
        ErrorKind::Unsupported => (
            RemoteErrorKind::Unsupported,
            "smbclient_not_installed",
            RetryDisposition::UserAction,
        ),
        ErrorKind::Unreachable | ErrorKind::Io => (
            RemoteErrorKind::Transport,
            "smbclient_execution_failed",
            RetryDisposition::Backoff,
        ),
        ErrorKind::Conflict => (
            RemoteErrorKind::Conflict,
            "smb_diagnostic_revision_conflict",
            RetryDisposition::Reauthenticate,
        ),
        ErrorKind::InvalidRequest => (
            RemoteErrorKind::InvalidInput,
            "smb_profile_not_supported_by_diagnostic_poc",
            RetryDisposition::UserAction,
        ),
        ErrorKind::TimedOut => (
            RemoteErrorKind::Timeout,
            "smbclient_diagnostic_timed_out",
            RetryDisposition::Backoff,
        ),
        ErrorKind::ClientRejected => (
            RemoteErrorKind::RemoteProtocol,
            "smbclient_diagnostic_rejected",
            RetryDisposition::UserAction,
        ),
    };
    remote_error(kind, RemoteOperation::Connect, reason_code, retry)
}

fn remote_error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    reason_code: &'static str,
    retry: RetryDisposition,
) -> RemoteError {
    RemoteError::new(kind, operation, reason(reason_code), retry)
}

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static SMB reason code must satisfy remote-core")
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_remote_core::{
        Authentication, ProfileId, RemoteEndpoint, SecretRef, SecretValue, TrustPolicy,
    };
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use uuid::Uuid;

    struct TestSecretStore {
        resolves: AtomicUsize,
    }

    impl SecretStore for TestSecretStore {
        fn resolve<'a>(
            &'a self,
            _reference: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
            self.resolves.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(SecretValue::new(b"production-secret".to_vec())) })
        }

        fn delete<'a>(
            &'a self,
            _reference: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    struct ObservedConfig {
        host: String,
        port: u16,
        share: String,
        username: String,
        domain: Option<String>,
        dialect: String,
        require_protection: bool,
    }

    struct TestConnector {
        observed: Arc<Mutex<Vec<ObservedConfig>>>,
    }

    impl SmbConnector for TestConnector {
        fn connect(&self, config: NativeSmbConfig) -> Result<Box<dyn SmbClient>, NativeError> {
            let NativeAuth::Password {
                username,
                domain,
                password,
            } = config.auth
            else {
                panic!("test requires password authentication")
            };
            assert_eq!(password.expose_secret(), b"production-secret");
            self.observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ObservedConfig {
                    host: config.host,
                    port: config.port,
                    share: config.share,
                    username,
                    domain,
                    dialect: match config.dialect {
                        NativeDialect::Smb2 => "SMB2".to_owned(),
                        NativeDialect::Smb3 => "SMB3".to_owned(),
                    },
                    require_protection: config.require_protection,
                });
            Ok(Box::new(TestClient::new()))
        }
    }

    struct TestClient {
        entries: HashMap<String, NativeEntry>,
        files: HashMap<String, Vec<u8>>,
        disconnected: bool,
    }

    impl TestClient {
        fn new() -> Self {
            let mut entries = HashMap::new();
            entries.insert(
                "/".to_owned(),
                entry("share", "/", NativeEntryKind::Directory),
            );
            entries.insert(
                "/report.txt".to_owned(),
                NativeEntry {
                    name: "report.txt".to_owned(),
                    path: "/report.txt".to_owned(),
                    kind: NativeEntryKind::File,
                    size_bytes: Some(128),
                    modified_at_unix_ms: Some(1_700_000_000_000),
                    unix_mode: Some(0o100640),
                },
            );
            let files = HashMap::from([("/report.txt".to_owned(), vec![b'x'; 128])]);
            Self {
                entries,
                files,
                disconnected: false,
            }
        }

        fn missing() -> NativeError {
            NativeError {
                kind: NativeErrorKind::NotFound,
                reason: "smb_path_not_found",
            }
        }
    }

    impl SmbClient for TestClient {
        fn set_timeout_ms(&mut self, timeout_ms: u32) {
            assert!(timeout_ms > 0);
        }

        fn list(&mut self, path: &str) -> Result<Vec<NativeEntry>, NativeError> {
            assert!(!self.disconnected);
            Ok(self
                .entries
                .values()
                .filter(|entry| {
                    path == "/" && entry.path != "/" && entry.path.matches('/').count() == 1
                })
                .cloned()
                .collect())
        }

        fn stat(&mut self, path: &str) -> Result<NativeEntry, NativeError> {
            assert!(!self.disconnected);
            self.entries.get(path).cloned().ok_or_else(Self::missing)
        }

        fn read_chunk(
            &mut self,
            path: &str,
            offset: u64,
            max_bytes: u32,
        ) -> Result<NativeReadChunk, NativeError> {
            let before = self.stat(path)?;
            let bytes = self.files.get(path).ok_or_else(Self::missing)?;
            let start = usize::try_from(offset).map_err(|_| Self::missing())?;
            if start > bytes.len() {
                return Err(NativeError {
                    kind: NativeErrorKind::Conflict,
                    reason: "smb_read_offset_exceeds_size",
                });
            }
            let end = start
                .saturating_add(usize::try_from(max_bytes).unwrap_or(usize::MAX))
                .min(bytes.len());
            Ok(NativeReadChunk {
                before: before.clone(),
                bytes: bytes[start..end].to_vec(),
                after: before,
            })
        }

        fn prepare_write(&mut self, path: &str, resume: bool) -> Result<NativeEntry, NativeError> {
            if resume {
                return self.stat(path);
            }
            let name = path.rsplit('/').next().expect("test path has a name");
            let mut entry = entry(name, path, NativeEntryKind::File);
            entry.size_bytes = Some(0);
            entry.modified_at_unix_ms = Some(1_700_000_000_100);
            self.files.insert(path.to_owned(), Vec::new());
            self.entries.insert(path.to_owned(), entry.clone());
            Ok(entry)
        }

        fn write_chunk(
            &mut self,
            path: &str,
            offset: u64,
            bytes: &[u8],
        ) -> Result<NativeWriteChunk, NativeError> {
            let before = self.stat(path)?;
            let file = self.files.get_mut(path).ok_or_else(Self::missing)?;
            let offset = usize::try_from(offset).map_err(|_| Self::missing())?;
            if offset != file.len() {
                return Err(NativeError {
                    kind: NativeErrorKind::Conflict,
                    reason: "smb_write_offset_mismatch",
                });
            }
            file.extend_from_slice(bytes);
            let mut after = before.clone();
            after.size_bytes = Some(file.len() as u64);
            after.modified_at_unix_ms = before.modified_at_unix_ms.map(|value| value + 1);
            self.entries.insert(path.to_owned(), after.clone());
            Ok(NativeWriteChunk { before, after })
        }

        fn create_directory(&mut self, path: &str) -> Result<NativeEntry, NativeError> {
            let name = path.rsplit('/').next().expect("test path has a name");
            let entry = entry(name, path, NativeEntryKind::Directory);
            self.entries.insert(path.to_owned(), entry.clone());
            Ok(entry)
        }

        fn rename(&mut self, from: &str, to: &str) -> Result<NativeEntry, NativeError> {
            let mut entry = self.entries.remove(from).ok_or_else(Self::missing)?;
            entry.name = to
                .rsplit('/')
                .next()
                .expect("test path has a name")
                .to_owned();
            entry.path = to.to_owned();
            self.entries.insert(to.to_owned(), entry.clone());
            if let Some(bytes) = self.files.remove(from) {
                self.files.insert(to.to_owned(), bytes);
            }
            Ok(entry)
        }

        fn delete(&mut self, path: &str) -> Result<(), NativeError> {
            let result = self
                .entries
                .remove(path)
                .map(|_| ())
                .ok_or_else(Self::missing);
            self.files.remove(path);
            result
        }

        fn disconnect(&mut self) {
            self.disconnected = true;
        }
    }

    fn entry(name: &str, path: &str, kind: NativeEntryKind) -> NativeEntry {
        NativeEntry {
            name: name.to_owned(),
            path: path.to_owned(),
            kind,
            size_bytes: None,
            modified_at_unix_ms: None,
            unix_mode: None,
        }
    }

    fn adapter(observed: Arc<Mutex<Vec<ObservedConfig>>>) -> SmbRemoteFileAdapter {
        SmbRemoteFileAdapter {
            program: PathBuf::from("smbclient"),
            report: CapabilityReport {
                status: CapabilityStatus::Healthy,
                reason: "test".to_owned(),
                client_version: Some("test".to_owned()),
                dialects: ["SMB2", "SMB3"],
                smb1_enabled: false,
                supports_workgroup_domain: true,
                supports_kerberos: true,
                supports_signing: true,
                supports_encryption: true,
                supports_share_browse_diagnostic: true,
                output_contract: crate::OutputContract::OpaqueHumanOutput,
                reauthentication: crate::ReauthenticationMode::FreshProcess,
            },
            capabilities: production_capability_matrix(),
            connector: Some(Arc::new(TestConnector { observed })),
            availability: AdapterAvailability::Healthy,
        }
    }

    fn profile() -> RemoteConnectionProfile {
        RemoteConnectionProfile::new(
            ProfileId::new(),
            "NAS",
            RemoteProtocol::Smb,
            RemoteEndpoint::new("nas.local", 445).expect("endpoint"),
            Some("alice".to_owned()),
            Some("WORKGROUP".to_owned()),
            Authentication::Password {
                secret: SecretRef::secret_service(Uuid::new_v4()),
            },
            TrustPolicy::SmbNegotiated,
            ProfileOptions::Smb {
                share: Some("documents".to_owned()),
                minimum_dialect: SmbDialect::Smb3,
                require_signing: true,
                require_encryption: false,
            },
        )
        .expect("SMB profile")
    }

    #[test]
    fn production_session_maps_structured_operations_and_disconnects() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let adapter = adapter(Arc::clone(&observed));
        let secrets = TestSecretStore {
            resolves: AtomicUsize::new(0),
        };

        let session = block_on(adapter.connect(&profile(), &secrets)).expect("production session");
        assert_eq!(secrets.resolves.load(Ordering::Relaxed), 1);
        assert_eq!(session.snapshot().state, ConnectionState::Ready);
        assert_eq!(
            adapter.capabilities().status(FileOperation::List),
            &CoreCapabilityStatus::Supported
        );
        assert_eq!(
            adapter.capabilities().status(FileOperation::Read),
            &CoreCapabilityStatus::Supported
        );

        let root = RemotePath::new("/").expect("root");
        let listed = block_on(session.list(&root)).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path.as_str(), "/report.txt");
        assert_eq!(listed[0].identity.size_bytes, Some(128));

        let first = block_on(session.read_chunk(RemoteReadRequest {
            path: RemotePath::new("/report.txt").expect("report path"),
            offset: 0,
            max_bytes: 8,
            expected_identity: None,
        }))
        .expect("first read chunk");
        assert_eq!(first.bytes, vec![b'x'; 8]);
        assert!(!first.eof);
        let last = block_on(session.read_chunk(RemoteReadRequest {
            path: RemotePath::new("/report.txt").expect("report path"),
            offset: 120,
            max_bytes: 8,
            expected_identity: Some(first.identity),
        }))
        .expect("resumed read chunk");
        assert_eq!(last.bytes, vec![b'x'; 8]);
        assert!(last.eof);

        let write = block_on(session.begin_write(BeginWriteRequest {
            final_path: RemotePath::new("/uploaded.txt").expect("upload path"),
            temporary_path:
                RemotePath::new("/uploaded.txt.localdesk-test.part").expect("temporary path"),
            expected_size_bytes: Some(6),
            resume_from: None,
            expected_destination: None,
        }))
        .expect("begin staged write");
        let write = block_on(session.write_chunk(write.handle, 0, b"abc".to_vec()))
            .expect("first write chunk");
        let write = block_on(session.write_chunk(write.handle, 3, b"def".to_vec()))
            .expect("second write chunk");
        let committed = block_on(session.commit_write(write.handle, None)).expect("commit write");
        assert_eq!(committed.path.as_str(), "/uploaded.txt");
        assert_eq!(committed.identity.size_bytes, Some(6));
        let uploaded = block_on(session.read_chunk(RemoteReadRequest {
            path: RemotePath::new("/uploaded.txt").expect("upload path"),
            offset: 0,
            max_bytes: 6,
            expected_identity: Some(committed.identity),
        }))
        .expect("read committed upload");
        assert_eq!(uploaded.bytes, b"abcdef");
        assert!(uploaded.eof);

        let created_path = RemotePath::new("/archive").expect("archive path");
        let created = block_on(session.create_directory(&created_path)).expect("mkdir");
        assert_eq!(created.kind, localdesk_remote_core::EntryKind::Directory);
        let renamed_path = RemotePath::new("/history").expect("history path");
        let renamed =
            block_on(session.rename(&created_path, &renamed_path)).expect("rename directory");
        assert_eq!(renamed.path.as_str(), "/history");
        block_on(session.delete(&renamed_path)).expect("delete directory");
        let missing = block_on(session.stat(&renamed_path)).expect_err("deleted path is absent");
        assert_eq!(missing.kind, RemoteErrorKind::NotFound);

        block_on(session.disconnect()).expect("disconnect");
        assert_eq!(session.snapshot().state, ConnectionState::Disconnected);
        let after_disconnect =
            block_on(session.list(&root)).expect_err("closed session rejects I/O");
        assert_eq!(after_disconnect.reason, reason("smb_session_not_ready"));

        assert_eq!(
            *observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            vec![ObservedConfig {
                host: "nas.local".to_owned(),
                port: 445,
                share: "documents".to_owned(),
                username: "alice".to_owned(),
                domain: Some("WORKGROUP".to_owned()),
                dialect: "SMB3".to_owned(),
                require_protection: true,
            }]
        );
    }

    #[test]
    fn production_transfer_enforces_identity_resume_abort_and_deadline() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let adapter = adapter(observed);
        let secrets = TestSecretStore {
            resolves: AtomicUsize::new(0),
        };
        let session = block_on(adapter.connect(&profile(), &secrets)).expect("production session");
        let report_path = RemotePath::new("/report.txt").expect("report path");

        let stale_read = block_on(session.read_chunk(RemoteReadRequest {
            path: report_path.clone(),
            offset: 0,
            max_bytes: 8,
            expected_identity: Some(localdesk_remote_core::ObjectIdentity {
                size_bytes: Some(127),
                modified_at_unix_ms: None,
                etag: None,
            }),
        }))
        .expect_err("stale read identity");
        assert_eq!(stale_read.kind, RemoteErrorKind::Conflict);
        assert_eq!(stale_read.reason, reason("smb_object_identity_changed"));

        let elapsed = block_on(session.read_chunk_controlled(
            RemoteReadRequest {
                path: report_path,
                offset: 0,
                max_bytes: 8,
                expected_identity: None,
            },
            RemoteIoControl::new(Instant::now()),
        ))
        .expect_err("elapsed deadline");
        assert_eq!(elapsed.kind, RemoteErrorKind::Timeout);
        assert_eq!(elapsed.reason, reason("remote_io_deadline_elapsed"));

        let destination_conflict = block_on(session.begin_write(BeginWriteRequest {
            final_path: RemotePath::new("/report.txt").expect("report path"),
            temporary_path:
                RemotePath::new("/report.txt.localdesk-conflict.part").expect("temporary path"),
            expected_size_bytes: Some(3),
            resume_from: None,
            expected_destination: Some(localdesk_remote_core::ObjectIdentity {
                size_bytes: Some(1),
                modified_at_unix_ms: None,
                etag: None,
            }),
        }))
        .expect_err("destination changed");
        assert_eq!(destination_conflict.kind, RemoteErrorKind::Conflict);

        let final_path = RemotePath::new("/resumed.txt").expect("final path");
        let temporary_path =
            RemotePath::new("/resumed.txt.localdesk-resume.part").expect("temporary path");
        let first = block_on(session.begin_write(BeginWriteRequest {
            final_path: final_path.clone(),
            temporary_path: temporary_path.clone(),
            expected_size_bytes: Some(6),
            resume_from: None,
            expected_destination: None,
        }))
        .expect("begin write");
        let offset_error = block_on(session.write_chunk(first.handle, 1, b"abc".to_vec()))
            .expect_err("offset mismatch");
        assert_eq!(offset_error.kind, RemoteErrorKind::Conflict);
        block_on(session.write_chunk(first.handle, 0, b"abc".to_vec())).expect("first chunk");

        let resumed = block_on(session.begin_write(BeginWriteRequest {
            final_path: final_path.clone(),
            temporary_path: temporary_path.clone(),
            expected_size_bytes: Some(6),
            resume_from: Some(3),
            expected_destination: None,
        }))
        .expect("resume staged write");
        let resumed = block_on(session.write_chunk(resumed.handle, 3, b"def".to_vec()))
            .expect("resumed chunk");
        let committed = block_on(session.commit_write(resumed.handle, None)).expect("commit");
        assert_eq!(committed.identity.size_bytes, Some(6));

        let abort_path =
            RemotePath::new("/abort.txt.localdesk-test.part").expect("abort temporary path");
        let abort = block_on(session.begin_write(BeginWriteRequest {
            final_path: RemotePath::new("/abort.txt").expect("abort final path"),
            temporary_path: abort_path.clone(),
            expected_size_bytes: Some(1),
            resume_from: None,
            expected_destination: None,
        }))
        .expect("begin abort write");
        block_on(session.abort_write(abort.handle)).expect("abort write");
        let missing = block_on(session.stat(&abort_path)).expect_err("part removed");
        assert_eq!(missing.kind, RemoteErrorKind::NotFound);
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
