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
    CapabilityAvailability, NOTES_CAPABILITY, NoteDeletedFilter, NoteDocument, NoteDraftMeta,
    NoteExportFormat, NoteMutationResult, NotePage, NoteQuery, NoteSort, NoteStatus,
    NoteWriteIntent, NotesCommand, NotesOutput,
};
use localdesk_ipc::{ClientError, RequestEnvelope, request_health, request_notes};
use localdesk_network::NetworkMonitor;
use localdesk_telemetry::TelemetryManager;
use network::NetworkSupervisor;
use notes::{NotesHandle, NotesSupervisor};
use remote::RemoteRuntime;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;
use tokio::{net::UnixListener, sync::watch, time::Duration};
use usage::UsageHandle;

async fn wait_until_ready(handle: &NotesHandle) {
    for _ in 0..100 {
        if handle.capability_state().status == CapabilityAvailability::Healthy {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("notes worker did not become ready");
}

fn draft(title: &str) -> NoteDraftMeta {
    NoteDraftMeta {
        title: title.to_owned(),
        diary_date: Some("2026-08-09".to_owned()),
        tags: vec!["daily".to_owned()],
        status: NoteStatus::Active,
        pinned: false,
    }
}

#[tokio::test]
async fn notes_socket_persists_cas_data_and_drains_without_accepting_new_uploads() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("state mode");
    let socket_directory = tempdir().expect("socket directory");
    let path = socket_directory.path().join("appd.sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let notes = NotesSupervisor::from_state_base_for_test(state.path());
    let notes_handle = notes.handle();
    let (notes_shutdown_tx, notes_shutdown_rx) = watch::channel(false);
    let notes_task = tokio::spawn(notes.run(notes_shutdown_rx));
    wait_until_ready(&notes_handle).await;

    let telemetry = TelemetryManager::with_defaults();
    let network = NetworkSupervisor::new(NetworkMonitor::default());
    let (ipc_shutdown_tx, ipc_shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(service::serve_appd(
        listener,
        telemetry.handle(),
        network.handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        notes_handle.clone(),
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        ipc_shutdown_rx,
    ));

    let health = request_health(
        &path,
        RequestEnvelope::health("fixture", vec![NOTES_CAPABILITY.to_owned()]),
    )
    .await
    .expect("health");
    assert_eq!(
        health.capabilities[0].status,
        CapabilityAvailability::Healthy
    );
    assert_eq!(health.capabilities[0].reason, "notes_ready");

    let created = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Create,
            meta: draft("第一天"),
            body_markdown: "- [ ] 记录中文日记".to_owned(),
        }),
    )
    .await
    .expect("create");
    let NotesOutput::Mutation(NoteMutationResult::Stored(created)) = created else {
        panic!("stored result");
    };

    let updated = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Save {
                id: created.id.clone(),
                expected_revision: created.revision,
                autosave: true,
            },
            meta: draft("第一天（更新）"),
            body_markdown: "- [x] 记录中文日记".to_owned(),
        }),
    )
    .await
    .expect("update");
    let NotesOutput::Mutation(NoteMutationResult::Stored(updated)) = updated else {
        panic!("updated result");
    };
    assert_eq!(updated.revision, created.revision + 1);

    let conflict = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::WriteInline {
            intent: NoteWriteIntent::Save {
                id: created.id.clone(),
                expected_revision: created.revision,
                autosave: true,
            },
            meta: draft("本地草稿保留"),
            body_markdown: "不会覆盖服务端".to_owned(),
        }),
    )
    .await
    .expect("conflict");
    assert!(matches!(
        conflict,
        NotesOutput::Mutation(NoteMutationResult::Conflict {
            expected_revision,
            current,
        }) if expected_revision == created.revision && current.revision == updated.revision
    ));

    let diary_query = NoteQuery {
        diary_date_from: Some("2026-08-09".to_owned()),
        diary_date_to: Some("2026-08-09".to_owned()),
        sort: NoteSort::DiaryDateDesc,
        ..NoteQuery::default()
    };
    let diary = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::List {
            query: diary_query.clone(),
        }),
    )
    .await
    .expect("diary query");
    let NotesOutput::Page(NotePage { notes: diary, .. }) = diary else {
        panic!("diary page");
    };
    let list = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::List {
            query: NoteQuery::default(),
        }),
    )
    .await
    .expect("list query");
    let NotesOutput::Page(NotePage { notes: list, .. }) = list else {
        panic!("list page");
    };
    assert_eq!(diary.len(), 1);
    assert_eq!(list.len(), 1);
    assert_eq!(diary[0].id, updated.id);
    assert_eq!(list[0].id, updated.id);
    assert_eq!(diary[0].revision, list[0].revision);

    let deleted = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::Delete {
            id: updated.id.clone(),
            expected_revision: updated.revision,
        }),
    )
    .await
    .expect("delete");
    let NotesOutput::Mutation(NoteMutationResult::Deleted(deleted)) = deleted else {
        panic!("deleted result");
    };
    assert!(deleted.deleted_at_ms.is_some());

    let visible = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::List {
            query: NoteQuery::default(),
        }),
    )
    .await
    .expect("active list after delete");
    assert!(matches!(visible, NotesOutput::Page(NotePage { notes, .. }) if notes.is_empty()));
    let deleted_only = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::List {
            query: NoteQuery {
                deleted: NoteDeletedFilter::Only,
                ..NoteQuery::default()
            },
        }),
    )
    .await
    .expect("deleted-only list");
    assert!(matches!(
        deleted_only,
        NotesOutput::Page(NotePage { notes, .. })
            if notes.len() == 1 && notes[0].id == deleted.id
    ));

    let restored = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::Restore {
            id: deleted.id.clone(),
            expected_revision: deleted.revision,
        }),
    )
    .await
    .expect("restore");
    let NotesOutput::Mutation(NoteMutationResult::Restored(restored)) = restored else {
        panic!("restored result");
    };
    assert_eq!(restored.deleted_at_ms, None);

    for format in [NoteExportFormat::Markdown, NoteExportFormat::Json] {
        let exported = request_notes(
            &path,
            RequestEnvelope::notes(NotesCommand::Export {
                query: diary_query.clone(),
                format,
            }),
        )
        .await
        .expect("export");
        let NotesOutput::Export(exported) = exported else {
            panic!("export result");
        };
        assert_eq!(exported.format, format);
        assert_eq!(exported.content_bytes as usize, exported.content.len());
        assert!(exported.content.contains("第一天（更新）"));
        assert!(exported.content.contains("记录中文日记"));
    }

    notes_handle.begin_shutdown().await;
    let upload = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::UploadBegin {
            intent: NoteWriteIntent::Create,
            meta: draft("late upload"),
            expected_total_bytes: 0,
            body_sha256: "0".repeat(64),
        }),
    )
    .await;
    assert!(matches!(
        upload,
        Err(ClientError::Daemon(error)) if error.reason == "note_uploads_closed"
    ));

    let document = request_notes(
        &path,
        RequestEnvelope::notes(NotesCommand::Get {
            id: created.id.clone(),
        }),
    )
    .await
    .expect("read during drain");
    assert!(matches!(
        document,
        NotesOutput::Document(NoteDocument { summary, body_markdown })
            if summary.revision == restored.revision && body_markdown.contains("[x]")
    ));

    ipc_shutdown_tx.send(true).expect("IPC shutdown");
    server.await.expect("server join").expect("serve");
    notes_shutdown_tx.send(true).expect("notes shutdown");
    notes_task.await.expect("notes join");

    let database = state.path().join("localdesk/notes.sqlite3");
    assert_eq!(
        std::fs::symlink_metadata(&database)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let second_socket = socket_directory.path().join("appd-2.sock");
    let second_listener = UnixListener::bind(&second_socket).expect("second listener");
    let second_notes = NotesSupervisor::from_state_base_for_test(state.path());
    let second_handle = second_notes.handle();
    let (second_notes_shutdown_tx, second_notes_shutdown_rx) = watch::channel(false);
    let second_notes_task = tokio::spawn(second_notes.run(second_notes_shutdown_rx));
    wait_until_ready(&second_handle).await;
    let (second_ipc_shutdown_tx, second_ipc_shutdown_rx) = watch::channel(false);
    let second_server = tokio::spawn(service::serve_appd(
        second_listener,
        TelemetryManager::with_defaults().handle(),
        NetworkSupervisor::new(NetworkMonitor::default()).handle(),
        UsageHandle::unavailable_for_test("usage_fixture_unavailable"),
        second_handle,
        RemoteRuntime::unavailable_for_test("remote_fixture_unavailable"),
        second_ipc_shutdown_rx,
    ));
    let reopened = request_notes(
        &second_socket,
        RequestEnvelope::notes(NotesCommand::Get { id: created.id }),
    )
    .await
    .expect("reopened document");
    assert!(matches!(
        reopened,
        NotesOutput::Document(NoteDocument { summary, .. }) if summary.revision == restored.revision
    ));
    second_ipc_shutdown_tx
        .send(true)
        .expect("second IPC shutdown");
    second_server
        .await
        .expect("second server join")
        .expect("second serve");
    second_notes_shutdown_tx
        .send(true)
        .expect("second notes shutdown");
    second_notes_task.await.expect("second notes join");
}
