use localdesk_domain::{CapabilityRuntime, CapabilityRuntimeState};
use localdesk_ipc::{
    ClientError, RemoteCapabilitiesProvider, RemoteProfileProvider, RemoteSessionProvider,
    RequestEnvelope, SecretCommandProvider, ServerConfig, SnapshotProviderError, TerminalProvider,
    TerminalStreamEvent, request_remote_capabilities, request_remote_profile,
    request_remote_session, request_secret, request_terminal, request_terminal_stream, serve,
};
use localdesk_remote_core::{
    AdapterAvailability, Authentication, CapabilityStatus, ConnectionState, FirstUsePolicy,
    ProfileId, ProfileOptions, REMOTE_PROTOCOLS, RemoteAdapterCatalog, RemoteAdapterDescriptor,
    RemoteConnectionProfile, RemoteEndpoint, RemoteProfileCommand, RemoteProfilePage,
    RemoteProfilePageQuery, RemoteProfileResult, RemoteProtocol, RemoteSession,
    RemoteSessionCommand, RemoteSessionResult, SafeReason, SecretCommand, SecretCommandResult,
    SecretInput, SecretKind, SecretRef, SessionId, StoredRemoteProfile, TerminalCommand,
    TerminalData, TerminalRead, TerminalResult, TerminalSessionId, TerminalState, TerminalStatus,
    TrustPolicy, unsupported_file_capabilities,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch, time::Duration};
use uuid::Uuid;

fn runtime() -> CapabilityRuntime {
    CapabilityRuntime::new(
        CapabilityRuntimeState::healthy("appd_online"),
        CapabilityRuntimeState::degraded("telemetry_warming_up"),
        CapabilityRuntimeState::degraded("network_warming_up"),
        CapabilityRuntimeState::unsupported("per_app_unavailable"),
        CapabilityRuntimeState::degraded("usage_warming_up"),
    )
}

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).expect("reason")
}

fn catalog() -> RemoteAdapterCatalog {
    RemoteAdapterCatalog::new(
        Uuid::from_u128(1),
        1,
        REMOTE_PROTOCOLS
            .iter()
            .copied()
            .map(|protocol| RemoteAdapterDescriptor {
                protocol,
                availability: AdapterAvailability::Unsupported(reason("fixture_unavailable")),
                terminal: CapabilityStatus::Unsupported(reason("terminal_not_applicable")),
                file_operations: unsupported_file_capabilities(reason("fixture_unavailable")),
            })
            .collect(),
    )
}

fn profile() -> RemoteConnectionProfile {
    RemoteConnectionProfile::new(
        ProfileId::from_uuid(Uuid::from_u128(10)),
        "fixture sftp",
        RemoteProtocol::Sftp,
        RemoteEndpoint::new("files.example.test", 22).expect("endpoint"),
        Some("operator".to_owned()),
        None,
        Authentication::SshAgent,
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        ProfileOptions::Sftp {
            jump_profiles: Vec::new(),
        },
    )
    .expect("profile")
}

async fn spawn_server(
    config: ServerConfig,
) -> (
    tempfile::TempDir,
    PathBuf,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), localdesk_ipc::ServerError>>,
) {
    let directory = tempdir().expect("socket directory");
    let path = directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(serve(listener, config, shutdown_rx));
    (directory, path, shutdown_tx, server)
}

#[tokio::test]
async fn remote_catalog_roundtrip_preserves_complete_capability_matrix() {
    let expected = catalog();
    let provider: RemoteCapabilitiesProvider = Arc::new({
        let expected = expected.clone();
        move || {
            let value = expected.clone();
            Box::pin(async move { Ok(value) })
        }
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_remote_capabilities_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let actual = request_remote_capabilities(&path, RequestEnvelope::remote_capabilities())
        .await
        .expect("catalog");
    assert_eq!(actual, expected);
    assert_eq!(actual.validate(), Ok(()));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn missing_or_invalid_remote_provider_is_a_typed_terminal_error() {
    let config = ServerConfig::new("fixture", Arc::new(runtime));
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;
    let missing = request_remote_capabilities(&path, RequestEnvelope::remote_capabilities()).await;
    assert!(matches!(
        missing,
        Err(ClientError::Daemon(error))
            if error.code == "remote_capabilities_provider_unavailable"
    ));
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");

    let provider: RemoteCapabilitiesProvider = Arc::new(|| {
        let mut invalid = catalog();
        invalid.snapshot_id = Uuid::nil();
        Box::pin(async move { Ok(invalid) })
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_remote_capabilities_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;
    let invalid = request_remote_capabilities(&path, RequestEnvelope::remote_capabilities()).await;
    assert!(matches!(
        invalid,
        Err(ClientError::Daemon(error)) if error.code == "remote_catalog_invalid"
    ));
    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[test]
fn provider_error_type_remains_safe_and_typed() {
    let error = SnapshotProviderError::new(
        "remote_runtime_unavailable",
        "remote_state_directory_unsafe",
        false,
    );
    assert_eq!(error.code, "remote_runtime_unavailable");
    assert!(!error.retryable);
}

#[tokio::test]
async fn remote_profile_commands_are_request_typed_and_revision_checked() {
    let provider: RemoteProfileProvider = Arc::new(|command| {
        Box::pin(async move {
            let result = match command {
                RemoteProfileCommand::List { .. } => RemoteProfileResult::Page(RemoteProfilePage {
                    profiles: Vec::new(),
                    next_after: None,
                }),
                RemoteProfileCommand::Upsert {
                    profile,
                    expected_revision,
                } => RemoteProfileResult::Stored(StoredRemoteProfile {
                    profile,
                    revision: expected_revision.map_or(0, |revision| revision + 1),
                    created_at_unix_ms: 1,
                    updated_at_unix_ms: 1,
                }),
                RemoteProfileCommand::Delete { profile_id, .. } => {
                    RemoteProfileResult::Deleted { profile_id }
                }
            };
            Ok(result)
        })
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_remote_profile_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let list = RemoteProfileCommand::List {
        query: RemoteProfilePageQuery {
            after: None,
            limit: 16,
        },
    };
    assert!(matches!(
        request_remote_profile(&path, RequestEnvelope::remote_profile(list)).await,
        Ok(RemoteProfileResult::Page(RemoteProfilePage { profiles, .. })) if profiles.is_empty()
    ));

    let stored = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile: profile(),
            expected_revision: None,
        }),
    )
    .await
    .expect("stored");
    assert!(matches!(
        stored,
        RemoteProfileResult::Stored(StoredRemoteProfile { revision: 0, .. })
    ));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn secret_command_roundtrip_keeps_a_typed_opaque_reference() {
    let reference = SecretRef::secret_service(Uuid::from_u128(500));
    let expected_reference = reference.clone();
    let provider: SecretCommandProvider = Arc::new(move |command| {
        let reference = reference.clone();
        Box::pin(async move {
            Ok(match command {
                SecretCommand::Store { .. } => SecretCommandResult::Stored { reference },
                SecretCommand::Delete { .. } => SecretCommandResult::Deleted,
            })
        })
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_secret_command_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let result = request_secret(
        &path,
        RequestEnvelope::secret(SecretCommand::Store {
            kind: SecretKind::Password,
            value: SecretInput::new(b"fixture-secret".to_vec()).expect("secret"),
        }),
    )
    .await
    .expect("secret result");
    assert_eq!(
        result,
        SecretCommandResult::Stored {
            reference: expected_reference,
        }
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn remote_session_commands_roundtrip_through_a_fake_provider() {
    let profile_id = ProfileId::from_uuid(Uuid::from_u128(700));
    let session_id = SessionId::from_uuid(Uuid::from_u128(701));
    let expected = RemoteSession {
        id: session_id,
        profile_id,
        protocol: RemoteProtocol::Sftp,
        state: ConnectionState::Ready,
        capabilities: unsupported_file_capabilities(reason("fixture_operations_unavailable")),
        opened_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    };
    let provider: RemoteSessionProvider = Arc::new({
        let expected = expected.clone();
        move |command| {
            let expected = expected.clone();
            Box::pin(async move {
                match command {
                    RemoteSessionCommand::Connect { .. } => {
                        Ok(RemoteSessionResult::Session(expected))
                    }
                    RemoteSessionCommand::Disconnect { session_id } => {
                        Ok(RemoteSessionResult::Disconnected { session_id })
                    }
                    _ => Err(SnapshotProviderError::new(
                        "fixture_operation_unsupported",
                        "fixture_operation_unsupported",
                        false,
                    )),
                }
            })
        }
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_remote_session_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let connected = request_remote_session(
        &path,
        RequestEnvelope::remote_session(RemoteSessionCommand::Connect { profile_id }),
    )
    .await
    .expect("connected session");
    assert_eq!(connected, RemoteSessionResult::Session(expected));

    let disconnected = request_remote_session(
        &path,
        RequestEnvelope::remote_session(RemoteSessionCommand::Disconnect { session_id }),
    )
    .await
    .expect("disconnected session");
    assert_eq!(
        disconnected,
        RemoteSessionResult::Disconnected { session_id }
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn slow_remote_session_returns_typed_error_after_snapshot_deadline() {
    let profile_id = ProfileId::from_uuid(Uuid::from_u128(702));
    let provider: RemoteSessionProvider = Arc::new(move |_| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(5_100)).await;
            Err(SnapshotProviderError::new(
                "remote_authentication_error",
                "ssh_authentication_failed",
                false,
            ))
        })
    });
    let config =
        ServerConfig::new("fixture", Arc::new(runtime)).with_remote_session_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;

    let result = request_remote_session(
        &path,
        RequestEnvelope::remote_session(RemoteSessionCommand::Connect { profile_id }),
    )
    .await;
    assert!(matches!(
        result,
        Err(ClientError::Daemon(error))
            if error.code == "remote_authentication_error"
                && error.reason == "ssh_authentication_failed"
    ));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn bounded_terminal_commands_roundtrip_without_a_shell_surface() {
    let session_id = TerminalSessionId::from_uuid(Uuid::from_u128(800));
    let provider: TerminalProvider = Arc::new(move |command| {
        Box::pin(async move {
            Ok(match command {
                TerminalCommand::Write { data, .. } => TerminalResult::Wrote {
                    session_id,
                    accepted_bytes: u32::try_from(data.decode().expect("validated data").len())
                        .expect("bounded length"),
                },
                _ => unreachable!("fixture command"),
            })
        })
    });
    let config = ServerConfig::new("fixture", Arc::new(runtime)).with_terminal_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;
    let command = TerminalCommand::Write {
        session_id,
        data: TerminalData::from_bytes(b"printf is data, not an argv surface").expect("data"),
    };

    let result = request_terminal(&path, RequestEnvelope::terminal(command))
        .await
        .expect("terminal result");
    assert_eq!(
        result,
        TerminalResult::Wrote {
            session_id,
            accepted_bytes: 35,
        }
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn terminal_stream_pushes_bounded_data_and_a_terminal_status_in_order() {
    let session_id = TerminalSessionId::from_uuid(Uuid::from_u128(801));
    let reads = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let provider: TerminalProvider = Arc::new(move |command| {
        let reads = Arc::clone(&reads);
        let polls = Arc::clone(&polls);
        Box::pin(async move {
            Ok(match command {
                TerminalCommand::Read { .. } if reads.fetch_add(1, Ordering::SeqCst) == 0 => {
                    TerminalResult::Read {
                        session_id,
                        output: TerminalRead::Data(
                            TerminalData::from_bytes(b"streamed output").expect("data"),
                        ),
                    }
                }
                TerminalCommand::Read { .. } => TerminalResult::Read {
                    session_id,
                    output: TerminalRead::EndOfStream,
                },
                TerminalCommand::Poll { .. } => TerminalResult::Status {
                    session_id,
                    status: TerminalStatus {
                        state: if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                            TerminalState::Running
                        } else {
                            TerminalState::Exited { code: Some(0) }
                        },
                        transcript_retained_bytes: 15,
                        transcript_dropped_bytes: 0,
                    },
                },
                _ => unreachable!("stream fixture command"),
            })
        })
    });
    let config = ServerConfig::new("fixture", Arc::new(runtime)).with_terminal_provider(provider);
    let (_directory, path, shutdown_tx, server) = spawn_server(config).await;
    let mut events = Vec::new();

    request_terminal_stream(
        &path,
        RequestEnvelope::terminal(TerminalCommand::Stream {
            session_id,
            max_bytes: 1_024,
        }),
        |event| {
            events.push(event);
            Ok(())
        },
    )
    .await
    .expect("terminal stream");

    assert!(matches!(
        events.first(),
        Some(TerminalStreamEvent::Started { session_id: actual, .. }) if *actual == session_id
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        TerminalStreamEvent::Data { session_id: actual, data }
            if *actual == session_id && data.decode().expect("data") == b"streamed output"
    )));
    assert!(matches!(
        events.last(),
        Some(TerminalStreamEvent::Ended { session_id: actual, status })
            if *actual == session_id
                && matches!(status.state, TerminalState::Exited { code: Some(0) })
    ));

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
