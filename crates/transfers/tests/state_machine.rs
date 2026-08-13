use localdesk_remote_core::{
    CapabilityMatrix, CapabilityStatus, FILE_OPERATIONS, FileOperation, ObjectIdentity,
    OperationCapability, ProfileId, RemoteErrorKind, RemoteOperation, RemotePath, RemoteProtocol,
    RetryDisposition, SafeReason,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, FeatureSupport, LocalFileHandle, RetryPolicy,
    TransferCheckpoint, TransferCompletion, TransferConflict, TransferDirection, TransferEndpoint,
    TransferFailure, TransferFeatureSet, TransferId, TransferMutationError, TransferState,
    TransferTask, VerificationLevel,
};

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).unwrap()
}

fn capabilities(resume: bool) -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: if resume
                && matches!(
                    operation,
                    FileOperation::ResumeRead | FileOperation::ResumeWrite
                ) {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported(reason("adapter_did_not_report_support"))
            },
        }
    }))
    .unwrap()
}

fn task(direction: TransferDirection, resume: bool) -> TransferTask {
    let remote = TransferEndpoint::Remote(localdesk_transfers::RemoteTransferEndpoint {
        profile_id: ProfileId::new(),
        protocol: RemoteProtocol::Sftp,
        path: RemotePath::new("/data.bin").unwrap(),
    });
    let local = TransferEndpoint::Local {
        handle: LocalFileHandle::new(),
    };
    let (source, destination) = match direction {
        TransferDirection::Upload => (local, remote),
        TransferDirection::Download => (remote, local),
    };
    let mut features =
        TransferFeatureSet::from_adapter(direction, RemoteProtocol::Sftp, &capabilities(resume));
    if resume {
        features.pause = FeatureSupport::Supported;
    }
    TransferTask::new(
        TransferId::new(),
        source,
        destination,
        direction,
        Some(ObjectIdentity {
            size_bytes: Some(100),
            modified_at_unix_ms: Some(1),
            etag: Some("source-v1".into()),
        }),
        None,
        RetryPolicy::default(),
        BandwidthLimit::bytes_per_second(10_000).unwrap(),
        ConflictPolicy::Fail,
        features,
        1,
    )
    .unwrap()
}

fn checkpoint(offset: u64, at: i64) -> TransferCheckpoint {
    TransferCheckpoint {
        offset,
        source_identity: None,
        destination_identity: None,
        verification: VerificationLevel::RemoteIdentity,
        verified_at_unix_ms: at,
    }
}

#[test]
fn pause_resume_is_capability_gated_and_invalidates_late_callbacks() {
    let mut unsupported = task(TransferDirection::Download, false);
    unsupported.start(2).unwrap();
    assert!(matches!(
        unsupported.request_pause(3),
        Err(TransferMutationError::UnsupportedFeature(_))
    ));

    let mut supported = task(TransferDirection::Download, true);
    let first_run = supported.start(2).unwrap();
    supported
        .record_progress(first_run, 40, Some(100), Some(20), 3)
        .unwrap();
    supported.request_pause(4).unwrap();
    supported
        .confirm_paused(first_run, checkpoint(40, 4), 5)
        .unwrap();
    assert!(supported.resume(6).is_ok());
    let second_run = supported.start(7).unwrap();
    assert_eq!(
        supported.record_progress(first_run, 50, Some(100), Some(20), 8),
        Err(TransferMutationError::StaleRunToken)
    );
    assert!(
        supported
            .record_progress(second_run, 50, Some(100), Some(20), 8)
            .is_ok()
    );
}

#[test]
fn cancel_waits_for_running_io_and_rejects_late_progress() {
    let mut transfer = task(TransferDirection::Upload, true);
    let run = transfer.start(2).unwrap();
    transfer
        .record_progress(run, 25, Some(100), Some(10), 3)
        .unwrap();
    transfer.request_cancel(4).unwrap();
    assert!(matches!(transfer.state, TransferState::Cancelling));
    assert!(
        transfer
            .record_progress(run, 30, Some(100), Some(10), 5)
            .is_err()
    );
    transfer
        .confirm_cancelled(run, Some(checkpoint(25, 5)), 6)
        .unwrap();
    assert!(matches!(transfer.state, TransferState::Cancelled { .. }));
    assert_eq!(
        transfer.record_progress(run, 30, Some(100), Some(10), 7),
        Err(TransferMutationError::StaleRunToken)
    );
}

#[test]
fn retry_is_bounded_and_auth_failure_is_not_automatically_retryable() {
    let mut transfer = task(TransferDirection::Download, true);
    let run = transfer.start(2).unwrap();
    transfer
        .fail(
            run,
            TransferFailure {
                kind: RemoteErrorKind::Transport,
                operation: RemoteOperation::Read,
                reason: reason("connection_reset"),
                retry: RetryDisposition::Backoff,
            },
            3,
        )
        .unwrap();
    transfer.schedule_retry(4).unwrap();
    assert_eq!(
        transfer.start(1_003),
        Err(TransferMutationError::RetryNotReady)
    );
    assert!(transfer.start(1_004).is_ok());

    let mut auth = task(TransferDirection::Download, true);
    let run = auth.start(2).unwrap();
    auth.fail(
        run,
        TransferFailure {
            kind: RemoteErrorKind::Authentication,
            operation: RemoteOperation::Connect,
            reason: reason("credentials_rejected"),
            retry: RetryDisposition::Never,
        },
        3,
    )
    .unwrap();
    assert_eq!(
        auth.schedule_retry(4),
        Err(TransferMutationError::NotRetryable)
    );
}

#[test]
fn conflict_requires_explicit_policy_and_resume_support() {
    let mut transfer = task(TransferDirection::Upload, false);
    let run = transfer.start(2).unwrap();
    transfer
        .enter_conflict(
            run,
            TransferConflict {
                reason: reason("destination_changed"),
                checkpoint: Some(checkpoint(0, 2)),
            },
            3,
        )
        .unwrap();
    assert!(matches!(
        transfer.resolve_conflict(ConflictPolicy::Resume, 4),
        Err(TransferMutationError::UnsupportedFeature(_))
    ));
    transfer
        .resolve_conflict(ConflictPolicy::Rename, 4)
        .unwrap();
    assert!(matches!(transfer.state, TransferState::Queued));
}

#[test]
fn completion_requires_actual_progress_and_records_verification_level() {
    let mut transfer = task(TransferDirection::Download, true);
    let run = transfer.start(2).unwrap();
    assert_eq!(
        transfer.complete(
            run,
            TransferCompletion {
                verification: VerificationLevel::Unverified,
                identity: None,
                completed_at_unix_ms: 3,
            }
        ),
        Err(TransferMutationError::IncompleteProgress)
    );
    transfer
        .record_progress(run, 100, Some(100), Some(50), 3)
        .unwrap();
    transfer
        .complete(
            run,
            TransferCompletion {
                verification: VerificationLevel::RemoteIdentity,
                identity: None,
                completed_at_unix_ms: 4,
            },
        )
        .unwrap();
    assert!(matches!(transfer.state, TransferState::Completed { .. }));
}

#[test]
fn serialized_task_has_no_secret_or_filesystem_path_fields() {
    let transfer = task(TransferDirection::Upload, true);
    let value = serde_json::to_value(&transfer).unwrap();

    fn assert_safe_keys(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    assert!(!matches!(
                        key.as_str(),
                        "secret" | "secret_ref" | "password" | "token" | "local_path"
                    ));
                    assert_safe_keys(value);
                }
            }
            serde_json::Value::Array(values) => values.iter().for_each(assert_safe_keys),
            _ => {}
        }
    }
    assert_safe_keys(&value);
}

#[test]
fn feature_support_is_explicit_in_task_contract() {
    let transfer = task(TransferDirection::Download, false);
    assert!(matches!(
        transfer.features.pause,
        FeatureSupport::Unsupported(_)
    ));
    assert!(matches!(
        transfer.features.resume,
        FeatureSupport::Unsupported(_)
    ));
}
