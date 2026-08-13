use localdesk_remote_core::{
    ObjectIdentity, ProfileId, RemotePath, RemoteProtocol, SafeReason,
    unsupported_file_capabilities,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, LocalFileHandle, MAX_TRANSFER_ETAG_BYTES,
    MAX_TRANSFER_PAGE_TASKS, MAX_TRANSFER_QUERY_OFFSET, RetryPolicy, TransferDirection,
    TransferDraft, TransferDraftEndpoint, TransferFeatureSet, TransferId, TransferLocalHandleGrant,
    TransferLocalHandlePurpose, TransferPage, TransferPublicError, TransferQuery,
    TransferStateKind,
};

fn draft() -> TransferDraft {
    TransferDraft {
        id: TransferId::new(),
        source: TransferDraftEndpoint::Local {
            handle: LocalFileHandle::new(),
        },
        destination: TransferDraftEndpoint::Remote {
            profile_id: ProfileId::new(),
            path: RemotePath::new("/fixture.bin").expect("path"),
        },
        direction: TransferDirection::Upload,
        expected_source: None,
        expected_destination: None,
        retry_policy: RetryPolicy::default(),
        bandwidth_limit: BandwidthLimit::unlimited(),
        conflict_policy: ConflictPolicy::Fail,
    }
}

fn task() -> localdesk_transfers::TransferTask {
    let capabilities =
        unsupported_file_capabilities(SafeReason::new("fixture_unsupported").expect("reason"));
    draft()
        .into_task(
            RemoteProtocol::Sftp,
            TransferFeatureSet::from_adapter(
                TransferDirection::Upload,
                RemoteProtocol::Sftp,
                &capabilities,
            ),
            1,
        )
        .expect("task")
}

#[test]
fn enqueue_draft_rejects_server_owned_fields() {
    let mut value = serde_json::to_value(draft()).expect("serialize draft");
    value
        .as_object_mut()
        .expect("object")
        .insert("state".to_owned(), serde_json::json!("queued"));

    assert!(serde_json::from_value::<TransferDraft>(value).is_err());
}

#[test]
fn public_task_rejects_oversized_identity_before_wire_or_storage() {
    let mut draft = draft();
    draft.expected_source = Some(ObjectIdentity {
        size_bytes: Some(1),
        modified_at_unix_ms: Some(1),
        etag: Some("e".repeat(MAX_TRANSFER_ETAG_BYTES + 1)),
    });

    assert_eq!(draft.validate(), Err(TransferPublicError::IdentityTooLarge));
}

#[test]
fn local_handle_grant_is_bounded_and_never_contains_a_path() {
    let grant = TransferLocalHandleGrant {
        handle: LocalFileHandle::new(),
        purpose: TransferLocalHandlePurpose::UploadSource,
        display_name: "report.txt".to_owned(),
        size_bytes: Some(12),
    };
    assert_eq!(grant.validate(), Ok(()));
    let encoded = serde_json::to_value(&grant).expect("grant json");
    assert!(encoded.get("path").is_none());

    let mut invalid = grant.clone();
    invalid.display_name = "/tmp/report.txt".to_owned();
    assert_eq!(
        invalid.validate(),
        Err(TransferPublicError::InvalidLocalHandleGrant)
    );

    let invalid = TransferLocalHandleGrant {
        purpose: TransferLocalHandlePurpose::DownloadDestination,
        size_bytes: Some(12),
        ..grant
    };
    assert_eq!(
        invalid.validate(),
        Err(TransferPublicError::InvalidLocalHandleGrant)
    );
}

#[test]
fn query_and_page_bounds_are_exact() {
    let query = TransferQuery {
        limit: MAX_TRANSFER_PAGE_TASKS,
        offset: MAX_TRANSFER_QUERY_OFFSET,
        states: vec![TransferStateKind::Queued],
        direction: None,
        profile_id: None,
    };
    assert_eq!(query.validate(), Ok(()));

    let mut invalid = query.clone();
    invalid.offset += 1;
    assert_eq!(
        invalid.validate(),
        Err(TransferPublicError::InvalidQueryOffset)
    );

    let invalid = TransferQuery {
        states: vec![TransferStateKind::Queued, TransferStateKind::Queued],
        ..query.clone()
    };
    assert_eq!(
        invalid.validate(),
        Err(TransferPublicError::InvalidStateFilters)
    );

    let page = TransferPage {
        query: TransferQuery {
            limit: 1,
            offset: 0,
            states: Vec::new(),
            direction: None,
            profile_id: None,
        },
        tasks: vec![task(), task()],
        has_more: false,
        next_offset: None,
    };
    assert_eq!(page.validate(), Err(TransferPublicError::InvalidPage));
}

#[test]
fn query_direction_filter_matches_the_task_direction() {
    let upload = task();
    let upload_query = TransferQuery {
        limit: 1,
        offset: 0,
        states: Vec::new(),
        direction: Some(TransferDirection::Upload),
        profile_id: None,
    };
    let download_query = TransferQuery {
        direction: Some(TransferDirection::Download),
        ..upload_query.clone()
    };

    assert!(upload_query.matches(&upload));
    assert!(!download_query.matches(&upload));
}
