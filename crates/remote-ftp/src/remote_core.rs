use std::collections::HashMap;
use std::fmt;
use std::io::{Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, Authentication, BeginWriteRequest, CapabilityMatrix,
    CapabilityStatus, ConnectionState, DataConnectionMode, EntryKind, FILE_OPERATIONS,
    FileOperation, MAX_REMOTE_CHUNK_BYTES, ObjectIdentity, OperationCapability, ProfileOptions,
    RemoteConnectionProfile, RemoteEntry, RemoteError, RemoteErrorKind, RemoteFileAdapter,
    RemoteFileSession, RemoteIoControl, RemoteIoControlSupport, RemoteOperation,
    RemotePath as CoreRemotePath, RemoteProtocol, RemoteReadChunk, RemoteReadRequest,
    RemoteSession, RemoteWriteHandle, RemoteWriteReceipt, RetryDisposition, SafeReason,
    SecretStore, SecretStoreError, SessionId,
};
use tempfile::NamedTempFile;

use crate::{
    Credentials, DataMode, FtpAdapter, FtpConfig, FtpError, FtpFailureKind, PlainFtpConfirmation,
    RemotePath, SecurityMode,
};

const SET_PERMISSIONS_UNSUPPORTED: &str = "ftp_set_permissions_not_implemented";
const IDENTITY_PRECONDITION_UNSUPPORTED: &str = "ftp_identity_precondition_not_supported";
const ATOMIC_RENAME_UNVERIFIED: &str = "ftp_atomic_rename_not_endpoint_verified";

pub struct RemoteFtpAdapter {
    protocol: RemoteProtocol,
    security: SecurityMode,
    availability: AdapterAvailability,
    capabilities: CapabilityMatrix,
    active_mode: Option<DataMode>,
    connector: Arc<dyn Connector>,
}

impl RemoteFtpAdapter {
    #[must_use]
    pub fn explicit_ftps() -> Self {
        Self {
            protocol: RemoteProtocol::FtpsExplicit,
            security: SecurityMode::ExplicitFtps,
            availability: AdapterAvailability::Healthy,
            capabilities: ftp_capabilities(),
            active_mode: None,
            connector: Arc::new(LibcurlConnector),
        }
    }

    #[must_use]
    pub fn plain_ftp(confirmation: PlainFtpConfirmation) -> Self {
        Self {
            protocol: RemoteProtocol::Ftp,
            security: SecurityMode::PlainFtp(confirmation),
            availability: AdapterAvailability::Degraded(reason("plain_ftp_explicitly_enabled")),
            capabilities: ftp_capabilities(),
            active_mode: None,
            connector: Arc::new(LibcurlConnector),
        }
    }

    /// Configures the single IP and port allowed for explicitly requested active mode.
    ///
    /// # Errors
    ///
    /// Returns a policy error for unspecified or multicast addresses.
    pub fn with_active_binding(
        mut self,
        bind_address: IpAddr,
        listen_port: NonZeroU16,
    ) -> Result<Self, FtpError> {
        let mode = DataMode::Active {
            bind_address,
            listen_port,
        };
        mode.active_binding()?;
        self.active_mode = Some(mode);
        Ok(self)
    }

    #[allow(clippy::too_many_lines)]
    async fn connect_profile(
        &self,
        profile: &RemoteConnectionProfile,
        secrets: &dyn SecretStore,
        control: Option<&RemoteIoControl>,
    ) -> Result<Box<dyn RemoteFileSession>, RemoteError> {
        check_control(control, RemoteOperation::Connect)?;
        profile.validate().map_err(|_| {
            remote_error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                "ftp_profile_invalid",
                RetryDisposition::Never,
            )
        })?;
        if profile.protocol != self.protocol {
            return Err(remote_error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                "ftp_profile_protocol_mismatch",
                RetryDisposition::Never,
            ));
        }
        if profile.domain.is_some() {
            return Err(remote_error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                "ftp_domain_not_supported",
                RetryDisposition::UserAction,
            ));
        }

        let data_mode = match &profile.options {
            ProfileOptions::Ftp { data_connection }
            | ProfileOptions::FtpsExplicit {
                data_connection, ..
            } => match data_connection {
                DataConnectionMode::Passive => DataMode::Passive,
                DataConnectionMode::ActiveRestricted => {
                    self.active_mode.clone().ok_or_else(|| {
                        remote_error(
                            RemoteErrorKind::Unsupported,
                            RemoteOperation::Connect,
                            "ftp_active_binding_required",
                            RetryDisposition::UserAction,
                        )
                    })?
                }
            },
            _ => {
                return Err(remote_error(
                    RemoteErrorKind::InvalidInput,
                    RemoteOperation::Connect,
                    "ftp_profile_options_mismatch",
                    RetryDisposition::Never,
                ));
            }
        };
        if matches!(
            profile.options,
            ProfileOptions::FtpsExplicit {
                require_protected_data_channel: false,
                ..
            }
        ) {
            return Err(remote_error(
                RemoteErrorKind::Unsupported,
                RemoteOperation::Connect,
                "ftps_protected_data_required",
                RetryDisposition::UserAction,
            ));
        }

        let credentials = match &profile.authentication {
            Authentication::Anonymous => {
                Credentials::new(profile.username.as_deref().unwrap_or("anonymous"), "")
                    .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?
            }
            Authentication::Password { secret } => {
                check_control(control, RemoteOperation::Connect)?;
                let username = profile.username.as_deref().ok_or_else(|| {
                    remote_error(
                        RemoteErrorKind::InvalidInput,
                        RemoteOperation::Connect,
                        "ftp_username_required",
                        RetryDisposition::UserAction,
                    )
                })?;
                let value = secrets
                    .resolve(secret)
                    .await
                    .map_err(|error| map_secret_error(&error))?;
                check_control(control, RemoteOperation::Connect)?;
                let password = std::str::from_utf8(value.expose_secret()).map_err(|_| {
                    remote_error(
                        RemoteErrorKind::InvalidInput,
                        RemoteOperation::ResolveSecret,
                        "ftp_password_not_utf8",
                        RetryDisposition::UserAction,
                    )
                })?;
                Credentials::new(username, password)
                    .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?
            }
            _ => {
                return Err(remote_error(
                    RemoteErrorKind::Unsupported,
                    RemoteOperation::Connect,
                    "ftp_authentication_not_supported",
                    RetryDisposition::UserAction,
                ));
            }
        };

        let port = NonZeroU16::new(profile.endpoint.port).ok_or_else(|| {
            remote_error(
                RemoteErrorKind::InvalidInput,
                RemoteOperation::Connect,
                "ftp_port_invalid",
                RetryDisposition::Never,
            )
        })?;
        let mut config = FtpConfig::explicit_ftps(profile.endpoint.host(), port, credentials)
            .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?;
        config.security = self.security.clone();
        config.data_mode = data_mode;
        if let localdesk_remote_core::TrustPolicy::PinnedTlsCertificate { certificate_pem } =
            &profile.trust
        {
            config.ca_certificate_pem = Some(certificate_pem.as_bytes().to_vec());
        }
        config
            .validate()
            .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?;

        check_control(control, RemoteOperation::Connect)?;
        let backend = self
            .connector
            .build(config)
            .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?;
        match control {
            Some(control) => backend.probe_controlled(control),
            None => backend.probe(),
        }
        .map_err(|error| map_ftp_error(&error, RemoteOperation::Connect))?;
        check_control(control, RemoteOperation::Connect)?;
        let now = unix_time_ms()?;
        Ok(Box::new(FtpRemoteSession {
            snapshot: Mutex::new(RemoteSession {
                id: SessionId::new(),
                profile_id: profile.id,
                protocol: profile.protocol,
                state: ConnectionState::Ready,
                capabilities: self.capabilities.clone(),
                opened_at_unix_ms: now,
                updated_at_unix_ms: now,
            }),
            backend,
            writes: Mutex::new(HashMap::new()),
        }))
    }

    #[cfg(test)]
    fn with_connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.connector = connector;
        self
    }
}

impl Default for RemoteFtpAdapter {
    fn default() -> Self {
        Self::explicit_ftps()
    }
}

impl fmt::Debug for RemoteFtpAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteFtpAdapter")
            .field("protocol", &self.protocol)
            .field("security", &self.security)
            .field("availability", &self.availability)
            .field("capabilities", &self.capabilities)
            .field("active_mode", &self.active_mode)
            .finish_non_exhaustive()
    }
}

impl RemoteFileAdapter for RemoteFtpAdapter {
    fn protocol(&self) -> RemoteProtocol {
        self.protocol
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
        Box::pin(async move { self.connect_profile(profile, secrets, None).await })
    }

    fn connect_controlled<'a>(
        &'a self,
        profile: &'a RemoteConnectionProfile,
        secrets: &'a dyn SecretStore,
        control: RemoteIoControl,
    ) -> AdapterFuture<'a, Result<Box<dyn RemoteFileSession>, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Connect)?;
            self.connect_profile(profile, secrets, Some(&control)).await
        })
    }
}

struct FtpRemoteSession {
    snapshot: Mutex<RemoteSession>,
    backend: Arc<dyn Backend>,
    writes: Mutex<HashMap<RemoteWriteHandle, PendingWrite>>,
}

impl fmt::Debug for FtpRemoteSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpRemoteSession")
            .field("snapshot", &self.snapshot().state)
            .field("pending_writes", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl FtpRemoteSession {
    fn ensure_ready(&self, operation: RemoteOperation) -> Result<(), RemoteError> {
        if matches!(self.lock_snapshot()?.state, ConnectionState::Ready) {
            Ok(())
        } else {
            Err(remote_error(
                RemoteErrorKind::InvalidInput,
                operation,
                "ftp_session_not_ready",
                RetryDisposition::Never,
            ))
        }
    }

    fn lock_snapshot(&self) -> Result<MutexGuard<'_, RemoteSession>, RemoteError> {
        self.snapshot.lock().map_err(|_| internal_state_error())
    }

    fn lock_writes(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<RemoteWriteHandle, PendingWrite>>, RemoteError> {
        self.writes.lock().map_err(|_| internal_state_error())
    }

    fn ensure_capability(
        &self,
        capability: FileOperation,
        operation: RemoteOperation,
    ) -> Result<(), RemoteError> {
        match self.snapshot().capabilities.status(capability) {
            CapabilityStatus::Supported => Ok(()),
            CapabilityStatus::Unsupported(reason) => Err(unsupported(operation, reason.as_str())),
        }
    }

    fn entry(&self, path: CoreRemotePath, kind: EntryKind, size_bytes: Option<u64>) -> RemoteEntry {
        RemoteEntry {
            name: entry_name(&path),
            path,
            kind,
            identity: ObjectIdentity {
                size_bytes,
                modified_at_unix_ms: None,
                etag: None,
            },
            unix_mode: None,
            capabilities: self.snapshot().capabilities,
        }
    }

    fn list_entries(
        &self,
        directory: &CoreRemotePath,
        bytes: &[u8],
    ) -> Result<Vec<RemoteEntry>, RemoteError> {
        let listing = std::str::from_utf8(bytes).map_err(|_| {
            remote_error(
                RemoteErrorKind::RemoteProtocol,
                RemoteOperation::List,
                "ftp_listing_not_utf8",
                RetryDisposition::Never,
            )
        })?;
        listing
            .lines()
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| !line.is_empty())
            .filter_map(parse_mlsd_entry)
            .map(|entry| {
                let path = if entry.name.starts_with('/') {
                    entry.name.to_owned()
                } else if directory.as_str().ends_with('/') {
                    format!("{}{}", directory.as_str(), entry.name)
                } else {
                    format!("{}/{}", directory.as_str(), entry.name)
                };
                let path = CoreRemotePath::new(path).map_err(|_| {
                    remote_error(
                        RemoteErrorKind::RemoteProtocol,
                        RemoteOperation::List,
                        "ftp_listing_path_invalid",
                        RetryDisposition::Never,
                    )
                })?;
                Ok(self.entry(path, entry.kind, entry.size_bytes))
            })
            .collect()
    }
}

struct MlsdEntry<'a> {
    name: &'a str,
    kind: EntryKind,
    size_bytes: Option<u64>,
}

fn parse_mlsd_entry(line: &str) -> Option<MlsdEntry<'_>> {
    let (facts, name) = line.split_once(' ')?;
    let name = name.trim_start();
    if name.is_empty() {
        return None;
    }
    let mut kind = EntryKind::Other;
    let mut size_bytes = None;
    for fact in facts.split(';').filter(|fact| !fact.is_empty()) {
        let (key, value) = fact.split_once('=')?;
        if key.eq_ignore_ascii_case("type") {
            if value.eq_ignore_ascii_case("cdir") || value.eq_ignore_ascii_case("pdir") {
                return None;
            }
            kind = if value.eq_ignore_ascii_case("file") {
                EntryKind::File
            } else if value.eq_ignore_ascii_case("dir") {
                EntryKind::Directory
            } else if value.to_ascii_lowercase().starts_with("os.unix=slink") {
                EntryKind::Symlink
            } else {
                EntryKind::Other
            };
        } else if key.eq_ignore_ascii_case("size") {
            size_bytes = value.parse().ok();
        }
    }
    if kind == EntryKind::Directory {
        size_bytes = None;
    }
    Some(MlsdEntry {
        name,
        kind,
        size_bytes,
    })
}

impl RemoteFileSession for FtpRemoteSession {
    fn id(&self) -> SessionId {
        self.snapshot().id
    }

    fn snapshot(&self) -> RemoteSession {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn io_control_support(&self) -> RemoteIoControlSupport {
        RemoteIoControlSupport::Supported
    }

    fn list<'a>(
        &'a self,
        path: &'a CoreRemotePath,
    ) -> AdapterFuture<'a, Result<Vec<RemoteEntry>, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::List)?;
            let ftp_path = ftp_path(path, RemoteOperation::List)?;
            let bytes = self
                .backend
                .list(&ftp_path)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::List))?;
            self.list_entries(path, &bytes)
        })
    }

    fn stat<'a>(
        &'a self,
        path: &'a CoreRemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Stat)?;
            self.ensure_capability(FileOperation::Stat, RemoteOperation::Stat)?;
            let ftp_path = ftp_path(path, RemoteOperation::Stat)?;
            let size = self
                .backend
                .stat_size(&ftp_path)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::Stat))?
                .ok_or_else(|| unsupported(RemoteOperation::Stat, "ftp_size_unavailable"))?;
            Ok(self.entry(path.clone(), EntryKind::File, Some(size)))
        })
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a CoreRemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::CreateDirectory)?;
            let ftp_path = ftp_path(path, RemoteOperation::CreateDirectory)?;
            self.backend
                .create_directory(&ftp_path)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::CreateDirectory))?;
            Ok(self.entry(path.clone(), EntryKind::Directory, None))
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a CoreRemotePath,
        to: &'a CoreRemotePath,
    ) -> AdapterFuture<'a, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Rename)?;
            let from_ftp = ftp_path(from, RemoteOperation::Rename)?;
            let to_ftp = ftp_path(to, RemoteOperation::Rename)?;
            self.backend
                .rename(&from_ftp, &to_ftp)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::Rename))?;
            Ok(self.entry(to.clone(), EntryKind::Other, None))
        })
    }

    fn delete<'a>(
        &'a self,
        path: &'a CoreRemotePath,
    ) -> AdapterFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Delete)?;
            let ftp_path = ftp_path(path, RemoteOperation::Delete)?;
            self.backend
                .delete_path(&ftp_path)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::Delete))
        })
    }

    fn read_chunk(
        &self,
        request: RemoteReadRequest,
    ) -> AdapterFuture<'_, Result<RemoteReadChunk, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Read)?;
            self.ensure_capability(FileOperation::Read, RemoteOperation::Read)?;
            if request.offset > 0 {
                self.ensure_capability(FileOperation::ResumeRead, RemoteOperation::Resume)?;
            }
            if !request.is_bounded() {
                return Err(invalid_input(
                    RemoteOperation::Read,
                    "ftp_read_chunk_unbounded",
                ));
            }
            if request.expected_identity.as_ref().is_some_and(|identity| {
                identity.modified_at_unix_ms.is_some() || identity.etag.is_some()
            }) {
                return Err(unsupported(
                    RemoteOperation::Read,
                    IDENTITY_PRECONDITION_UNSUPPORTED,
                ));
            }
            let ftp_path = ftp_path(&request.path, RemoteOperation::Read)?;
            let (bytes, size) = self
                .backend
                .read_chunk(&ftp_path, request.offset, request.max_bytes)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::Read))?;
            if request
                .expected_identity
                .as_ref()
                .and_then(|identity| identity.size_bytes)
                .is_some_and(|expected| expected != size)
            {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Read,
                    "ftp_identity_size_changed",
                    RetryDisposition::UserAction,
                ));
            }
            let eof = request.offset.saturating_add(bytes.len() as u64) >= size;
            Ok(RemoteReadChunk {
                offset: request.offset,
                bytes,
                eof,
                identity: ObjectIdentity {
                    size_bytes: Some(size),
                    modified_at_unix_ms: None,
                    etag: None,
                },
            })
        })
    }

    fn begin_write(
        &self,
        request: BeginWriteRequest,
    ) -> AdapterFuture<'_, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Write)?;
            if request.expected_destination.is_some() {
                return Err(unsupported(
                    RemoteOperation::Write,
                    IDENTITY_PRECONDITION_UNSUPPORTED,
                ));
            }
            self.ensure_capability(FileOperation::Write, RemoteOperation::Write)?;
            if request.resume_from.is_some() {
                self.ensure_capability(FileOperation::ResumeWrite, RemoteOperation::Resume)?;
            }
            if request.final_path == request.temporary_path
                || request
                    .temporary_path
                    .as_str()
                    .rsplit_once('.')
                    .is_none_or(|(_, extension)| extension != "part")
            {
                return Err(invalid_input(
                    RemoteOperation::Write,
                    "ftp_temporary_path_must_be_part",
                ));
            }
            let resume_from = request.resume_from.unwrap_or(0);
            if request
                .expected_size_bytes
                .is_some_and(|expected| resume_from > expected)
            {
                return Err(invalid_input(
                    RemoteOperation::Resume,
                    "ftp_resume_offset_exceeds_expected_size",
                ));
            }
            ftp_path(&request.final_path, RemoteOperation::Write)?;
            let temporary_ftp = ftp_path(&request.temporary_path, RemoteOperation::Resume)?;
            if let Some(expected) = request.resume_from {
                let actual = self
                    .backend
                    .stat_size(&temporary_ftp)
                    .map_err(|error| map_ftp_error(&error, RemoteOperation::Resume))?
                    .unwrap_or(0);
                if actual != expected {
                    return Err(remote_error(
                        RemoteErrorKind::Conflict,
                        RemoteOperation::Resume,
                        "ftp_resume_partial_size_mismatch",
                        RetryDisposition::UserAction,
                    ));
                }
            }

            let file = NamedTempFile::new().map_err(|_| {
                remote_error(
                    RemoteErrorKind::Transport,
                    RemoteOperation::Write,
                    "ftp_local_staging_create_failed",
                    RetryDisposition::UserAction,
                )
            })?;
            file.as_file().set_len(resume_from).map_err(|_| {
                remote_error(
                    RemoteErrorKind::Transport,
                    RemoteOperation::Write,
                    "ftp_local_staging_resize_failed",
                    RetryDisposition::UserAction,
                )
            })?;
            let handle = RemoteWriteHandle::new();
            self.lock_writes()?.insert(
                handle,
                PendingWrite {
                    file,
                    final_path: request.final_path,
                    temporary_path: request.temporary_path,
                    expected_size_bytes: request.expected_size_bytes,
                    resume_from: request.resume_from,
                    next_offset: resume_from,
                },
            );
            Ok(RemoteWriteReceipt {
                handle,
                next_offset: resume_from,
                identity: None,
            })
        })
    }

    fn write_chunk(
        &self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
    ) -> AdapterFuture<'_, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Write)?;
            if bytes.is_empty() || bytes.len() > MAX_REMOTE_CHUNK_BYTES as usize {
                return Err(invalid_input(
                    RemoteOperation::Write,
                    "ftp_write_chunk_invalid_size",
                ));
            }
            let mut writes = self.lock_writes()?;
            let pending = writes
                .get_mut(&handle)
                .ok_or_else(|| invalid_input(RemoteOperation::Write, "ftp_write_handle_unknown"))?;
            if offset != pending.next_offset {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "ftp_write_offset_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            let next_offset = offset.checked_add(bytes.len() as u64).ok_or_else(|| {
                invalid_input(RemoteOperation::Write, "ftp_write_offset_overflow")
            })?;
            if pending
                .expected_size_bytes
                .is_some_and(|expected| next_offset > expected)
            {
                return Err(invalid_input(
                    RemoteOperation::Write,
                    "ftp_write_exceeds_expected_size",
                ));
            }
            pending
                .file
                .as_file_mut()
                .seek(SeekFrom::Start(offset))
                .and_then(|_| pending.file.as_file_mut().write_all(&bytes))
                .map_err(|_| {
                    remote_error(
                        RemoteErrorKind::Transport,
                        RemoteOperation::Write,
                        "ftp_local_staging_write_failed",
                        RetryDisposition::UserAction,
                    )
                })?;
            pending.next_offset = next_offset;
            Ok(RemoteWriteReceipt {
                handle,
                next_offset,
                identity: None,
            })
        })
    }

    fn commit_write(
        &self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
    ) -> AdapterFuture<'_, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Write)?;
            if expected_identity.is_some() {
                return Err(unsupported(
                    RemoteOperation::Write,
                    IDENTITY_PRECONDITION_UNSUPPORTED,
                ));
            }
            let mut pending = self
                .lock_writes()?
                .remove(&handle)
                .ok_or_else(|| invalid_input(RemoteOperation::Write, "ftp_write_handle_unknown"))?;
            if pending
                .expected_size_bytes
                .is_some_and(|expected| expected != pending.next_offset)
            {
                self.lock_writes()?.insert(handle, pending);
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "ftp_write_size_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            if pending.file.as_file_mut().sync_all().is_err() {
                self.lock_writes()?.insert(handle, pending);
                return Err(remote_error(
                    RemoteErrorKind::Transport,
                    RemoteOperation::Write,
                    "ftp_local_staging_sync_failed",
                    RetryDisposition::UserAction,
                ));
            }
            let final_ftp = ftp_path(&pending.final_path, RemoteOperation::Write)?;
            let temporary_ftp = ftp_path(&pending.temporary_path, RemoteOperation::Write)?;
            if let Err(error) = self.backend.upload_with_temporary(
                pending.file.path(),
                &temporary_ftp,
                &final_ftp,
                pending.resume_from,
            ) {
                self.lock_writes()?.insert(handle, pending);
                return Err(map_ftp_error(&error, RemoteOperation::Write));
            }
            let committed_size = self.backend.stat_size(&final_ftp).ok().flatten();
            Ok(self.entry(pending.final_path, EntryKind::File, committed_size))
        })
    }

    fn abort_write(&self, handle: RemoteWriteHandle) -> AdapterFuture<'_, Result<(), RemoteError>> {
        Box::pin(async move {
            self.ensure_ready(RemoteOperation::Write)?;
            self.lock_writes()?
                .remove(&handle)
                .ok_or_else(|| invalid_input(RemoteOperation::Write, "ftp_write_handle_unknown"))?;
            Ok(())
        })
    }

    fn disconnect(&self) -> AdapterFuture<'_, Result<(), RemoteError>> {
        Box::pin(async move {
            let now = unix_time_ms()?;
            let mut snapshot = self.lock_snapshot()?;
            snapshot
                .transition(ConnectionState::Closing, now)
                .map_err(|_| internal_state_error())?;
            snapshot
                .transition(ConnectionState::Disconnected, now)
                .map_err(|_| internal_state_error())?;
            drop(snapshot);
            self.lock_writes()?.clear();
            Ok(())
        })
    }

    fn read_chunk_controlled(
        &self,
        request: RemoteReadRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<RemoteReadChunk, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Read)?;
            self.ensure_ready(RemoteOperation::Read)?;
            self.ensure_capability(FileOperation::Read, RemoteOperation::Read)?;
            if request.offset > 0 {
                self.ensure_capability(FileOperation::ResumeRead, RemoteOperation::Resume)?;
            }
            if !request.is_bounded() {
                return Err(invalid_input(
                    RemoteOperation::Read,
                    "ftp_read_chunk_unbounded",
                ));
            }
            if request.expected_identity.as_ref().is_some_and(|identity| {
                identity.modified_at_unix_ms.is_some() || identity.etag.is_some()
            }) {
                return Err(unsupported(
                    RemoteOperation::Read,
                    IDENTITY_PRECONDITION_UNSUPPORTED,
                ));
            }
            let ftp_path = ftp_path(&request.path, RemoteOperation::Read)?;
            let (bytes, size) = self
                .backend
                .read_chunk_controlled(&ftp_path, request.offset, request.max_bytes, &control)
                .map_err(|error| map_ftp_error(&error, RemoteOperation::Read))?;
            if request
                .expected_identity
                .as_ref()
                .and_then(|identity| identity.size_bytes)
                .is_some_and(|expected| expected != size)
            {
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Read,
                    "ftp_identity_size_changed",
                    RetryDisposition::UserAction,
                ));
            }
            let eof = request.offset.saturating_add(bytes.len() as u64) >= size;
            Ok(RemoteReadChunk {
                offset: request.offset,
                bytes,
                eof,
                identity: ObjectIdentity {
                    size_bytes: Some(size),
                    modified_at_unix_ms: None,
                    etag: None,
                },
            })
        })
    }

    fn begin_write_controlled(
        &self,
        request: BeginWriteRequest,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            self.begin_write(request).await
        })
    }

    fn write_chunk_controlled(
        &self,
        handle: RemoteWriteHandle,
        offset: u64,
        bytes: Vec<u8>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<RemoteWriteReceipt, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            self.write_chunk(handle, offset, bytes).await
        })
    }

    fn commit_write_controlled(
        &self,
        handle: RemoteWriteHandle,
        expected_identity: Option<ObjectIdentity>,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<RemoteEntry, RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            self.ensure_ready(RemoteOperation::Write)?;
            if expected_identity.is_some() {
                return Err(unsupported(
                    RemoteOperation::Write,
                    IDENTITY_PRECONDITION_UNSUPPORTED,
                ));
            }
            let mut pending = self
                .lock_writes()?
                .remove(&handle)
                .ok_or_else(|| invalid_input(RemoteOperation::Write, "ftp_write_handle_unknown"))?;
            if pending
                .expected_size_bytes
                .is_some_and(|expected| expected != pending.next_offset)
            {
                self.lock_writes()?.insert(handle, pending);
                return Err(remote_error(
                    RemoteErrorKind::Conflict,
                    RemoteOperation::Write,
                    "ftp_write_size_mismatch",
                    RetryDisposition::UserAction,
                ));
            }
            if pending.file.as_file_mut().sync_all().is_err() {
                self.lock_writes()?.insert(handle, pending);
                return Err(remote_error(
                    RemoteErrorKind::Transport,
                    RemoteOperation::Write,
                    "ftp_local_staging_sync_failed",
                    RetryDisposition::UserAction,
                ));
            }
            if let Err(error) = control.check(RemoteOperation::Write) {
                self.lock_writes()?.insert(handle, pending);
                return Err(error);
            }
            let final_ftp = ftp_path(&pending.final_path, RemoteOperation::Write)?;
            let temporary_ftp = ftp_path(&pending.temporary_path, RemoteOperation::Write)?;
            if let Err(error) = self.backend.upload_with_temporary_controlled(
                pending.file.path(),
                &temporary_ftp,
                &final_ftp,
                pending.resume_from,
                &control,
            ) {
                self.lock_writes()?.insert(handle, pending);
                return Err(map_ftp_error(&error, RemoteOperation::Write));
            }
            Ok(self.entry(pending.final_path, EntryKind::File, None))
        })
    }

    fn abort_write_controlled(
        &self,
        handle: RemoteWriteHandle,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Write)?;
            self.abort_write(handle).await
        })
    }

    fn disconnect_controlled(
        &self,
        control: RemoteIoControl,
    ) -> AdapterFuture<'_, Result<(), RemoteError>> {
        Box::pin(async move {
            control.check(RemoteOperation::Disconnect)?;
            self.disconnect().await
        })
    }
}

struct PendingWrite {
    file: NamedTempFile,
    final_path: CoreRemotePath,
    temporary_path: CoreRemotePath,
    expected_size_bytes: Option<u64>,
    resume_from: Option<u64>,
    next_offset: u64,
}

trait Connector: Send + Sync {
    fn build(&self, config: FtpConfig) -> Result<Arc<dyn Backend>, FtpError>;
}

struct LibcurlConnector;

impl Connector for LibcurlConnector {
    fn build(&self, config: FtpConfig) -> Result<Arc<dyn Backend>, FtpError> {
        Ok(Arc::new(FtpAdapter::new(config)?))
    }
}

trait Backend: Send + Sync {
    fn probe(&self) -> Result<(), FtpError>;
    fn probe_controlled(&self, control: &RemoteIoControl) -> Result<(), FtpError> {
        check_ftp_control(control)?;
        let result = self.probe();
        check_ftp_control(control)?;
        result
    }
    fn list(&self, directory: &RemotePath) -> Result<Vec<u8>, FtpError>;
    fn stat_size(&self, path: &RemotePath) -> Result<Option<u64>, FtpError>;
    fn create_directory(&self, path: &RemotePath) -> Result<(), FtpError>;
    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), FtpError>;
    fn delete_path(&self, path: &RemotePath) -> Result<(), FtpError>;
    fn read_chunk(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
    ) -> Result<(Vec<u8>, u64), FtpError>;
    fn read_chunk_controlled(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
        control: &RemoteIoControl,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        check_ftp_control(control)?;
        let result = self.read_chunk(remote, offset, max_bytes);
        check_ftp_control(control)?;
        result
    }
    fn upload_with_temporary(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
    ) -> Result<(), FtpError>;
    fn upload_with_temporary_controlled(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
        control: &RemoteIoControl,
    ) -> Result<(), FtpError> {
        check_ftp_control(control)?;
        let result = self.upload_with_temporary(source, temporary, final_path, resume_from);
        check_ftp_control(control)?;
        result
    }
}

impl Backend for FtpAdapter {
    fn probe(&self) -> Result<(), FtpError> {
        self.probe()
    }

    fn probe_controlled(&self, control: &RemoteIoControl) -> Result<(), FtpError> {
        self.probe_controlled(control)
    }

    fn list(&self, directory: &RemotePath) -> Result<Vec<u8>, FtpError> {
        self.list(directory)
    }

    fn stat_size(&self, path: &RemotePath) -> Result<Option<u64>, FtpError> {
        self.stat_size(path)
    }

    fn create_directory(&self, path: &RemotePath) -> Result<(), FtpError> {
        self.create_directory(path)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), FtpError> {
        self.rename(from, to)
    }

    fn delete_path(&self, path: &RemotePath) -> Result<(), FtpError> {
        self.delete_path(path)
    }

    fn read_chunk(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        self.read_chunk(remote, offset, max_bytes)
    }

    fn read_chunk_controlled(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
        control: &RemoteIoControl,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        self.read_chunk_controlled(remote, offset, max_bytes, control)
    }

    fn upload_with_temporary(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
    ) -> Result<(), FtpError> {
        self.upload_with_temporary(source, temporary, final_path, resume_from)
    }

    fn upload_with_temporary_controlled(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
        control: &RemoteIoControl,
    ) -> Result<(), FtpError> {
        self.upload_with_temporary_controlled(source, temporary, final_path, resume_from, control)
    }
}

fn ftp_capabilities() -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: match operation {
                FileOperation::List
                | FileOperation::CreateDirectory
                | FileOperation::Rename
                | FileOperation::Delete
                | FileOperation::Stat
                | FileOperation::Read
                | FileOperation::Write
                | FileOperation::ResumeRead
                | FileOperation::ResumeWrite => CapabilityStatus::Supported,
                FileOperation::AtomicRename => {
                    CapabilityStatus::Unsupported(reason(ATOMIC_RENAME_UNVERIFIED))
                }
                FileOperation::SetPermissions => {
                    CapabilityStatus::Unsupported(reason(SET_PERMISSIONS_UNSUPPORTED))
                }
            },
        }
    }))
    .expect("static FTP capability matrix is complete")
}

fn ftp_path(path: &CoreRemotePath, operation: RemoteOperation) -> Result<RemotePath, RemoteError> {
    RemotePath::new(path.as_str())
        .map_err(|_| invalid_input(operation, "ftp_remote_path_must_be_absolute"))
}

fn entry_name(path: &CoreRemotePath) -> String {
    path.as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("/")
        .to_owned()
}

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).expect("static safe reason is valid")
}

fn remote_error(
    kind: RemoteErrorKind,
    operation: RemoteOperation,
    reason_code: &str,
    retry: RetryDisposition,
) -> RemoteError {
    RemoteError::new(kind, operation, reason(reason_code), retry)
}

fn invalid_input(operation: RemoteOperation, reason_code: &str) -> RemoteError {
    remote_error(
        RemoteErrorKind::InvalidInput,
        operation,
        reason_code,
        RetryDisposition::Never,
    )
}

fn unsupported(operation: RemoteOperation, reason_code: &str) -> RemoteError {
    remote_error(
        RemoteErrorKind::Unsupported,
        operation,
        reason_code,
        RetryDisposition::UserAction,
    )
}

fn internal_state_error() -> RemoteError {
    remote_error(
        RemoteErrorKind::Transport,
        RemoteOperation::Disconnect,
        "ftp_internal_state_unavailable",
        RetryDisposition::Never,
    )
}

fn unix_time_ms() -> Result<i64, RemoteError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| internal_state_error())?
        .as_millis();
    i64::try_from(millis).map_err(|_| internal_state_error())
}

fn check_control(
    control: Option<&RemoteIoControl>,
    operation: RemoteOperation,
) -> Result<(), RemoteError> {
    control.map_or(Ok(()), |control| control.check(operation))
}

fn check_ftp_control(control: &RemoteIoControl) -> Result<(), FtpError> {
    if control.is_cancelled() {
        return Err(FtpError::Cancelled);
    }
    if std::time::Instant::now() >= control.deadline() {
        return Err(FtpError::DeadlineExceeded);
    }
    Ok(())
}

fn map_secret_error(error: &SecretStoreError) -> RemoteError {
    let (reason_code, retry) = match error {
        SecretStoreError::Locked(_) => ("secret_store_locked", RetryDisposition::UserAction),
        SecretStoreError::PermissionDenied(_) => (
            "secret_store_permission_denied",
            RetryDisposition::UserAction,
        ),
        SecretStoreError::Unavailable(_) => ("secret_store_unavailable", RetryDisposition::Backoff),
        SecretStoreError::NotFound(_) => {
            ("secret_store_item_not_found", RetryDisposition::UserAction)
        }
        SecretStoreError::Backend(_) => ("secret_store_backend_failed", RetryDisposition::Backoff),
    };
    remote_error(
        RemoteErrorKind::SecretStore,
        RemoteOperation::ResolveSecret,
        reason_code,
        retry,
    )
}

fn map_ftp_error(error: &FtpError, operation: RemoteOperation) -> RemoteError {
    match error {
        FtpError::Cancelled => remote_error(
            RemoteErrorKind::Cancelled,
            operation,
            "remote_io_cancelled",
            RetryDisposition::Never,
        ),
        FtpError::DeadlineExceeded => remote_error(
            RemoteErrorKind::Timeout,
            operation,
            "remote_io_deadline_elapsed",
            RetryDisposition::Backoff,
        ),
        FtpError::Configuration(_) => invalid_input(operation, "ftp_invalid_configuration"),
        FtpError::Policy(_) => unsupported(operation, "ftp_policy_rejected"),
        FtpError::Protocol(_) => remote_error(
            RemoteErrorKind::RemoteProtocol,
            operation,
            "ftp_protocol_verification_failed",
            RetryDisposition::Never,
        ),
        FtpError::Remote { failure, code, .. } => {
            let failure = if *code == Some(530) {
                FtpFailureKind::Authentication
            } else if *code == Some(550) {
                FtpFailureKind::NotFound
            } else {
                *failure
            };
            match failure {
                FtpFailureKind::Trust => remote_error(
                    RemoteErrorKind::Trust,
                    operation,
                    "ftps_trust_verification_failed",
                    RetryDisposition::UserAction,
                ),
                FtpFailureKind::Authentication => remote_error(
                    RemoteErrorKind::Authentication,
                    operation,
                    "ftp_authentication_failed",
                    RetryDisposition::Reauthenticate,
                ),
                FtpFailureKind::PermissionDenied => remote_error(
                    RemoteErrorKind::PermissionDenied,
                    operation,
                    "ftp_permission_denied",
                    RetryDisposition::UserAction,
                ),
                FtpFailureKind::NotFound => remote_error(
                    RemoteErrorKind::NotFound,
                    operation,
                    "ftp_path_not_found",
                    RetryDisposition::UserAction,
                ),
                FtpFailureKind::Timeout => remote_error(
                    RemoteErrorKind::Timeout,
                    operation,
                    "ftp_operation_timed_out",
                    RetryDisposition::Backoff,
                ),
                FtpFailureKind::Protocol => remote_error(
                    RemoteErrorKind::RemoteProtocol,
                    operation,
                    "ftp_command_failed",
                    RetryDisposition::Never,
                ),
                FtpFailureKind::Transport => remote_error(
                    RemoteErrorKind::Transport,
                    operation,
                    "ftp_transport_failed",
                    RetryDisposition::Backoff,
                ),
            }
        }
        FtpError::Io(_) => remote_error(
            RemoteErrorKind::Transport,
            operation,
            "ftp_local_io_failed",
            RetryDisposition::UserAction,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use localdesk_remote_core::{
        Authentication, ProfileId, RemoteEndpoint, SecretRef, SecretStoreError, SecretValue,
        TrustPolicy,
    };
    use uuid::Uuid;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeBackend {
        probe_count: AtomicUsize,
        read_count: AtomicUsize,
        listing: Mutex<Vec<u8>>,
        file: Mutex<Vec<u8>>,
        uploads: Mutex<Vec<UploadRecord>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct UploadRecord {
        temporary: String,
        final_path: String,
        resume_from: Option<u64>,
        bytes: Vec<u8>,
    }

    impl Backend for FakeBackend {
        fn probe(&self) -> Result<(), FtpError> {
            self.probe_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn list(&self, _: &RemotePath) -> Result<Vec<u8>, FtpError> {
            Ok(self.listing.lock().unwrap().clone())
        }

        fn stat_size(&self, _: &RemotePath) -> Result<Option<u64>, FtpError> {
            Ok(Some(self.file.lock().unwrap().len() as u64))
        }

        fn create_directory(&self, _: &RemotePath) -> Result<(), FtpError> {
            Ok(())
        }

        fn rename(&self, _: &RemotePath, _: &RemotePath) -> Result<(), FtpError> {
            Ok(())
        }

        fn delete_path(&self, _: &RemotePath) -> Result<(), FtpError> {
            Ok(())
        }

        fn read_chunk(
            &self,
            _: &RemotePath,
            offset: u64,
            max_bytes: u32,
        ) -> Result<(Vec<u8>, u64), FtpError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            let file = self.file.lock().unwrap();
            let start = usize::try_from(offset)
                .map_err(|_| FtpError::Protocol("fake offset overflow".into()))?;
            let end = start.saturating_add(max_bytes as usize).min(file.len());
            Ok((file[start..end].to_vec(), file.len() as u64))
        }

        fn upload_with_temporary(
            &self,
            source: &Path,
            temporary: &RemotePath,
            final_path: &RemotePath,
            resume_from: Option<u64>,
        ) -> Result<(), FtpError> {
            let all_bytes = std::fs::read(source)?;
            let start = usize::try_from(resume_from.unwrap_or(0))
                .map_err(|_| FtpError::Protocol("fake resume overflow".into()))?;
            *self.file.lock().unwrap() = all_bytes.clone();
            self.uploads.lock().unwrap().push(UploadRecord {
                temporary: temporary.as_str().into(),
                final_path: final_path.as_str().into(),
                resume_from,
                bytes: all_bytes[start..].to_vec(),
            });
            Ok(())
        }
    }

    struct FakeConnector {
        backend: Arc<FakeBackend>,
        config_debug: Mutex<Vec<String>>,
        ca_certificates: Mutex<Vec<Option<Vec<u8>>>>,
    }

    impl Connector for FakeConnector {
        fn build(&self, config: FtpConfig) -> Result<Arc<dyn Backend>, FtpError> {
            self.ca_certificates
                .lock()
                .unwrap()
                .push(config.ca_certificate_pem.clone());
            self.config_debug
                .lock()
                .unwrap()
                .push(format!("{config:?}"));
            Ok(self.backend.clone())
        }
    }

    struct FakeSecretStore {
        value: Vec<u8>,
    }

    impl SecretStore for FakeSecretStore {
        fn resolve<'a>(
            &'a self,
            _: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
            Box::pin(async move { Ok(SecretValue::new(self.value.clone())) })
        }

        fn delete<'a>(
            &'a self,
            _: &'a SecretRef,
        ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn ftps_profile(authentication: Authentication) -> RemoteConnectionProfile {
        RemoteConnectionProfile::new(
            ProfileId::new(),
            "fixture FTPS",
            RemoteProtocol::FtpsExplicit,
            RemoteEndpoint::new("files.example.test", 21).unwrap(),
            if matches!(&authentication, Authentication::Anonymous) {
                None
            } else {
                Some("operator".into())
            },
            None,
            authentication,
            TrustPolicy::SystemTls,
            ProfileOptions::FtpsExplicit {
                data_connection: DataConnectionMode::Passive,
                require_protected_data_channel: true,
            },
        )
        .unwrap()
    }

    fn ftp_profile() -> RemoteConnectionProfile {
        RemoteConnectionProfile::new(
            ProfileId::new(),
            "fixture FTP",
            RemoteProtocol::Ftp,
            RemoteEndpoint::new("files.example.test", 21).unwrap(),
            None,
            None,
            Authentication::Anonymous,
            TrustPolicy::PlaintextAcknowledged,
            ProfileOptions::Ftp {
                data_connection: DataConnectionMode::Passive,
            },
        )
        .unwrap()
    }

    fn fixture_adapter(backend: Arc<FakeBackend>) -> (RemoteFtpAdapter, Arc<FakeConnector>) {
        let connector = Arc::new(FakeConnector {
            backend,
            config_debug: Mutex::new(Vec::new()),
            ca_certificates: Mutex::new(Vec::new()),
        });
        (
            RemoteFtpAdapter::explicit_ftps().with_connector(connector.clone()),
            connector,
        )
    }

    fn ready<T>(future: AdapterFuture<'_, T>) -> T {
        poll_ready(future)
    }

    fn poll_ready<T>(mut future: Pin<Box<dyn Future<Output = T> + Send + '_>>) -> T {
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("fixture future unexpectedly pending"),
        }
    }

    #[test]
    fn controlled_connect_rejects_cancellation_before_connector_io() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend.clone());
        let control =
            RemoteIoControl::new(std::time::Instant::now() + std::time::Duration::from_secs(5));
        control.cancel();

        let error = ready(adapter.connect_controlled(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
            control,
        ))
        .err()
        .expect("pre-cancelled connect must fail");

        assert_eq!(error.kind, RemoteErrorKind::Cancelled);
        assert_eq!(error.reason.as_str(), "remote_io_cancelled");
        assert_eq!(error.retry, RetryDisposition::Never);
        assert_eq!(backend.probe_count.load(Ordering::SeqCst), 0);
        assert!(connector.config_debug.lock().unwrap().is_empty());
    }

    #[test]
    fn controlled_connect_maps_elapsed_deadline_before_connector_io() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend.clone());
        let control = RemoteIoControl::new(std::time::Instant::now());

        let error = ready(adapter.connect_controlled(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
            control,
        ))
        .err()
        .expect("elapsed connect deadline must fail");

        assert_eq!(error.kind, RemoteErrorKind::Timeout);
        assert_eq!(error.reason.as_str(), "remote_io_deadline_elapsed");
        assert_eq!(error.retry, RetryDisposition::Backoff);
        assert_eq!(backend.probe_count.load(Ordering::SeqCst), 0);
        assert!(connector.config_debug.lock().unwrap().is_empty());
    }

    #[test]
    fn pinned_ftps_profile_supplies_one_in_memory_ca_certificate() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend);
        let certificate =
            "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n".to_owned();
        let mut profile = ftps_profile(Authentication::Anonymous);
        profile.trust = TrustPolicy::PinnedTlsCertificate {
            certificate_pem: certificate.clone(),
        };

        ready(adapter.connect(&profile, &FakeSecretStore { value: Vec::new() }))
            .expect("pinned profile connects through fake backend");

        assert_eq!(
            connector.ca_certificates.lock().unwrap().as_slice(),
            &[Some(certificate.into_bytes())]
        );
    }

    #[test]
    fn ftp_failure_mapping_preserves_typed_kind_reason_and_retry_contract() {
        let cases = [
            (
                FtpFailureKind::Trust,
                RemoteErrorKind::Trust,
                "ftps_trust_verification_failed",
                RetryDisposition::UserAction,
            ),
            (
                FtpFailureKind::Authentication,
                RemoteErrorKind::Authentication,
                "ftp_authentication_failed",
                RetryDisposition::Reauthenticate,
            ),
            (
                FtpFailureKind::PermissionDenied,
                RemoteErrorKind::PermissionDenied,
                "ftp_permission_denied",
                RetryDisposition::UserAction,
            ),
            (
                FtpFailureKind::NotFound,
                RemoteErrorKind::NotFound,
                "ftp_path_not_found",
                RetryDisposition::UserAction,
            ),
            (
                FtpFailureKind::Timeout,
                RemoteErrorKind::Timeout,
                "ftp_operation_timed_out",
                RetryDisposition::Backoff,
            ),
            (
                FtpFailureKind::Protocol,
                RemoteErrorKind::RemoteProtocol,
                "ftp_command_failed",
                RetryDisposition::Never,
            ),
            (
                FtpFailureKind::Transport,
                RemoteErrorKind::Transport,
                "ftp_transport_failed",
                RetryDisposition::Backoff,
            ),
        ];

        for (failure, kind, reason, retry) in cases {
            let mapped = map_ftp_error(
                &FtpError::Remote {
                    code: None,
                    failure,
                    reason: "fixture".into(),
                },
                RemoteOperation::List,
            );
            assert_eq!(mapped.kind, kind);
            assert_eq!(mapped.operation, RemoteOperation::List);
            assert_eq!(mapped.reason.as_str(), reason);
            assert_eq!(mapped.retry, retry);
        }

        for (code, expected_kind, expected_retry) in [
            (
                530,
                RemoteErrorKind::Authentication,
                RetryDisposition::Reauthenticate,
            ),
            (550, RemoteErrorKind::NotFound, RetryDisposition::UserAction),
        ] {
            let mapped = map_ftp_error(
                &FtpError::Remote {
                    code: Some(code),
                    failure: FtpFailureKind::Transport,
                    reason: "fixture".into(),
                },
                RemoteOperation::Stat,
            );
            assert_eq!(mapped.kind, expected_kind);
            assert_eq!(mapped.operation, RemoteOperation::Stat);
            assert_eq!(mapped.retry, expected_retry);
        }
    }

    #[test]
    fn controlled_session_rejects_pre_cancelled_read_before_backend_io() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, _) = fixture_adapter(backend.clone());
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();
        let control =
            RemoteIoControl::new(std::time::Instant::now() + std::time::Duration::from_secs(5));
        control.cancel();

        let error = ready(session.read_chunk_controlled(
            RemoteReadRequest {
                path: CoreRemotePath::new("/report.bin").unwrap(),
                offset: 0,
                max_bytes: 16,
                expected_identity: None,
            },
            control,
        ))
        .unwrap_err();

        assert_eq!(
            session.io_control_support(),
            RemoteIoControlSupport::Supported
        );
        assert_eq!(error.kind, RemoteErrorKind::Cancelled);
        assert_eq!(error.reason.as_str(), "remote_io_cancelled");
        assert_eq!(backend.read_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bridge_resolves_secret_without_exposing_it_in_debug_or_session_state() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend.clone());
        let secret = "fixture-super-secret";
        let profile = ftps_profile(Authentication::Password {
            secret: SecretRef::secret_service(
                Uuid::parse_str("12345678-1234-5678-1234-567812345678").unwrap(),
            ),
        });
        let store = FakeSecretStore {
            value: secret.as_bytes().to_vec(),
        };

        let session = ready(adapter.connect(&profile, &store)).unwrap();

        assert_eq!(backend.probe_count.load(Ordering::SeqCst), 1);
        assert_eq!(session.snapshot().state, ConnectionState::Ready);
        assert!(!format!("{profile:?}").contains(secret));
        assert!(!connector.config_debug.lock().unwrap()[0].contains(secret));
        assert!(!format!("{:?}", session.snapshot()).contains(secret));
    }

    #[test]
    fn unsupported_capabilities_and_identity_preconditions_have_explicit_reasons() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, _) = fixture_adapter(backend);
        for operation in [
            FileOperation::List,
            FileOperation::Stat,
            FileOperation::Read,
            FileOperation::Write,
            FileOperation::ResumeRead,
            FileOperation::ResumeWrite,
            FileOperation::CreateDirectory,
            FileOperation::Rename,
            FileOperation::Delete,
        ] {
            assert_eq!(
                adapter.capabilities().status(operation),
                &CapabilityStatus::Supported
            );
        }
        for (operation, reason_code) in [
            (FileOperation::AtomicRename, ATOMIC_RENAME_UNVERIFIED),
            (FileOperation::SetPermissions, SET_PERMISSIONS_UNSUPPORTED),
        ] {
            assert_eq!(
                adapter.capabilities().status(operation),
                &CapabilityStatus::Unsupported(reason(reason_code))
            );
        }
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();
        let error = ready(session.begin_write(BeginWriteRequest {
            final_path: CoreRemotePath::new("/report.bin").unwrap(),
            temporary_path: CoreRemotePath::new("/report.bin.part").unwrap(),
            expected_size_bytes: Some(6),
            resume_from: None,
            expected_destination: Some(ObjectIdentity {
                size_bytes: Some(1),
                modified_at_unix_ms: None,
                etag: None,
            }),
        }))
        .unwrap_err();
        assert_eq!(error.kind, RemoteErrorKind::Unsupported);
        assert_eq!(error.reason.as_str(), IDENTITY_PRECONDITION_UNSUPPORTED);
    }

    #[test]
    fn stat_and_read_use_the_backend_when_supported() {
        let backend = Arc::new(FakeBackend::default());
        *backend.file.lock().unwrap() = b"fixture".to_vec();
        let (adapter, _) = fixture_adapter(backend);
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();
        let path = CoreRemotePath::new("/report.bin").unwrap();

        let stat = ready(session.stat(&path)).unwrap();
        assert_eq!(stat.identity.size_bytes, Some(7));

        let read = ready(session.read_chunk(RemoteReadRequest {
            path,
            offset: 0,
            max_bytes: 16,
            expected_identity: None,
        }))
        .unwrap();
        assert_eq!(read.bytes, b"fixture");
        assert!(read.eof);
    }

    #[test]
    fn listing_parses_mlsd_type_and_size_facts() {
        let backend = Arc::new(FakeBackend::default());
        *backend.listing.lock().unwrap() =
            b"type=dir;modify=20260813083000; user1_2\r\ntype=file;size=7; alpha.txt\r\n".to_vec();
        let (adapter, _) = fixture_adapter(backend);
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();

        let entries = ready(session.list(&CoreRemotePath::new("/docs").unwrap())).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "user1_2");
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[0].identity.size_bytes, None);
        assert_eq!(entries[0].identity.modified_at_unix_ms, None);
        assert_eq!(entries[0].identity.etag, None);
        assert_eq!(entries[0].unix_mode, None);
        assert_eq!(entries[1].name, "alpha.txt");
        assert_eq!(entries[1].kind, EntryKind::File);
        assert_eq!(entries[1].identity.size_bytes, Some(7));
    }

    #[test]
    fn listing_omits_mlsd_current_and_parent_directory_entries() {
        let backend = Arc::new(FakeBackend::default());
        *backend.listing.lock().unwrap() =
            b"type=cdir; .\r\ntype=pdir; ..\r\ntype=dir; reports\r\n".to_vec();
        let (adapter, _) = fixture_adapter(backend);
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();

        let entries = ready(session.list(&CoreRemotePath::new("/docs").unwrap())).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.as_str(), "/docs/reports");
        assert_eq!(entries[0].kind, EntryKind::Directory);
    }

    #[test]
    fn supported_write_stages_and_commits_through_the_backend() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, _) = fixture_adapter(backend.clone());
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();
        let started = ready(session.begin_write(BeginWriteRequest {
            final_path: CoreRemotePath::new("/report.bin").unwrap(),
            temporary_path: CoreRemotePath::new("/report.bin.part").unwrap(),
            expected_size_bytes: Some(6),
            resume_from: None,
            expected_destination: None,
        }))
        .unwrap();
        let written = ready(session.write_chunk(started.handle, 0, b"report".to_vec())).unwrap();
        assert_eq!(written.next_offset, 6);
        let committed = ready(session.commit_write(started.handle, None)).unwrap();

        assert_eq!(committed.path.as_str(), "/report.bin");
        assert_eq!(
            backend.uploads.lock().unwrap().as_slice(),
            &[UploadRecord {
                temporary: "/report.bin.part".into(),
                final_path: "/report.bin".into(),
                resume_from: None,
                bytes: b"report".to_vec(),
            }]
        );
    }

    #[test]
    fn resume_write_rejects_remote_partial_size_drift_before_creating_handle() {
        let backend = Arc::new(FakeBackend::default());
        *backend.file.lock().unwrap() = b"bad".to_vec();
        let (adapter, _) = fixture_adapter(backend.clone());
        let session = ready(adapter.connect(
            &ftps_profile(Authentication::Anonymous),
            &FakeSecretStore { value: Vec::new() },
        ))
        .unwrap();

        let error = ready(session.begin_write(BeginWriteRequest {
            final_path: CoreRemotePath::new("/report.bin").unwrap(),
            temporary_path: CoreRemotePath::new("/report.bin.part").unwrap(),
            expected_size_bytes: Some(10),
            resume_from: Some(5),
            expected_destination: None,
        }))
        .unwrap_err();

        assert_eq!(error.kind, RemoteErrorKind::Conflict);
        assert_eq!(error.operation, RemoteOperation::Resume);
        assert_eq!(error.reason.as_str(), "ftp_resume_partial_size_mismatch");
        assert!(backend.uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn explicit_ftps_rejects_clear_data_profile_before_connector_build() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend);
        let mut profile = ftps_profile(Authentication::Anonymous);
        profile.options = ProfileOptions::FtpsExplicit {
            data_connection: DataConnectionMode::Passive,
            require_protected_data_channel: false,
        };

        let error = ready(adapter.connect(&profile, &FakeSecretStore { value: Vec::new() }))
            .err()
            .expect("clear data profile must fail");

        assert_eq!(error.kind, RemoteErrorKind::Unsupported);
        assert_eq!(error.reason.as_str(), "ftps_protected_data_required");
        assert!(connector.config_debug.lock().unwrap().is_empty());
    }

    #[test]
    fn active_mode_without_restricted_binding_is_explicitly_unsupported() {
        let backend = Arc::new(FakeBackend::default());
        let (adapter, connector) = fixture_adapter(backend);
        let mut profile = ftps_profile(Authentication::Anonymous);
        profile.options = ProfileOptions::FtpsExplicit {
            data_connection: DataConnectionMode::ActiveRestricted,
            require_protected_data_channel: true,
        };

        let error = ready(adapter.connect(&profile, &FakeSecretStore { value: Vec::new() }))
            .err()
            .expect("active profile without binding must fail");

        assert_eq!(
            RemoteFtpAdapter::default().protocol(),
            RemoteProtocol::FtpsExplicit
        );
        assert_eq!(error.kind, RemoteErrorKind::Unsupported);
        assert_eq!(error.reason.as_str(), "ftp_active_binding_required");
        assert!(connector.config_debug.lock().unwrap().is_empty());
    }

    #[test]
    fn plain_ftp_bridge_requires_confirmation_and_reports_degraded_availability() {
        let backend = Arc::new(FakeBackend::default());
        let connector = Arc::new(FakeConnector {
            backend: backend.clone(),
            config_debug: Mutex::new(Vec::new()),
            ca_certificates: Mutex::new(Vec::new()),
        });
        let confirmation =
            PlainFtpConfirmation::acknowledge(crate::PLAIN_FTP_ACKNOWLEDGEMENT).unwrap();
        let adapter = RemoteFtpAdapter::plain_ftp(confirmation).with_connector(connector.clone());

        let session =
            ready(adapter.connect(&ftp_profile(), &FakeSecretStore { value: Vec::new() })).unwrap();

        assert_eq!(adapter.protocol(), RemoteProtocol::Ftp);
        assert_eq!(
            adapter.availability(),
            AdapterAvailability::Degraded(reason("plain_ftp_explicitly_enabled"))
        );
        assert_eq!(session.snapshot().state, ConnectionState::Ready);
        assert_eq!(backend.probe_count.load(Ordering::SeqCst), 1);
        assert!(connector.config_debug.lock().unwrap()[0].contains("PlainFtp"));
    }
}
