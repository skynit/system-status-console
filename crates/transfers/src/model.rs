use localdesk_remote_core::{
    CapabilityMatrix, CapabilityStatus, FileOperation, ObjectIdentity, ProfileId, RemotePath,
    RemoteProtocol, SafeReason,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use thiserror::Error;
use uuid::Uuid;

pub const MAX_RETRY_ATTEMPTS: u16 = 20;
pub const MAX_BANDWIDTH_BYTES_PER_SECOND: u64 = 1024 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TransferId(Uuid);

impl TransferId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for TransferId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LocalFileHandle(Uuid);

impl LocalFileHandle {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for LocalFileHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteTransferEndpoint {
    pub profile_id: ProfileId,
    pub protocol: RemoteProtocol,
    pub path: RemotePath,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferEndpoint {
    Local { handle: LocalFileHandle },
    Remote(RemoteTransferEndpoint),
}

impl TransferEndpoint {
    pub fn remote(&self) -> Option<&RemoteTransferEndpoint> {
        match self {
            Self::Local { .. } => None,
            Self::Remote(remote) => Some(remote),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Download,
}

impl TransferDirection {
    pub fn validate_endpoints(
        self,
        source: &TransferEndpoint,
        destination: &TransferEndpoint,
    ) -> Result<(), TransferValidationError> {
        match (self, source, destination) {
            (Self::Upload, TransferEndpoint::Local { .. }, TransferEndpoint::Remote(_))
            | (Self::Download, TransferEndpoint::Remote(_), TransferEndpoint::Local { .. }) => {
                Ok(())
            }
            _ => Err(TransferValidationError::DirectionEndpointMismatch),
        }
    }

    pub const fn resume_operation(self) -> FileOperation {
        match self {
            Self::Upload => FileOperation::ResumeWrite,
            Self::Download => FileOperation::ResumeRead,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum FeatureSupport {
    Supported,
    Unsupported(SafeReason),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeValidation {
    RemoteIdentity,
    SizeOnly,
}

impl FeatureSupport {
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

impl From<&CapabilityStatus> for FeatureSupport {
    fn from(value: &CapabilityStatus) -> Self {
        match value {
            CapabilityStatus::Supported => Self::Supported,
            CapabilityStatus::Unsupported(reason) => Self::Unsupported(reason.clone()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferFeatureSet {
    pub pause: FeatureSupport,
    pub resume: FeatureSupport,
    pub resume_validation: Option<ResumeValidation>,
}

impl TransferFeatureSet {
    pub fn from_adapter(
        direction: TransferDirection,
        protocol: RemoteProtocol,
        capabilities: &CapabilityMatrix,
    ) -> Self {
        let resume = FeatureSupport::from(capabilities.status(direction.resume_operation()));
        let resume_validation = resume.is_supported().then_some(match protocol {
            RemoteProtocol::Ftp | RemoteProtocol::FtpsExplicit => ResumeValidation::SizeOnly,
            RemoteProtocol::Sftp => ResumeValidation::RemoteIdentity,
            RemoteProtocol::Ssh | RemoteProtocol::Smb => ResumeValidation::SizeOnly,
        });
        Self {
            pause: FeatureSupport::Unsupported(
                SafeReason::new("transfer_pause_requires_inflight_control")
                    .expect("static reason is valid"),
            ),
            resume,
            resume_validation,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Fail,
    Overwrite,
    Rename,
    Resume,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BandwidthLimit(Option<NonZeroU64>);

impl BandwidthLimit {
    pub const fn unlimited() -> Self {
        Self(None)
    }

    pub fn bytes_per_second(value: u64) -> Result<Self, TransferValidationError> {
        let value = NonZeroU64::new(value).ok_or(TransferValidationError::InvalidBandwidthLimit)?;
        if value.get() > MAX_BANDWIDTH_BYTES_PER_SECOND {
            return Err(TransferValidationError::InvalidBandwidthLimit);
        }
        Ok(Self(Some(value)))
    }

    pub const fn get(self) -> Option<NonZeroU64> {
        self.0
    }
}

impl Default for BandwidthLimit {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryPolicy {
    pub max_attempts: u16,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u16,
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
    ) -> Result<Self, RetryPolicyError> {
        if max_attempts == 0 || max_attempts > MAX_RETRY_ATTEMPTS {
            return Err(RetryPolicyError::InvalidAttempts);
        }
        if initial_backoff_ms == 0
            || max_backoff_ms < initial_backoff_ms
            || max_backoff_ms > 24 * 60 * 60 * 1000
        {
            return Err(RetryPolicyError::InvalidBackoff);
        }
        Ok(Self {
            max_attempts,
            initial_backoff_ms,
            max_backoff_ms,
        })
    }

    pub fn backoff_ms(self, completed_attempts: u16) -> u64 {
        let exponent = completed_attempts.saturating_sub(1).min(31) as u32;
        self.initial_backoff_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.max_backoff_ms)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff_ms: 1_000,
            max_backoff_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum RetryPolicyError {
    #[error("retry attempts must be between 1 and {MAX_RETRY_ATTEMPTS}")]
    InvalidAttempts,
    #[error("retry backoff must be non-zero, ordered, and at most 24 hours")]
    InvalidBackoff,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferProgress {
    pub bytes_transferred: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: Option<u64>,
    pub sampled_at_unix_ms: Option<i64>,
}

impl TransferProgress {
    pub const fn pending(total_bytes: Option<u64>) -> Self {
        Self {
            bytes_transferred: 0,
            total_bytes,
            bytes_per_second: None,
            sampled_at_unix_ms: None,
        }
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum TransferValidationError {
    #[error("transfer direction does not match source and destination endpoint kinds")]
    DirectionEndpointMismatch,
    #[error("bandwidth limit must be non-zero and within the hard upper bound")]
    InvalidBandwidthLimit,
    #[error("expected total size conflicts with source identity")]
    InconsistentExpectedSize,
}

pub(crate) fn expected_size(identity: &Option<ObjectIdentity>) -> Option<u64> {
    identity.as_ref().and_then(|value| value.size_bytes)
}
