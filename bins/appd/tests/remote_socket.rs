#[allow(dead_code)]
#[path = "../src/network.rs"]
mod network;
#[allow(dead_code)]
#[path = "../src/notes.rs"]
mod notes;
#[allow(dead_code)]
#[path = "../src/remote.rs"]
mod remote;
#[path = "../src/service.rs"]
mod service;
#[allow(dead_code)]
#[path = "../src/usage.rs"]
mod usage;

use localdesk_domain::{
    REMOTE_FTP_CAPABILITY, REMOTE_SFTP_CAPABILITY, REMOTE_SMB_CAPABILITY, REMOTE_SSH_CAPABILITY,
    TRANSFERS_CAPABILITY,
};
use localdesk_ipc::{
    ClientError, RequestEnvelope, request_health, request_remote_capabilities,
    request_remote_profile, request_remote_session, request_terminal,
};
use localdesk_network::NetworkMonitor;
use localdesk_remote_core::{
    AdapterAvailability, Authentication, CapabilityStatus, FileOperation, FirstUsePolicy,
    ProfileId, ProfileOptions, RemoteConnectionProfile, RemoteEndpoint, RemoteProfileCommand,
    RemoteProfilePageQuery, RemoteProfileResult, RemoteProtocol, RemoteSessionCommand, SmbDialect,
    TerminalCommand, TerminalSize, TrustPolicy,
};
use localdesk_remote_smb::{
    CapabilityReport as SmbCapabilityReport, CapabilityStatus as SmbCapabilityStatus,
    OutputContract, ReauthenticationMode, SmbRemoteFileAdapter,
};
use localdesk_telemetry::TelemetryManager;
use network::NetworkSupervisor;
use remote::{MAX_REMOTE_SESSIONS, RemoteRuntime};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch};
use usage::UsageHandle;

#[tokio::test]
async fn system_adapter_catalog_and_transfer_store_are_exposed_without_remote_access() {
    let socket_directory = tempdir().expect("socket directory");
    let path = socket_directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let state_directory = tempdir().expect("state directory");
    std::fs::set_permissions(
        state_directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("state permissions");
    let remote = RemoteRuntime::from_state_base_for_test(state_directory.path());
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        remote,
        shutdown_rx,
    ));

    let catalog = request_remote_capabilities(&path, RequestEnvelope::remote_capabilities())
        .await
        .expect("remote catalog");
    assert_eq!(catalog.validate(), Ok(()));
    assert_eq!(catalog.adapters.len(), 5);

    let ssh = catalog
        .adapters
        .iter()
        .find(|adapter| adapter.protocol == RemoteProtocol::Ssh)
        .expect("ssh");
    assert!(matches!(ssh.terminal, CapabilityStatus::Supported));
    assert!(
        !ssh.file_operations
            .status(FileOperation::Read)
            .is_supported()
    );

    let plain_ftp = catalog
        .adapters
        .iter()
        .find(|adapter| adapter.protocol == RemoteProtocol::Ftp)
        .expect("ftp");
    assert!(matches!(
        plain_ftp.availability,
        AdapterAvailability::Degraded(_)
    ));
    assert!(matches!(
        &plain_ftp.availability,
        AdapterAvailability::Degraded(reason)
            if reason.as_str() == "plain_ftp_explicitly_enabled"
    ));
    assert!(
        plain_ftp
            .file_operations
            .status(FileOperation::List)
            .is_supported()
    );

    let sftp = catalog
        .adapters
        .iter()
        .find(|adapter| adapter.protocol == RemoteProtocol::Sftp)
        .expect("sftp");
    if matches!(sftp.availability, AdapterAvailability::Healthy) {
        assert_eq!(
            sftp.file_operations
                .iter()
                .filter(|operation| operation.status.is_supported())
                .count(),
            9
        );
        assert!(
            !sftp
                .file_operations
                .status(FileOperation::SetPermissions)
                .is_supported()
        );
    }

    let smb = catalog
        .adapters
        .iter()
        .find(|adapter| adapter.protocol == RemoteProtocol::Smb)
        .expect("smb");
    assert_eq!(smb.availability, AdapterAvailability::Healthy);
    assert_eq!(
        smb.file_operations
            .iter()
            .filter(|operation| operation.status.is_supported())
            .count(),
        10
    );
    for operation in [
        FileOperation::List,
        FileOperation::Stat,
        FileOperation::Read,
        FileOperation::Write,
        FileOperation::CreateDirectory,
        FileOperation::Rename,
        FileOperation::Delete,
        FileOperation::ResumeRead,
        FileOperation::ResumeWrite,
        FileOperation::AtomicRename,
    ] {
        assert!(smb.file_operations.status(operation).is_supported());
    }
    assert!(
        !smb.file_operations
            .status(FileOperation::SetPermissions)
            .is_supported()
    );

    let requested = [
        REMOTE_SSH_CAPABILITY,
        REMOTE_SFTP_CAPABILITY,
        REMOTE_FTP_CAPABILITY,
        REMOTE_SMB_CAPABILITY,
        TRANSFERS_CAPABILITY,
    ];
    let health = request_health(
        &path,
        RequestEnvelope::health(
            "test-client",
            requested.iter().map(|value| (*value).to_owned()).collect(),
        ),
    )
    .await
    .expect("health");
    assert_eq!(health.capabilities.len(), requested.len());
    assert!(health.capabilities.iter().all(|capability| {
        capability.reason != "sftp_session_registry_ipc_pending"
            && capability.reason != "ftps_session_registry_ipc_pending"
            && capability.reason != "sftp_permissions_not_implemented"
            && capability.reason != "smb_transfer_endpoint_unverified"
    }));
    for capability_id in [REMOTE_SFTP_CAPABILITY, REMOTE_SMB_CAPABILITY] {
        let capability = health
            .capabilities
            .iter()
            .find(|capability| capability.id == capability_id)
            .expect("remote file capability");
        assert_eq!(
            capability.status,
            localdesk_domain::CapabilityAvailability::Healthy
        );
        assert_eq!(capability.reason, "remote_adapter_available");
    }
    assert_eq!(
        health
            .capabilities
            .iter()
            .find(|capability| capability.id == TRANSFERS_CAPABILITY)
            .expect("transfers")
            .reason,
        "transfer_runner_not_started_public_commands_unavailable"
    );

    for file in ["known_hosts", "transfers.sqlite3", "remote.sqlite3"] {
        let metadata =
            std::fs::symlink_metadata(state_directory.path().join("localdesk").join(file))
                .expect("metadata");
        assert_eq!(metadata.uid(), nix::unistd::Uid::current().as_raw());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn profile_crud_uses_revision_cas_and_persists_no_secret_value() {
    let socket_directory = tempdir().expect("socket directory");
    let path = socket_directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let state_directory = tempdir().expect("state directory");
    std::fs::set_permissions(
        state_directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("state permissions");
    let remote = RemoteRuntime::from_state_base_for_test(state_directory.path());
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        remote,
        shutdown_rx,
    ));

    let profile_id = ProfileId::from_uuid(uuid::Uuid::from_u128(100));
    let mut profile = RemoteConnectionProfile::new(
        profile_id,
        "test sftp",
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
    .expect("profile");

    let created = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile: profile.clone(),
            expected_revision: None,
        }),
    )
    .await
    .expect("create");
    assert!(matches!(
        created,
        RemoteProfileResult::Stored(ref stored) if stored.revision == 0
    ));

    let page = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::List {
            query: RemoteProfilePageQuery {
                after: None,
                limit: 16,
            },
        }),
    )
    .await
    .expect("list");
    assert!(matches!(
        page,
        RemoteProfileResult::Page(ref page) if page.profiles.len() == 1
    ));

    profile.label = "renamed sftp".to_owned();
    let updated = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile: profile.clone(),
            expected_revision: Some(0),
        }),
    )
    .await
    .expect("update");
    assert!(matches!(
        updated,
        RemoteProfileResult::Stored(ref stored)
            if stored.revision == 1 && stored.profile.label == "renamed sftp"
    ));

    let stale = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile,
            expected_revision: Some(0),
        }),
    )
    .await;
    assert!(matches!(
        stale,
        Err(ClientError::Daemon(error)) if error.code == "remote_profile_conflict"
    ));

    let deleted = request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Delete {
            profile_id,
            expected_revision: 1,
        }),
    )
    .await
    .expect("delete");
    assert_eq!(deleted, RemoteProfileResult::Deleted { profile_id });

    let database = std::fs::read(state_directory.path().join("localdesk/remote.sqlite3"))
        .expect("database bytes");
    assert!(
        !database
            .windows(b"password_value".len())
            .any(|window| window == b"password_value")
    );

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn ssh_terminal_rejects_agent_forwarding_before_opening_a_network_connection() {
    let socket_directory = tempdir().expect("socket directory");
    let path = socket_directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let state_directory = tempdir().expect("state directory");
    std::fs::set_permissions(
        state_directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("state permissions");
    let remote = RemoteRuntime::from_state_base_for_test(state_directory.path());
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        remote,
        shutdown_rx,
    ));

    let profile_id = ProfileId::from_uuid(uuid::Uuid::from_u128(200));
    let profile = RemoteConnectionProfile::new(
        profile_id,
        "rejected terminal fixture",
        RemoteProtocol::Ssh,
        RemoteEndpoint::new("must-not-be-contacted.invalid", 22).expect("endpoint"),
        Some("operator".to_owned()),
        None,
        Authentication::SshAgent,
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: true,
        },
    )
    .expect("SSH profile");
    request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile,
            expected_revision: None,
        }),
    )
    .await
    .expect("stored profile");

    for _ in 0..=MAX_REMOTE_SESSIONS {
        let opened = request_terminal(
            &path,
            RequestEnvelope::terminal(TerminalCommand::Open {
                profile_id,
                size: TerminalSize {
                    rows: 24,
                    columns: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
                accept_new_host_key: false,
            }),
        )
        .await;
        assert!(matches!(
            opened,
            Err(ClientError::Daemon(error))
                if error.code == "remote_unsupported"
                    && error.reason == "ssh_agent_forwarding_forbidden"
                    && !error.retryable
        ));
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn smb_diagnostic_connect_is_rejected_without_remote_access_or_capacity_use() {
    let socket_directory = tempdir().expect("socket directory");
    let path = socket_directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let state_directory = tempdir().expect("state directory");
    std::fs::set_permissions(
        state_directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("state permissions");
    let report = SmbCapabilityReport {
        status: SmbCapabilityStatus::Healthy,
        reason: "fixture diagnostic available".to_owned(),
        client_version: Some("fixture smbclient".to_owned()),
        dialects: ["SMB2", "SMB3"],
        smb1_enabled: false,
        supports_workgroup_domain: true,
        supports_kerberos: true,
        supports_signing: true,
        supports_encryption: true,
        supports_share_browse_diagnostic: true,
        reauthentication: ReauthenticationMode::FreshProcess,
        output_contract: OutputContract::OpaqueHumanOutput,
    };
    let remote = RemoteRuntime::from_state_base_for_test(state_directory.path())
        .with_file_adapter_for_test(std::sync::Arc::new(SmbRemoteFileAdapter::from_report(
            "fixture-smbclient",
            report,
        )));
    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes::NotesHandle::unavailable_for_test(),
        remote,
        shutdown_rx,
    ));

    let profile_id = ProfileId::from_uuid(uuid::Uuid::from_u128(300));
    let profile = RemoteConnectionProfile::new(
        profile_id,
        "SMB diagnostic only",
        RemoteProtocol::Smb,
        RemoteEndpoint::new("not-contacted.invalid", 445).expect("endpoint"),
        None,
        Some("EXAMPLE.TEST".to_owned()),
        Authentication::Kerberos,
        TrustPolicy::SmbNegotiated,
        ProfileOptions::Smb {
            share: None,
            minimum_dialect: SmbDialect::Smb3,
            require_signing: true,
            require_encryption: true,
        },
    )
    .expect("SMB profile");
    request_remote_profile(
        &path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile,
            expected_revision: None,
        }),
    )
    .await
    .expect("stored profile");

    for _ in 0..=MAX_REMOTE_SESSIONS {
        assert!(matches!(
            request_remote_session(
                &path,
                RequestEnvelope::remote_session(RemoteSessionCommand::Connect { profile_id }),
            )
            .await,
            Err(ClientError::Daemon(error))
                if error.code == "remote_unsupported"
                    && error.reason == "remote_file_operations_unsupported"
                    && !error.retryable
        ));
    }

    shutdown_tx.send(true).expect("shutdown");
    server.await.expect("join").expect("serve");
}
