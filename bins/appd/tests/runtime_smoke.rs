use localdesk_domain::{
    APPD_HEALTH_CAPABILITY, CapabilityAvailability, NoteDraftMeta, NoteMutationResult, NoteQuery,
    NoteStatus, NoteWriteIntent, NotesCommand, NotesOutput, REMOTE_SFTP_CAPABILITY,
    REMOTE_SMB_CAPABILITY, TELEMETRY_SCHEMA_VERSION, TRANSFERS_CAPABILITY,
    USAGE_FOREGROUND_CAPABILITY, UsagePeriod, UsageSummary, UsageSummaryQuery,
};
use localdesk_ipc::{
    RequestEnvelope, request_health, request_network_snapshot, request_notes,
    request_remote_capabilities, request_remote_profile, request_telemetry_snapshot,
    request_transfer, request_usage_summary,
};
use localdesk_remote_core::{
    Authentication, FirstUsePolicy, ProfileId, ProfileOptions, RemoteConnectionProfile,
    RemoteEndpoint, RemoteProfileCommand, RemoteProfilePageQuery, RemoteProfileResult,
    RemoteProtocol, TrustPolicy,
};
use localdesk_transfers::{TransferCommand, TransferOutput, TransferQuery};
use localdesk_usage::{ClockSource, SummaryBucket, SummaryKind, SystemClock};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tempfile::tempdir;

const SOCKET_READY_ATTEMPTS: usize = 250;
const EXIT_ATTEMPTS: usize = 250;
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[tokio::test]
async fn real_appd_process_serves_read_only_product_smoke_and_cleans_socket() {
    let runtime = tempdir().expect("temporary XDG runtime directory");
    let state = tempdir().expect("temporary XDG state directory");
    set_private_directory(runtime.path());
    set_private_directory(state.path());

    let socket_directory = runtime.path().join("localdesk");
    let socket_path = socket_directory.join("appd.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_localdesk-appd"))
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("XDG_STATE_HOME", state.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start real appd process");

    wait_for_socket(&mut child, &socket_path).await;
    assert_eq!(mode(&socket_directory), 0o700);
    assert_eq!(mode(&socket_path), 0o600);

    let health = request_health(
        &socket_path,
        RequestEnvelope::health(
            "appd-runtime-smoke",
            vec![APPD_HEALTH_CAPABILITY.to_owned()],
        ),
    )
    .await
    .expect("health response from real appd");
    assert_eq!(health.capabilities.len(), 1);
    assert_eq!(health.capabilities[0].id, APPD_HEALTH_CAPABILITY);
    assert_eq!(
        health.capabilities[0].status,
        CapabilityAvailability::Healthy
    );

    let product_health = request_health(
        &socket_path,
        RequestEnvelope::health(
            "appd-runtime-product-smoke",
            [
                REMOTE_SFTP_CAPABILITY,
                REMOTE_SMB_CAPABILITY,
                TRANSFERS_CAPABILITY,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
    )
    .await
    .expect("remote and transfer health from real appd");
    assert!(product_health.capabilities.iter().all(|capability| {
        capability.status == CapabilityAvailability::Healthy
            && matches!(
                capability.reason.as_str(),
                "remote_adapter_available" | "transfer_runner_active_public_commands_available"
            )
    }));

    let telemetry = request_telemetry_snapshot(&socket_path, RequestEnvelope::telemetry_snapshot())
        .await
        .expect("telemetry snapshot from real appd");
    assert_eq!(telemetry.schema_version, TELEMETRY_SCHEMA_VERSION);
    assert!(!telemetry.snapshot_id.is_nil());
    assert!(!telemetry.reason.is_empty());

    let network = request_network_snapshot(&socket_path, RequestEnvelope::network_snapshot())
        .await
        .expect("network snapshot from real appd");
    assert_eq!(network.validate(), Ok(()));
    if network.per_application.status == CapabilityAvailability::Unsupported {
        assert!(network.applications.is_empty());
    }

    let catalog = request_remote_capabilities(&socket_path, RequestEnvelope::remote_capabilities())
        .await
        .expect("remote catalog from real appd");
    assert_eq!(catalog.validate(), Ok(()));

    let profiles = request_remote_profile(
        &socket_path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::List {
            query: RemoteProfilePageQuery {
                after: None,
                limit: 1,
            },
        }),
    )
    .await
    .expect("empty profile list from real appd");
    let RemoteProfileResult::Page(page) = profiles else {
        panic!("expected profile page");
    };
    assert!(page.profiles.is_empty());
    assert_eq!(page.next_after, None);

    let transfers = request_transfer(
        &socket_path,
        RequestEnvelope::transfer(TransferCommand::List {
            query: TransferQuery {
                limit: 16,
                offset: 0,
                states: Vec::new(),
                direction: None,
                profile_id: None,
            },
        }),
    )
    .await
    .expect("empty transfer queue from real appd");
    let TransferOutput::Page {
        page: transfer_page,
    } = transfers
    else {
        panic!("expected transfer page");
    };
    assert!(transfer_page.tasks.is_empty());
    assert_eq!(transfer_page.next_offset, None);

    let notes = request_notes(
        &socket_path,
        RequestEnvelope::notes(NotesCommand::List {
            query: NoteQuery::default(),
        }),
    )
    .await
    .expect("empty notes page from real appd");
    let NotesOutput::Page(note_page) = notes else {
        panic!("expected notes page");
    };
    assert!(note_page.notes.is_empty());
    assert_eq!(note_page.next_offset, None);

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("signal appd");
    assert!(signal.success());
    let status = wait_for_exit(&mut child).await;
    assert!(status.success(), "appd exited unsuccessfully: {status}");
    assert!(!socket_path.exists(), "appd socket must be removed on exit");
}

#[tokio::test]
async fn real_appd_process_persists_profiles_and_notes_across_restart() {
    let runtime = tempdir().expect("temporary XDG runtime directory");
    let state = tempdir().expect("temporary XDG state directory");
    set_private_directory(runtime.path());
    set_private_directory(state.path());

    let socket_path = runtime.path().join("localdesk/appd.sock");
    let profile_id = ProfileId::from_uuid(uuid::Uuid::from_u128(200));
    let profile = RemoteConnectionProfile::new(
        profile_id,
        "persisted SSH agent",
        RemoteProtocol::Ssh,
        RemoteEndpoint::new("persisted.invalid", 22).expect("endpoint"),
        Some("operator".to_owned()),
        None,
        Authentication::SshAgent,
        TrustPolicy::SshKnownHosts {
            first_use: FirstUsePolicy::Reject,
        },
        ProfileOptions::Ssh {
            jump_profiles: Vec::new(),
            agent_forwarding: false,
        },
    )
    .expect("valid SSH profile");

    let mut first = spawn_appd(runtime.path(), state.path());
    wait_for_socket(&mut first, &socket_path).await;

    let stored_profile = request_remote_profile(
        &socket_path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::Upsert {
            profile: profile.clone(),
            expected_revision: None,
        }),
    )
    .await
    .expect("store profile through real appd");
    assert!(matches!(
        stored_profile,
        RemoteProfileResult::Stored(ref stored)
            if stored.revision == 0 && stored.profile == profile
    ));

    let stored_note = request_notes(
        &socket_path,
        RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Create,
            meta: NoteDraftMeta {
                title: "重启持久化".to_owned(),
                diary_date: Some("2026-08-13".to_owned()),
                tags: vec!["runtime-smoke".to_owned()],
                status: NoteStatus::Active,
                pinned: true,
            },
            body_markdown: "真实 appd 重启后仍可读取。".to_owned(),
        }),
    )
    .await
    .expect("store note through real appd");
    let NotesOutput::Mutation(NoteMutationResult::Stored(stored_note)) = stored_note else {
        panic!("expected stored note");
    };

    terminate_appd(&mut first, &socket_path).await;

    let mut second = spawn_appd(runtime.path(), state.path());
    wait_for_socket(&mut second, &socket_path).await;

    let profiles = request_remote_profile(
        &socket_path,
        RequestEnvelope::remote_profile(RemoteProfileCommand::List {
            query: RemoteProfilePageQuery {
                after: None,
                limit: 16,
            },
        }),
    )
    .await
    .expect("read persisted profile after restart");
    assert!(matches!(
        profiles,
        RemoteProfileResult::Page(ref page)
            if page.profiles.len() == 1 && page.profiles[0].profile == profile
    ));

    let note = request_notes(
        &socket_path,
        RequestEnvelope::notes(NotesCommand::Get {
            id: stored_note.id.clone(),
        }),
    )
    .await
    .expect("read persisted note after restart");
    assert!(matches!(
        note,
        NotesOutput::Document(ref document)
            if document.summary == stored_note
                && document.body_markdown == "真实 appd 重启后仍可读取。"
    ));

    terminate_appd(&mut second, &socket_path).await;
}

#[tokio::test]
async fn usage_native_workers_are_joined_when_idle_source_is_unavailable() {
    let runtime = tempdir().expect("temporary XDG runtime directory");
    let state = tempdir().expect("temporary XDG state directory");
    let commands = tempdir().expect("temporary command directory");
    set_private_directory(runtime.path());
    set_private_directory(state.path());
    set_private_directory(commands.path());
    write_executable(
        &commands.path().join("niri"),
        "#!/bin/sh\nprintf '%s\\n' '{\"WindowsChanged\":{\"windows\":[{\"id\":1,\"app_id\":\"terminal\",\"pid\":42,\"is_focused\":true}]}}'\nexec sleep 3600\n",
    );
    write_executable(
        &commands.path().join("loginctl"),
        "#!/bin/sh\nprintf '%s\\n' 'Active=yes' 'LockedHint=no'\n",
    );
    write_executable(
        &commands.path().join("gdbus"),
        "#!/bin/sh\nexec sleep 3600\n",
    );

    let socket_path = runtime.path().join("localdesk/appd.sock");
    let inherited_path = std::env::var("PATH").expect("PATH");
    let mut child = Command::new(env!("CARGO_BIN_EXE_localdesk-appd"))
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_SESSION_ID", "1")
        .env(
            "PATH",
            format!("{}:{inherited_path}", commands.path().display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start real appd process with fake usage sources");

    wait_for_socket(&mut child, &socket_path).await;
    wait_for_usage_state(
        &mut child,
        &socket_path,
        CapabilityAvailability::Degraded,
        "wayland_idle_event_stream_unavailable",
    )
    .await;

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("signal appd");
    assert!(signal.success());
    let status = wait_for_exit(&mut child).await;
    assert!(status.success(), "appd exited unsuccessfully: {status}");
    assert!(!socket_path.exists(), "appd socket must be removed on exit");
}

fn total_usage_ns(summary: &UsageSummary) -> u64 {
    summary
        .applications
        .iter()
        .map(|application| application.duration_ns)
        .sum()
}

async fn assert_foreground_time_tracks_checkpoints(socket_path: &std::path::Path) {
    let mut clock = SystemClock;
    let sample = clock.sample().expect("sample current local bucket");
    let bucket =
        SummaryBucket::for_sample(SummaryKind::Daily, &sample).expect("current daily bucket");
    let query = UsageSummaryQuery {
        period: UsagePeriod::Daily,
        bucket_key: bucket.bucket_key,
    };
    let first = request_usage_summary(socket_path, RequestEnvelope::usage_summary(query.clone()))
        .await
        .expect("initial usage summary");
    let first_checkpoint = first
        .coverage
        .last_checkpoint_unix_ms
        .expect("initial checkpoint");
    let first_total = total_usage_ns(&first);

    tokio::time::sleep(Duration::from_millis(750)).await;
    let request_started = tokio::time::Instant::now();
    let second = request_usage_summary(socket_path, RequestEnvelope::usage_summary(query))
        .await
        .expect("query-driven usage checkpoint");
    assert!(
        request_started.elapsed() < Duration::from_secs(2),
        "current-bucket query waited for the periodic checkpoint"
    );
    let second_checkpoint = second
        .coverage
        .last_checkpoint_unix_ms
        .expect("updated checkpoint");
    assert!(
        second_checkpoint > first_checkpoint,
        "current-bucket query did not checkpoint foreground time"
    );
    let usage_delta_ns = total_usage_ns(&second)
        .checked_sub(first_total)
        .expect("usage total must not regress");
    let checkpoint_delta_ns = u64::try_from(second_checkpoint - first_checkpoint)
        .expect("checkpoint must advance")
        * 1_000_000;
    assert!(
        usage_delta_ns.abs_diff(checkpoint_delta_ns) <= 1_000_000_000,
        "foreground delta {usage_delta_ns}ns differs from checkpoint delta {checkpoint_delta_ns}ns"
    );
}

#[tokio::test]
#[ignore = "requires the current niri Wayland and logind session"]
async fn live_wayland_usage_reaches_healthy_with_isolated_state() {
    let runtime = tempdir().expect("temporary XDG runtime directory");
    let state = tempdir().expect("temporary XDG state directory");
    set_private_directory(runtime.path());
    set_private_directory(state.path());

    let display = std::env::var("WAYLAND_DISPLAY").expect("WAYLAND_DISPLAY");
    let real_runtime = std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    std::os::unix::fs::symlink(
        std::path::Path::new(&real_runtime).join(&display),
        runtime.path().join(&display),
    )
    .expect("link current Wayland socket into isolated runtime");

    let socket_path = runtime.path().join("localdesk/appd.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_localdesk-appd"))
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("XDG_STATE_HOME", state.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start isolated appd on current Wayland session");

    wait_for_socket(&mut child, &socket_path).await;
    wait_for_usage_state(
        &mut child,
        &socket_path,
        CapabilityAvailability::Healthy,
        "usage_tracking_active",
    )
    .await;

    assert_foreground_time_tracks_checkpoints(&socket_path).await;

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("signal isolated appd");
    assert!(signal.success());
    let status = wait_for_exit(&mut child).await;
    assert!(status.success(), "appd exited unsuccessfully: {status}");
    assert!(!socket_path.exists(), "appd socket must be removed on exit");
}

fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("write fake command");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("make command executable");
}

fn set_private_directory(path: &std::path::Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("private temporary directory");
}

fn spawn_appd(runtime_path: &std::path::Path, state_path: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_localdesk-appd"))
        .env("XDG_RUNTIME_DIR", runtime_path)
        .env("XDG_STATE_HOME", state_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start real appd process")
}

async fn terminate_appd(child: &mut Child, socket_path: &std::path::Path) {
    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("signal appd");
    assert!(signal.success());
    let status = wait_for_exit(child).await;
    assert!(status.success(), "appd exited unsuccessfully: {status}");
    assert!(!socket_path.exists(), "appd socket must be removed on exit");
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("path metadata")
        .permissions()
        .mode()
        & 0o777
}

async fn wait_for_socket(child: &mut Child, socket_path: &std::path::Path) {
    for _ in 0..SOCKET_READY_ATTEMPTS {
        assert!(
            child.try_wait().expect("poll appd").is_none(),
            "appd exited before creating its socket"
        );
        if socket_path.exists() {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("appd socket was not created before the deadline");
}

async fn wait_for_usage_state(
    child: &mut Child,
    socket_path: &std::path::Path,
    expected_status: CapabilityAvailability,
    expected_reason: &str,
) {
    for _ in 0..SOCKET_READY_ATTEMPTS {
        assert!(
            child.try_wait().expect("poll appd").is_none(),
            "appd exited before usage reached the expected state"
        );
        let health = request_health(
            socket_path,
            RequestEnvelope::health(
                "appd-usage-runtime-smoke",
                vec![USAGE_FOREGROUND_CAPABILITY.to_owned()],
            ),
        )
        .await
        .expect("usage health response");
        if health.capabilities[0].status == expected_status
            && health.capabilities[0].reason == expected_reason
        {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("usage did not reach {expected_status:?}/{expected_reason} before the deadline");
}

async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    for _ in 0..EXIT_ATTEMPTS {
        if let Some(status) = child.try_wait().expect("poll appd exit") {
            return status;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let _ = child.kill();
    let status = child.wait().expect("reap timed-out appd");
    panic!("appd did not exit before the deadline; forced status: {status}");
}
