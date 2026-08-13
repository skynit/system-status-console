use crate::{
    BandwidthLimit, ConflictPolicy, FeatureSupport, LocalFileHandle, RemoteTransferEndpoint,
    RetryPolicy, TransferDirection, TransferEndpoint, TransferFeatureSet, TransferId,
    TransferState, TransferStateKind, TransferTask,
};
use localdesk_remote_core::{ObjectIdentity, ProfileId, RemotePath, RemoteProtocol};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const TRANSFER_PUBLIC_SCHEMA_VERSION: u16 = 1;
pub const MAX_TRANSFER_PAGE_TASKS: u8 = 64;
pub const MAX_TRANSFER_QUERY_OFFSET: u32 = 100_000;
pub const MAX_TRANSFER_STATE_FILTERS: usize = 10;
pub const MAX_TRANSFER_REMOTE_PATH_BYTES: usize = 8 * 1024;
pub const MAX_TRANSFER_ETAG_BYTES: usize = 4 * 1024;
pub const MAX_PUBLIC_TRANSFER_TASK_BYTES: usize = 48 * 1024;
pub const MAX_LOCAL_HANDLE_DISPLAY_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferLocalHandlePurpose {
    UploadSource,
    DownloadDestination,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferLocalHandleGrant {
    pub handle: LocalFileHandle,
    pub purpose: TransferLocalHandlePurpose,
    pub display_name: String,
    pub size_bytes: Option<u64>,
}

impl TransferLocalHandleGrant {
    pub fn validate(&self) -> Result<(), TransferPublicError> {
        if self.handle.as_uuid().is_nil() {
            return Err(TransferPublicError::InvalidLocalHandle);
        }
        if self.display_name.is_empty()
            || self.display_name.len() > MAX_LOCAL_HANDLE_DISPLAY_NAME_BYTES
            || self.display_name.chars().any(char::is_control)
            || self.display_name.contains(['/', '\\'])
            || matches!(
                self.purpose,
                TransferLocalHandlePurpose::DownloadDestination
            ) && self.size_bytes.is_some()
        {
            return Err(TransferPublicError::InvalidLocalHandleGrant);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum TransferDraftEndpoint {
    Local {
        handle: LocalFileHandle,
    },
    Remote {
        profile_id: ProfileId,
        path: RemotePath,
    },
}

impl TransferDraftEndpoint {
    pub const fn local_handle(&self) -> Option<LocalFileHandle> {
        match self {
            Self::Local { handle } => Some(*handle),
            Self::Remote { .. } => None,
        }
    }

    pub const fn remote_profile_id(&self) -> Option<ProfileId> {
        match self {
            Self::Local { .. } => None,
            Self::Remote { profile_id, .. } => Some(*profile_id),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferDraft {
    pub id: TransferId,
    pub source: TransferDraftEndpoint,
    pub destination: TransferDraftEndpoint,
    pub direction: TransferDirection,
    pub expected_source: Option<ObjectIdentity>,
    pub expected_destination: Option<ObjectIdentity>,
    pub retry_policy: RetryPolicy,
    pub bandwidth_limit: BandwidthLimit,
    pub conflict_policy: ConflictPolicy,
}

impl TransferDraft {
    pub fn validate(&self) -> Result<(), TransferPublicError> {
        validate_id(self.id)?;
        validate_draft_endpoints(self.direction, &self.source, &self.destination)?;
        validate_identity(self.expected_source.as_ref())?;
        validate_identity(self.expected_destination.as_ref())?;
        validate_retry_policy(self.retry_policy)?;
        validate_bandwidth(self.bandwidth_limit)?;
        Ok(())
    }

    pub fn remote_profile_id(&self) -> ProfileId {
        self.source
            .remote_profile_id()
            .or_else(|| self.destination.remote_profile_id())
            .expect("validated draft has exactly one remote endpoint")
    }

    pub fn local_handle(&self) -> LocalFileHandle {
        self.source
            .local_handle()
            .or_else(|| self.destination.local_handle())
            .expect("validated draft has exactly one local endpoint")
    }

    pub fn into_task(
        self,
        protocol: RemoteProtocol,
        features: TransferFeatureSet,
        created_at_unix_ms: i64,
    ) -> Result<TransferTask, TransferPublicError> {
        self.validate()?;
        let source = draft_endpoint(self.source, protocol);
        let destination = draft_endpoint(self.destination, protocol);
        let task = TransferTask::new(
            self.id,
            source,
            destination,
            self.direction,
            self.expected_source,
            self.expected_destination,
            self.retry_policy,
            self.bandwidth_limit,
            self.conflict_policy,
            features,
            created_at_unix_ms,
        )?;
        task.validate_public()?;
        Ok(task)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferQuery {
    pub limit: u8,
    pub offset: u32,
    pub states: Vec<TransferStateKind>,
    #[serde(default)]
    pub direction: Option<TransferDirection>,
    pub profile_id: Option<ProfileId>,
}

impl TransferQuery {
    pub fn validate(&self) -> Result<(), TransferPublicError> {
        if self.limit == 0 || self.limit > MAX_TRANSFER_PAGE_TASKS {
            return Err(TransferPublicError::InvalidQueryLimit);
        }
        if self.offset > MAX_TRANSFER_QUERY_OFFSET {
            return Err(TransferPublicError::InvalidQueryOffset);
        }
        if self.states.len() > MAX_TRANSFER_STATE_FILTERS
            || self
                .states
                .iter()
                .enumerate()
                .any(|(index, state)| self.states[..index].contains(state))
        {
            return Err(TransferPublicError::InvalidStateFilters);
        }
        if self
            .profile_id
            .is_some_and(|profile_id| profile_id.as_uuid().is_nil())
        {
            return Err(TransferPublicError::InvalidProfileId);
        }
        Ok(())
    }

    pub fn matches(&self, task: &TransferTask) -> bool {
        (self.states.is_empty() || self.states.contains(&task.state.kind()))
            && self
                .direction
                .is_none_or(|direction| direction == task.direction)
            && self
                .profile_id
                .is_none_or(|profile_id| profile_id == task.remote_profile_id())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPage {
    pub query: TransferQuery,
    pub tasks: Vec<TransferTask>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

impl TransferPage {
    pub fn validate(&self) -> Result<(), TransferPublicError> {
        self.query.validate()?;
        if self.tasks.len() > usize::from(self.query.limit)
            || self
                .tasks
                .iter()
                .any(|task| task.validate_public().is_err() || !self.query.matches(task))
        {
            return Err(TransferPublicError::InvalidPage);
        }
        let expected_next = self
            .query
            .offset
            .checked_add(
                u32::try_from(self.tasks.len()).map_err(|_| TransferPublicError::InvalidPage)?,
            )
            .ok_or(TransferPublicError::InvalidPage)?;
        if self.has_more {
            if self.tasks.is_empty() || self.next_offset != Some(expected_next) {
                return Err(TransferPublicError::InvalidPage);
            }
        } else if self.next_offset.is_some() {
            return Err(TransferPublicError::InvalidPage);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "command", rename_all = "snake_case")]
pub enum TransferCommand {
    Enqueue {
        draft: TransferDraft,
    },
    List {
        query: TransferQuery,
    },
    Get {
        id: TransferId,
    },
    Cancel {
        id: TransferId,
        expected_revision: u64,
    },
    Retry {
        id: TransferId,
        expected_revision: u64,
    },
    ResolveConflict {
        id: TransferId,
        expected_revision: u64,
        policy: ConflictPolicy,
    },
}

impl TransferCommand {
    pub fn validate(&self) -> Result<(), TransferPublicError> {
        match self {
            Self::Enqueue { draft } => draft.validate(),
            Self::List { query } => query.validate(),
            Self::Get { id }
            | Self::Cancel { id, .. }
            | Self::Retry { id, .. }
            | Self::ResolveConflict { id, .. } => validate_id(*id),
        }
    }

    pub const fn task_id(&self) -> Option<TransferId> {
        match self {
            Self::Enqueue { draft } => Some(draft.id),
            Self::List { .. } => None,
            Self::Get { id }
            | Self::Cancel { id, .. }
            | Self::Retry { id, .. }
            | Self::ResolveConflict { id, .. } => Some(*id),
        }
    }

    pub const fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::Cancel {
                expected_revision, ..
            }
            | Self::Retry {
                expected_revision, ..
            }
            | Self::ResolveConflict {
                expected_revision, ..
            } => Some(*expected_revision),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "result", rename_all = "snake_case")]
pub enum TransferMutationResult {
    Updated {
        task: TransferTask,
    },
    Conflict {
        expected_revision: u64,
        current: TransferTask,
    },
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "output", rename_all = "snake_case")]
pub enum TransferOutput {
    Task { task: TransferTask },
    Page { page: TransferPage },
    Mutation { result: TransferMutationResult },
}

impl TransferOutput {
    pub fn validate_for(&self, command: &TransferCommand) -> Result<(), TransferPublicError> {
        command.validate()?;
        match (command, self) {
            (TransferCommand::Enqueue { draft }, Self::Task { task })
                if task.id == draft.id && task.validate_public().is_ok() =>
            {
                Ok(())
            }
            (TransferCommand::Get { id }, Self::Task { task })
                if task.id == *id && task.validate_public().is_ok() =>
            {
                Ok(())
            }
            (TransferCommand::List { query }, Self::Page { page })
                if page.query == *query && page.validate().is_ok() =>
            {
                Ok(())
            }
            (
                TransferCommand::Cancel {
                    id,
                    expected_revision,
                }
                | TransferCommand::Retry {
                    id,
                    expected_revision,
                }
                | TransferCommand::ResolveConflict {
                    id,
                    expected_revision,
                    ..
                },
                Self::Mutation { result },
            ) => validate_mutation(*id, *expected_revision, result),
            _ => Err(TransferPublicError::ResultMismatch),
        }
    }
}

impl TransferTask {
    pub fn validate_public(&self) -> Result<(), TransferPublicError> {
        validate_id(self.id)?;
        self.direction
            .validate_endpoints(&self.source, &self.destination)?;
        for endpoint in [&self.source, &self.destination] {
            match endpoint {
                TransferEndpoint::Local { handle } if handle.as_uuid().is_nil() => {
                    return Err(TransferPublicError::InvalidLocalHandle);
                }
                TransferEndpoint::Remote(remote) => {
                    if remote.profile_id.as_uuid().is_nil() {
                        return Err(TransferPublicError::InvalidProfileId);
                    }
                    validate_path(&remote.path)?;
                }
                TransferEndpoint::Local { .. } => {}
            }
        }
        validate_identity(self.expected_source.as_ref())?;
        validate_identity(self.expected_destination.as_ref())?;
        validate_state_identities(&self.state)?;
        validate_retry_policy(self.retry_policy)?;
        validate_bandwidth(self.bandwidth_limit)?;
        if self.completed_attempts > self.retry_policy.max_attempts
            || self.created_at_unix_ms < 0
            || self.updated_at_unix_ms < self.created_at_unix_ms
            || self
                .progress
                .total_bytes
                .is_some_and(|total| self.progress.bytes_transferred > total)
            || self
                .progress
                .bytes_per_second
                .is_some_and(|speed| speed > crate::MAX_BANDWIDTH_BYTES_PER_SECOND)
        {
            return Err(TransferPublicError::InvalidTask);
        }
        match (&self.features.resume, self.features.resume_validation) {
            (FeatureSupport::Supported, None) | (FeatureSupport::Unsupported(_), Some(_)) => {
                return Err(TransferPublicError::InvalidTask);
            }
            _ => {}
        }
        let encoded = serde_json::to_vec(self).map_err(|_| TransferPublicError::InvalidTask)?;
        if encoded.len() > MAX_PUBLIC_TRANSFER_TASK_BYTES {
            return Err(TransferPublicError::TaskTooLarge);
        }
        Ok(())
    }
}

fn validate_mutation(
    id: TransferId,
    expected_revision: u64,
    result: &TransferMutationResult,
) -> Result<(), TransferPublicError> {
    match result {
        TransferMutationResult::Updated { task }
            if task.id == id
                && task.revision >= expected_revision
                && task.validate_public().is_ok() =>
        {
            Ok(())
        }
        TransferMutationResult::Conflict {
            expected_revision: returned,
            current,
        } if *returned == expected_revision
            && current.id == id
            && current.revision != expected_revision
            && current.validate_public().is_ok() =>
        {
            Ok(())
        }
        _ => Err(TransferPublicError::ResultMismatch),
    }
}

fn draft_endpoint(endpoint: TransferDraftEndpoint, protocol: RemoteProtocol) -> TransferEndpoint {
    match endpoint {
        TransferDraftEndpoint::Local { handle } => TransferEndpoint::Local { handle },
        TransferDraftEndpoint::Remote { profile_id, path } => {
            TransferEndpoint::Remote(RemoteTransferEndpoint {
                profile_id,
                protocol,
                path,
            })
        }
    }
}

fn validate_draft_endpoints(
    direction: TransferDirection,
    source: &TransferDraftEndpoint,
    destination: &TransferDraftEndpoint,
) -> Result<(), TransferPublicError> {
    match (direction, source, destination) {
        (
            TransferDirection::Upload,
            TransferDraftEndpoint::Local { handle },
            TransferDraftEndpoint::Remote { profile_id, path },
        )
        | (
            TransferDirection::Download,
            TransferDraftEndpoint::Remote { profile_id, path },
            TransferDraftEndpoint::Local { handle },
        ) => {
            if handle.as_uuid().is_nil() {
                return Err(TransferPublicError::InvalidLocalHandle);
            }
            if profile_id.as_uuid().is_nil() {
                return Err(TransferPublicError::InvalidProfileId);
            }
            validate_path(path)
        }
        _ => Err(TransferPublicError::DirectionEndpointMismatch),
    }
}

fn validate_path(path: &RemotePath) -> Result<(), TransferPublicError> {
    if path.as_str().len() > MAX_TRANSFER_REMOTE_PATH_BYTES
        || path.as_str().chars().any(char::is_control)
    {
        return Err(TransferPublicError::RemotePathTooLarge);
    }
    Ok(())
}

fn validate_identity(identity: Option<&ObjectIdentity>) -> Result<(), TransferPublicError> {
    if identity
        .and_then(|value| value.etag.as_ref())
        .is_some_and(|etag| {
            etag.len() > MAX_TRANSFER_ETAG_BYTES || etag.chars().any(char::is_control)
        })
    {
        return Err(TransferPublicError::IdentityTooLarge);
    }
    Ok(())
}

fn validate_state_identities(state: &TransferState) -> Result<(), TransferPublicError> {
    let checkpoint = match state {
        TransferState::Paused { checkpoint }
        | TransferState::Conflict {
            conflict:
                crate::TransferConflict {
                    checkpoint: Some(checkpoint),
                    ..
                },
        }
        | TransferState::Cancelled {
            checkpoint: Some(checkpoint),
            ..
        } => Some(checkpoint),
        _ => None,
    };
    if let Some(checkpoint) = checkpoint {
        validate_identity(checkpoint.source_identity.as_ref())?;
        validate_identity(checkpoint.destination_identity.as_ref())?;
    }
    if let TransferState::Completed { completion } = state {
        validate_identity(completion.identity.as_ref())?;
    }
    Ok(())
}

fn validate_retry_policy(policy: RetryPolicy) -> Result<(), TransferPublicError> {
    RetryPolicy::new(
        policy.max_attempts,
        policy.initial_backoff_ms,
        policy.max_backoff_ms,
    )
    .map(|_| ())
    .map_err(Into::into)
}

fn validate_bandwidth(limit: BandwidthLimit) -> Result<(), TransferPublicError> {
    if limit
        .get()
        .is_some_and(|value| value.get() > crate::MAX_BANDWIDTH_BYTES_PER_SECOND)
    {
        return Err(TransferPublicError::InvalidBandwidth);
    }
    Ok(())
}

fn validate_id(id: TransferId) -> Result<(), TransferPublicError> {
    if id.as_uuid().is_nil() {
        return Err(TransferPublicError::InvalidTransferId);
    }
    Ok(())
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum TransferPublicError {
    #[error("transfer id must not be nil")]
    InvalidTransferId,
    #[error("local file handle must not be nil")]
    InvalidLocalHandle,
    #[error("local file handle grant contains invalid display metadata")]
    InvalidLocalHandleGrant,
    #[error("remote profile id must not be nil")]
    InvalidProfileId,
    #[error("transfer direction does not match source and destination")]
    DirectionEndpointMismatch,
    #[error("remote path exceeds the public bound or contains control characters")]
    RemotePathTooLarge,
    #[error("object identity etag exceeds the public bound or contains control characters")]
    IdentityTooLarge,
    #[error("transfer retry policy is invalid")]
    InvalidRetryPolicy,
    #[error("transfer bandwidth limit is invalid")]
    InvalidBandwidth,
    #[error("transfer query limit is outside 1..=64")]
    InvalidQueryLimit,
    #[error("transfer query offset exceeds 100000")]
    InvalidQueryOffset,
    #[error("transfer state filters are duplicated or exceed the hard bound")]
    InvalidStateFilters,
    #[error("transfer page is inconsistent with its query")]
    InvalidPage,
    #[error("transfer task violates the public contract")]
    InvalidTask,
    #[error("serialized transfer task exceeds the public frame-safe bound")]
    TaskTooLarge,
    #[error("transfer result does not match its command")]
    ResultMismatch,
}

impl From<crate::TransferValidationError> for TransferPublicError {
    fn from(value: crate::TransferValidationError) -> Self {
        match value {
            crate::TransferValidationError::DirectionEndpointMismatch => {
                Self::DirectionEndpointMismatch
            }
            crate::TransferValidationError::InvalidBandwidthLimit => Self::InvalidBandwidth,
            crate::TransferValidationError::InconsistentExpectedSize => Self::InvalidTask,
        }
    }
}

impl From<crate::RetryPolicyError> for TransferPublicError {
    fn from(_: crate::RetryPolicyError) -> Self {
        Self::InvalidRetryPolicy
    }
}
