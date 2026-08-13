use localdesk_remote_core::{
    Authentication, CapabilityMatrix, CapabilityMatrixError, CapabilityStatus, ConnectionState,
    EntryKind, FILE_OPERATIONS, FileOperation, FirstUsePolicy, ObjectIdentity, OperationCapability,
    ProfileId, ProfileOptions, RemoteConnectionProfile, RemoteEndpoint, RemoteEntry, RemotePath,
    RemoteProtocol, RemoteReadChunk, RemoteSession, SafeReason, SecretCommand, SecretInput,
    SecretKind, SecretRef, SessionId, TrustPolicy,
};
use uuid::Uuid;

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).unwrap()
}

fn matrix(supported: &[FileOperation]) -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: if supported.contains(&operation) {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Unsupported(reason("server_did_not_report_capability"))
            },
        }
    }))
    .unwrap()
}

#[test]
fn profile_serialization_contains_only_an_opaque_reference() {
    let profile = RemoteConnectionProfile::new(
        ProfileId::new(),
        "production sftp",
        RemoteProtocol::Sftp,
        RemoteEndpoint::new("files.example.test", 22).unwrap(),
        Some("operator".into()),
        None,
        Authentication::Password {
            secret: SecretRef::secret_service(
                Uuid::parse_str("12345678-1234-5678-1234-567812345678").unwrap(),
            ),
        },
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )
    .unwrap();

    let serialized = serde_json::to_string(&profile).unwrap();
    let debug = format!("{profile:?}");

    assert!(serialized.contains("secret_service"));
    assert!(serialized.contains("12345678-1234-5678-1234-567812345678"));
    for forbidden in ["password_value", "private_key_value", "token_value"] {
        assert!(!serialized.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
    assert!(debug.contains("<opaque>"));
    assert!(!debug.contains("12345678-1234-5678-1234-567812345678"));
}

#[test]
fn profile_rejects_protocol_policy_mismatches_and_credential_urls() {
    assert!(RemoteEndpoint::new("user:secret@example.test", 22).is_err());
    let result = RemoteConnectionProfile::new(
        ProfileId::new(),
        "bad policy",
        RemoteProtocol::FtpsExplicit,
        RemoteEndpoint::new("files.example.test", 21).unwrap(),
        Some("operator".into()),
        None,
        Authentication::Anonymous,
        TrustPolicy::PlaintextAcknowledged,
        ProfileOptions::FtpsExplicit {
            data_connection: localdesk_remote_core::DataConnectionMode::Passive,
            require_protected_data_channel: true,
        },
    );
    assert!(result.is_err());
}

#[test]
fn capability_matrix_requires_explicit_protocol_answers() {
    let incomplete = CapabilityMatrix::complete([OperationCapability {
        operation: FileOperation::Read,
        status: CapabilityStatus::Supported,
    }]);
    assert_eq!(
        incomplete,
        Err(CapabilityMatrixError::Missing(FileOperation::List))
    );

    let complete = matrix(&[FileOperation::Read, FileOperation::ResumeRead]);
    assert!(complete.status(FileOperation::Read).is_supported());
    assert!(!complete.status(FileOperation::AtomicRename).is_supported());
}

#[test]
fn remote_entry_keeps_unreported_metadata_optional() {
    let entry = RemoteEntry {
        name: "report.txt".into(),
        path: RemotePath::new("/report.txt").unwrap(),
        kind: EntryKind::File,
        identity: ObjectIdentity {
            size_bytes: Some(100),
            modified_at_unix_ms: None,
            etag: None,
        },
        unix_mode: None,
        capabilities: matrix(&[FileOperation::Read]),
    };
    let value = serde_json::to_value(entry).unwrap();
    assert!(value["unix_mode"].is_null());
    assert!(value["identity"]["modified_at_unix_ms"].is_null());
}

#[test]
fn session_state_machine_rejects_unproven_recovery() {
    let mut session = RemoteSession {
        id: SessionId::new(),
        profile_id: ProfileId::new(),
        protocol: RemoteProtocol::Sftp,
        state: ConnectionState::Disconnected,
        capabilities: matrix(&[]),
        opened_at_unix_ms: 10,
        updated_at_unix_ms: 10,
    };

    assert!(session.transition(ConnectionState::Ready, 11).is_err());
    assert!(session.transition(ConnectionState::Connecting, 11).unwrap());
    assert!(
        session
            .transition(ConnectionState::Authenticating, 12)
            .unwrap()
    );
    assert!(session.transition(ConnectionState::Ready, 13).unwrap());
    assert!(
        session
            .transition(ConnectionState::Reconnecting { attempt: 1 }, 14)
            .unwrap()
    );
    assert!(session.transition(ConnectionState::Ready, 15).unwrap());
    assert!(!session.transition(ConnectionState::Ready, 16).unwrap());
}

#[test]
fn transferred_file_bytes_are_redacted_from_debug_output() {
    let chunk = RemoteReadChunk {
        offset: 0,
        bytes: b"confidential file contents".to_vec(),
        eof: true,
        identity: ObjectIdentity {
            size_bytes: Some(26),
            modified_at_unix_ms: None,
            etag: None,
        },
    };

    let debug = format!("{chunk:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("confidential file contents"));
}

#[test]
fn secret_commands_redact_material_from_debug_and_bound_input() {
    let command = SecretCommand::Store {
        kind: SecretKind::Password,
        value: SecretInput::new(b"fixture-super-secret".to_vec()).expect("secret"),
    };
    let debug = format!("{command:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("fixture-super-secret"));
    assert!(SecretInput::new(Vec::new()).is_err());
    assert!(SecretInput::new(vec![0; localdesk_remote_core::MAX_SECRET_INPUT_BYTES + 1]).is_err());
}
