use crate::model::expected_size;
use crate::{
    BandwidthLimit, ConflictPolicy, FeatureSupport, RetryPolicy, TransferDirection,
    TransferEndpoint, TransferFeatureSet, TransferId, TransferProgress, TransferValidationError,
};
use localdesk_remote_core::{
    ObjectIdentity, RemoteError, RemoteErrorKind, RemoteOperation, RetryDisposition, SafeReason,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunToken {
    pub task_id: TransferId,
    generation: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateChange {
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStateKind {
    Queued,
    Running,
    Pausing,
    Paused,
    Cancelling,
    RetryScheduled,
    Conflict,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferCheckpoint {
    pub offset: u64,
    pub source_identity: Option<ObjectIdentity>,
    pub destination_identity: Option<ObjectIdentity>,
    pub verification: VerificationLevel,
    pub verified_at_unix_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferFailure {
    pub kind: RemoteErrorKind,
    pub operation: RemoteOperation,
    pub reason: SafeReason,
    pub retry: RetryDisposition,
}

impl From<RemoteError> for TransferFailure {
    fn from(value: RemoteError) -> Self {
        Self {
            kind: value.kind,
            operation: value.operation,
            reason: value.reason,
            retry: value.retry,
        }
    }
}

impl TransferFailure {
    pub const fn is_retryable(&self) -> bool {
        self.retry.is_retryable()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    Size,
    RemoteIdentity,
    Checksum,
    Unverified,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferCompletion {
    pub verification: VerificationLevel,
    pub identity: Option<ObjectIdentity>,
    pub completed_at_unix_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferConflict {
    pub reason: SafeReason,
    pub checkpoint: Option<TransferCheckpoint>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransferState {
    Queued,
    Running,
    Pausing,
    Paused {
        checkpoint: TransferCheckpoint,
    },
    Cancelling,
    RetryScheduled {
        not_before_unix_ms: i64,
        failure: TransferFailure,
    },
    Conflict {
        conflict: TransferConflict,
    },
    Completed {
        completion: TransferCompletion,
    },
    Failed {
        failure: TransferFailure,
    },
    Cancelled {
        checkpoint: Option<TransferCheckpoint>,
        cancelled_at_unix_ms: i64,
    },
}

impl TransferState {
    pub const fn kind(&self) -> TransferStateKind {
        match self {
            Self::Queued => TransferStateKind::Queued,
            Self::Running => TransferStateKind::Running,
            Self::Pausing => TransferStateKind::Pausing,
            Self::Paused { .. } => TransferStateKind::Paused,
            Self::Cancelling => TransferStateKind::Cancelling,
            Self::RetryScheduled { .. } => TransferStateKind::RetryScheduled,
            Self::Conflict { .. } => TransferStateKind::Conflict,
            Self::Completed { .. } => TransferStateKind::Completed,
            Self::Failed { .. } => TransferStateKind::Failed,
            Self::Cancelled { .. } => TransferStateKind::Cancelled,
        }
    }

    pub const fn is_active(&self) -> bool {
        matches!(
            Self::kind(self),
            TransferStateKind::Running | TransferStateKind::Pausing | TransferStateKind::Cancelling
        )
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferTask {
    pub id: TransferId,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub direction: TransferDirection,
    pub expected_source: Option<ObjectIdentity>,
    pub expected_destination: Option<ObjectIdentity>,
    pub state: TransferState,
    pub progress: TransferProgress,
    pub retry_policy: RetryPolicy,
    pub completed_attempts: u16,
    pub bandwidth_limit: BandwidthLimit,
    pub conflict_policy: ConflictPolicy,
    pub features: TransferFeatureSet,
    pub revision: u64,
    generation: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl TransferTask {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TransferId,
        source: TransferEndpoint,
        destination: TransferEndpoint,
        direction: TransferDirection,
        expected_source: Option<ObjectIdentity>,
        expected_destination: Option<ObjectIdentity>,
        retry_policy: RetryPolicy,
        bandwidth_limit: BandwidthLimit,
        conflict_policy: ConflictPolicy,
        features: TransferFeatureSet,
        created_at_unix_ms: i64,
    ) -> Result<Self, TransferValidationError> {
        direction.validate_endpoints(&source, &destination)?;
        let total_bytes = expected_size(&expected_source);
        Ok(Self {
            id,
            source,
            destination,
            direction,
            expected_source,
            expected_destination,
            state: TransferState::Queued,
            progress: TransferProgress::pending(total_bytes),
            retry_policy,
            completed_attempts: 0,
            bandwidth_limit,
            conflict_policy,
            features,
            revision: 0,
            generation: 0,
            created_at_unix_ms,
            updated_at_unix_ms: created_at_unix_ms,
        })
    }

    pub fn start(&mut self, now_unix_ms: i64) -> Result<RunToken, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        match &self.state {
            TransferState::Queued => {}
            TransferState::RetryScheduled {
                not_before_unix_ms, ..
            } if now_unix_ms >= *not_before_unix_ms => {}
            TransferState::RetryScheduled { .. } => {
                return Err(TransferMutationError::RetryNotReady);
            }
            _ => return Err(self.invalid_state("start")),
        }
        if self.completed_attempts >= self.retry_policy.max_attempts {
            return Err(TransferMutationError::RetryExhausted);
        }
        self.completed_attempts += 1;
        self.generation = self.generation.saturating_add(1);
        self.state = TransferState::Running;
        self.changed(now_unix_ms);
        Ok(RunToken {
            task_id: self.id,
            generation: self.generation,
        })
    }

    pub fn record_progress(
        &mut self,
        token: RunToken,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        bytes_per_second: Option<u64>,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Running | TransferState::Pausing) {
            return Err(self.invalid_state("record_progress"));
        }
        if bytes_transferred < self.progress.bytes_transferred {
            return Err(TransferMutationError::ProgressRegressed);
        }
        if let (Some(previous), Some(next)) = (self.progress.total_bytes, total_bytes)
            && previous != next
        {
            return Err(TransferMutationError::TotalChanged);
        }
        let total_bytes = total_bytes.or(self.progress.total_bytes);
        if total_bytes.is_some_and(|total| bytes_transferred > total) {
            return Err(TransferMutationError::ProgressExceedsTotal);
        }
        if bytes_per_second.is_some_and(|speed| speed > crate::MAX_BANDWIDTH_BYTES_PER_SECOND) {
            return Err(TransferMutationError::ReportedSpeedOutOfRange);
        }
        let next = TransferProgress {
            bytes_transferred,
            total_bytes,
            bytes_per_second,
            sampled_at_unix_ms: Some(now_unix_ms),
        };
        if self.progress == next {
            return Ok(StateChange::Unchanged);
        }
        self.progress = next;
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn request_pause(
        &mut self,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        match &self.features.pause {
            FeatureSupport::Supported => {}
            FeatureSupport::Unsupported(reason) => {
                return Err(TransferMutationError::UnsupportedFeature(reason.clone()));
            }
        }
        match self.state {
            TransferState::Paused { .. } | TransferState::Pausing => Ok(StateChange::Unchanged),
            TransferState::Running => {
                self.state = TransferState::Pausing;
                self.changed(now_unix_ms);
                Ok(StateChange::Changed)
            }
            _ => Err(self.invalid_state("request_pause")),
        }
    }

    pub fn confirm_paused(
        &mut self,
        token: RunToken,
        checkpoint: TransferCheckpoint,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Pausing) {
            return Err(self.invalid_state("confirm_paused"));
        }
        self.ensure_checkpoint(&checkpoint)?;
        self.state = TransferState::Paused { checkpoint };
        self.invalidate_run();
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn resume(&mut self, now_unix_ms: i64) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        match &self.features.resume {
            FeatureSupport::Supported => {}
            FeatureSupport::Unsupported(reason) => {
                return Err(TransferMutationError::UnsupportedFeature(reason.clone()));
            }
        }
        match self.state {
            TransferState::Queued => Ok(StateChange::Unchanged),
            TransferState::Paused { .. } => {
                self.state = TransferState::Queued;
                self.changed(now_unix_ms);
                Ok(StateChange::Changed)
            }
            _ => Err(self.invalid_state("resume")),
        }
    }

    pub fn request_cancel(
        &mut self,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        match self.state {
            TransferState::Cancelled { .. } | TransferState::Cancelling => {
                Ok(StateChange::Unchanged)
            }
            TransferState::Completed { .. } => Err(self.invalid_state("request_cancel")),
            TransferState::Running | TransferState::Pausing => {
                self.state = TransferState::Cancelling;
                self.changed(now_unix_ms);
                Ok(StateChange::Changed)
            }
            _ => {
                let checkpoint = self.current_checkpoint().cloned();
                self.state = TransferState::Cancelled {
                    checkpoint,
                    cancelled_at_unix_ms: now_unix_ms,
                };
                self.invalidate_run();
                self.changed(now_unix_ms);
                Ok(StateChange::Changed)
            }
        }
    }

    pub fn confirm_cancelled(
        &mut self,
        token: RunToken,
        checkpoint: Option<TransferCheckpoint>,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Cancelling) {
            return Err(self.invalid_state("confirm_cancelled"));
        }
        if let Some(checkpoint) = &checkpoint {
            self.ensure_checkpoint(checkpoint)?;
        }
        self.state = TransferState::Cancelled {
            checkpoint,
            cancelled_at_unix_ms: now_unix_ms,
        };
        self.invalidate_run();
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn complete(
        &mut self,
        token: RunToken,
        completion: TransferCompletion,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(completion.completed_at_unix_ms)?;
        if !matches!(self.state, TransferState::Running) {
            return Err(self.invalid_state("complete"));
        }
        if self
            .progress
            .total_bytes
            .is_some_and(|total| self.progress.bytes_transferred != total)
        {
            return Err(TransferMutationError::IncompleteProgress);
        }
        let at = completion.completed_at_unix_ms;
        self.state = TransferState::Completed { completion };
        self.invalidate_run();
        self.changed(at);
        Ok(StateChange::Changed)
    }

    pub fn fail(
        &mut self,
        token: RunToken,
        failure: TransferFailure,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Running | TransferState::Pausing) {
            return Err(self.invalid_state("fail"));
        }
        self.state = TransferState::Failed { failure };
        self.invalidate_run();
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn schedule_retry(
        &mut self,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        let failure = match &self.state {
            TransferState::Failed { failure } => failure.clone(),
            TransferState::RetryScheduled { .. } => return Ok(StateChange::Unchanged),
            _ => return Err(self.invalid_state("schedule_retry")),
        };
        if !failure.is_retryable() {
            return Err(TransferMutationError::NotRetryable);
        }
        if self.completed_attempts >= self.retry_policy.max_attempts {
            return Err(TransferMutationError::RetryExhausted);
        }
        let delay = self.retry_policy.backoff_ms(self.completed_attempts);
        let not_before_unix_ms = now_unix_ms.saturating_add_unsigned(delay);
        self.state = TransferState::RetryScheduled {
            not_before_unix_ms,
            failure,
        };
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn enter_conflict(
        &mut self,
        token: RunToken,
        conflict: TransferConflict,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_token(token)?;
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Running | TransferState::Pausing) {
            return Err(self.invalid_state("enter_conflict"));
        }
        if let Some(checkpoint) = &conflict.checkpoint {
            self.ensure_checkpoint(checkpoint)?;
        }
        self.state = TransferState::Conflict { conflict };
        self.invalidate_run();
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn resolve_conflict(
        &mut self,
        policy: ConflictPolicy,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        if !matches!(self.state, TransferState::Conflict { .. }) {
            return Err(self.invalid_state("resolve_conflict"));
        }
        if matches!(policy, ConflictPolicy::Resume) && !self.features.resume.is_supported() {
            let FeatureSupport::Unsupported(reason) = &self.features.resume else {
                unreachable!()
            };
            return Err(TransferMutationError::UnsupportedFeature(reason.clone()));
        }
        self.conflict_policy = policy;
        self.state = TransferState::Queued;
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn recover_after_restart(
        &mut self,
        now_unix_ms: i64,
    ) -> Result<StateChange, TransferMutationError> {
        self.check_time(now_unix_ms)?;
        if !self.state.is_active() {
            return Ok(StateChange::Unchanged);
        }
        self.state = TransferState::Failed {
            failure: TransferFailure {
                kind: RemoteErrorKind::Transport,
                operation: match self.direction {
                    TransferDirection::Upload => RemoteOperation::Write,
                    TransferDirection::Download => RemoteOperation::Read,
                },
                reason: SafeReason::new("app_restarted_state_unverified")
                    .expect("static reason is valid"),
                retry: RetryDisposition::Backoff,
            },
        };
        self.invalidate_run();
        self.changed(now_unix_ms);
        Ok(StateChange::Changed)
    }

    pub fn remote_profile_id(&self) -> localdesk_remote_core::ProfileId {
        self.source
            .remote()
            .or_else(|| self.destination.remote())
            .expect("validated transfer has one remote endpoint")
            .profile_id
    }

    fn check_token(&self, token: RunToken) -> Result<(), TransferMutationError> {
        if token.task_id != self.id || token.generation != self.generation {
            return Err(TransferMutationError::StaleRunToken);
        }
        Ok(())
    }

    fn check_time(&self, now_unix_ms: i64) -> Result<(), TransferMutationError> {
        if now_unix_ms < self.updated_at_unix_ms {
            return Err(TransferMutationError::TimestampRegressed);
        }
        Ok(())
    }

    fn ensure_checkpoint(
        &self,
        checkpoint: &TransferCheckpoint,
    ) -> Result<(), TransferMutationError> {
        if checkpoint.offset > self.progress.bytes_transferred {
            return Err(TransferMutationError::CheckpointExceedsProgress);
        }
        Ok(())
    }

    fn current_checkpoint(&self) -> Option<&TransferCheckpoint> {
        match &self.state {
            TransferState::Paused { checkpoint } => Some(checkpoint),
            TransferState::Conflict {
                conflict:
                    TransferConflict {
                        checkpoint: Some(checkpoint),
                        ..
                    },
            } => Some(checkpoint),
            _ => None,
        }
    }

    fn invalid_state(&self, operation: &'static str) -> TransferMutationError {
        TransferMutationError::InvalidState {
            operation,
            state: self.state.kind(),
        }
    }

    fn invalidate_run(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }

    fn changed(&mut self, at_unix_ms: i64) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at_unix_ms = at_unix_ms;
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum TransferMutationError {
    #[error("cannot {operation} while transfer is {state:?}")]
    InvalidState {
        operation: &'static str,
        state: TransferStateKind,
    },
    #[error("run token is stale")]
    StaleRunToken,
    #[error("transfer timestamp moved backwards")]
    TimestampRegressed,
    #[error("progress moved backwards")]
    ProgressRegressed,
    #[error("reported total size changed")]
    TotalChanged,
    #[error("progress exceeds total size")]
    ProgressExceedsTotal,
    #[error("reported speed exceeds hard bound")]
    ReportedSpeedOutOfRange,
    #[error("checkpoint exceeds persisted progress")]
    CheckpointExceedsProgress,
    #[error("transfer has not reached its reported total size")]
    IncompleteProgress,
    #[error("feature is unsupported: {0}")]
    UnsupportedFeature(SafeReason),
    #[error("retry delay has not elapsed")]
    RetryNotReady,
    #[error("failure is not retryable")]
    NotRetryable,
    #[error("retry attempts are exhausted")]
    RetryExhausted,
}
