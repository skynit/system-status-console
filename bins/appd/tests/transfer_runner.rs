#[allow(dead_code)]
#[path = "../src/remote.rs"]
mod remote;

use localdesk_remote_core::{
    ProfileId, RemotePath, RemoteProtocol, SafeReason, unsupported_file_capabilities,
};
use localdesk_transfers::{
    BandwidthLimit, ConflictPolicy, LocalFileHandle, QueueLimits, RemoteTransferEndpoint,
    RetryPolicy, SqliteTransferStore, TransferDirection, TransferEndpoint, TransferFeatureSet,
    TransferId, TransferQueue, TransferState, TransferStore, TransferTask,
};
use remote::RemoteRuntime;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn reason(value: &'static str) -> SafeReason {
    SafeReason::new(value).expect("static reason")
}

fn transfer_task() -> TransferTask {
    let capabilities = unsupported_file_capabilities(reason("fixture_resume_unsupported"));
    TransferTask::new(
        TransferId::new(),
        TransferEndpoint::Local {
            handle: LocalFileHandle::new(),
        },
        TransferEndpoint::Remote(RemoteTransferEndpoint {
            profile_id: ProfileId::new(),
            protocol: RemoteProtocol::Sftp,
            path: RemotePath::new("/fixture.bin").expect("path"),
        }),
        TransferDirection::Upload,
        None,
        None,
        RetryPolicy::default(),
        BandwidthLimit::unlimited(),
        ConflictPolicy::Fail,
        TransferFeatureSet::from_adapter(
            TransferDirection::Upload,
            RemoteProtocol::Sftp,
            &capabilities,
        ),
        1,
    )
    .expect("task")
}

#[tokio::test]
async fn runner_recovers_sqlite_active_state_and_reports_non_product_health() {
    let state = tempdir().expect("state directory");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    let runtime = RemoteRuntime::from_state_base_for_test(state.path());
    assert_eq!(
        runtime.capability_states().4.reason,
        "transfer_runner_not_started_public_commands_unavailable"
    );
    let database = state.path().join("localdesk/transfers.sqlite3");
    let store = SqliteTransferStore::open(&database).expect("store");
    let mut queue =
        TransferQueue::open(store, QueueLimits::new(32, 4, 2).expect("limits"), 1).expect("queue");
    let task = transfer_task();
    let id = task.id;
    queue.enqueue(task).expect("enqueue");
    queue.start_next(2).expect("start").expect("run token");
    drop(queue.into_store());

    runtime.start_transfer_runner().await;
    assert_eq!(
        runtime.capability_states().4.reason,
        "transfer_runner_active_provider_not_wired"
    );
    runtime.shutdown_sessions().await;
    assert_eq!(
        runtime.capability_states().4.reason,
        "transfer_runner_stopped_public_commands_unavailable"
    );

    let store = SqliteTransferStore::open(&database).expect("reopen store");
    let recovered = store
        .load_all()
        .expect("load")
        .into_iter()
        .find(|task| task.id == id)
        .expect("task");
    let TransferState::Failed { failure } = recovered.state else {
        panic!("active task must recover as failed")
    };
    assert_eq!(failure.reason.as_str(), "app_restarted_state_unverified");
}
