mod network;
mod notes;
mod remote;
mod service;
mod socket;
mod task_join;
mod telemetry;
mod usage;

use localdesk_ipc::SHUTDOWN_GRACE;
use localdesk_network::NetworkMonitor;
use localdesk_telemetry::TelemetryManager;
use network::{NetworkHelperCollector, NetworkSupervisor};
use notes::{NotesHandle, NotesSupervisor};
use remote::RemoteRuntime;
use std::{path::Path, process::ExitCode, time::Duration};
use task_join::{abort_task, drain_task};
use telemetry::{ChildReaper, TelemetrySupervisor, store_config};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{Instant, timeout_at},
};
use usage::UsageSupervisor;

const CONTROL_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
const REMOTE_SHUTDOWN_RESERVE: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("localdesk_appd=info")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "localdesk-appd stopped with an error");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = socket::runtime_dir_from_env()?;
    let supervisor = TelemetrySupervisor::new(TelemetryManager::new(store_config()));
    let telemetry_handle = supervisor.handle();
    let child_reaper = supervisor.cleanup_handle();
    let network_supervisor =
        NetworkSupervisor::new(NetworkMonitor::default().with_per_app_collector(Box::new(
            NetworkHelperCollector::new(telemetry_handle.clone()),
        )));
    let network_handle = network_supervisor.handle();
    let usage_supervisor = UsageSupervisor::from_environment();
    let usage_handle = usage_supervisor.handle();
    let notes_supervisor = NotesSupervisor::from_environment();
    let notes_handle = notes_supervisor.handle();
    let remote_runtime = RemoteRuntime::from_environment();
    remote_runtime.start_transfer_runner().await;
    let bound = socket::bind_appd_socket(&runtime_dir).await?;
    let socket_path = bound.path.clone();

    let (telemetry_shutdown_tx, telemetry_shutdown_rx) = watch::channel(false);
    let (network_shutdown_tx, network_shutdown_rx) = watch::channel(false);
    let (usage_shutdown_tx, usage_shutdown_rx) = watch::channel(false);
    let (notes_shutdown_tx, notes_shutdown_rx) = watch::channel(false);
    let (ipc_shutdown_tx, ipc_shutdown_rx) = watch::channel(false);
    let (kill_ack_tx, kill_ack_rx) = oneshot::channel();
    let mut server_task = tokio::spawn(service::serve_appd(
        bound.listener,
        telemetry_handle,
        network_handle,
        usage_handle,
        notes_handle.clone(),
        remote_runtime.clone(),
        ipc_shutdown_rx,
    ));
    let mut telemetry_task = tokio::spawn(supervisor.run(telemetry_shutdown_rx, kill_ack_tx));
    let mut network_task = tokio::spawn(network_supervisor.run(network_shutdown_rx));
    let mut usage_task = tokio::spawn(usage_supervisor.run(usage_shutdown_rx));
    let mut notes_task = tokio::spawn(notes_supervisor.run(notes_shutdown_rx));

    let mut runtime_error = None;
    tokio::select! {
        signal = wait_for_shutdown_signal() => {
            if let Err(error) = signal {
                runtime_error = Some(error.to_string());
            }
        }
        result = &mut server_task => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => runtime_error = Some(error.to_string()),
                Err(error) => runtime_error = Some(error.to_string()),
            }
        }
        result = &mut telemetry_task => {
            if let Err(error) = result {
                runtime_error = Some(error.to_string());
            }
        }
        result = &mut network_task => {
            runtime_error = Some(match result {
                Ok(()) => "network supervisor stopped unexpectedly".to_owned(),
                Err(error) => error.to_string(),
            });
        }
    }

    let shutdown_result = shutdown_runtime_until(
        RuntimeHandles {
            server_task: &mut server_task,
            telemetry_task: &mut telemetry_task,
            network_task: &mut network_task,
            usage_task: &mut usage_task,
            notes_task: &mut notes_task,
            telemetry_shutdown_tx,
            network_shutdown_tx,
            usage_shutdown_tx,
            notes_shutdown_tx,
            notes_handle,
            remote_runtime,
            ipc_shutdown_tx,
            kill_ack_rx,
            child_reaper,
            socket_path: &socket_path,
        },
        Instant::now() + SHUTDOWN_GRACE,
    )
    .await;

    if let Err(error) = shutdown_result {
        runtime_error.get_or_insert(error);
    }
    if let Some(error) = runtime_error {
        return Err(error.into());
    }
    Ok(())
}

struct RuntimeHandles<'a> {
    server_task: &'a mut JoinHandle<Result<(), localdesk_ipc::ServerError>>,
    telemetry_task: &'a mut JoinHandle<()>,
    network_task: &'a mut JoinHandle<()>,
    usage_task: &'a mut JoinHandle<()>,
    notes_task: &'a mut JoinHandle<()>,
    telemetry_shutdown_tx: watch::Sender<bool>,
    network_shutdown_tx: watch::Sender<bool>,
    usage_shutdown_tx: watch::Sender<bool>,
    notes_shutdown_tx: watch::Sender<bool>,
    notes_handle: NotesHandle,
    remote_runtime: RemoteRuntime,
    ipc_shutdown_tx: watch::Sender<bool>,
    kill_ack_rx: oneshot::Receiver<()>,
    child_reaper: ChildReaper,
    socket_path: &'a Path,
}

async fn shutdown_runtime_until(
    handles: RuntimeHandles<'_>,
    deadline: Instant,
) -> Result<(), String> {
    let RuntimeHandles {
        server_task,
        telemetry_task,
        network_task,
        usage_task,
        notes_task,
        telemetry_shutdown_tx,
        network_shutdown_tx,
        usage_shutdown_tx,
        notes_shutdown_tx,
        notes_handle,
        remote_runtime,
        ipc_shutdown_tx,
        kill_ack_rx,
        child_reaper,
        socket_path,
    } = handles;
    let _ = telemetry_shutdown_tx.send(true);
    let _ = network_shutdown_tx.send(true);
    let _ = usage_shutdown_tx.send(true);
    let notes_deadline = (Instant::now() + CONTROL_SHUTDOWN_GRACE).min(deadline);
    let _ = timeout_at(notes_deadline, notes_handle.begin_shutdown()).await;
    let _ = ipc_shutdown_tx.send(true);
    let mut remote_cleanup_task = tokio::spawn(async move {
        remote_runtime.shutdown_sessions().await;
    });
    let server_deadline = deadline
        .checked_sub(REMOTE_SHUTDOWN_RESERVE)
        .filter(|reserved| *reserved > Instant::now())
        .unwrap_or(deadline);
    let server_drained = if server_task.is_finished() {
        true
    } else {
        timeout_at(server_deadline, &mut *server_task).await.is_ok()
    };
    let _ = notes_shutdown_tx.send(true);
    let kill_ack = timeout_at(deadline, kill_ack_rx).await;
    if kill_ack.is_err() {
        let _ = timeout_at(deadline, child_reaper.start_kill()).await;
    }
    let reaper_clone = child_reaper.clone();
    let mut reaper_task = tokio::spawn(async move { reaper_clone.wait().await });
    let drained = timeout_at(deadline, async {
        if !server_drained {
            drain_task(server_task).await;
        }
        drain_task(telemetry_task).await;
        drain_task(network_task).await;
        drain_task(usage_task).await;
        drain_task(notes_task).await;
        drain_task(&mut remote_cleanup_task).await;
        drain_task(&mut reaper_task).await;
    })
    .await;
    if drained.is_err() {
        abort_task(server_task);
        abort_task(telemetry_task);
        abort_task(network_task);
        abort_task(usage_task);
        abort_task(notes_task);
        abort_task(&remote_cleanup_task);
        abort_task(&reaper_task);
        drain_task(server_task).await;
        drain_task(telemetry_task).await;
        drain_task(network_task).await;
        drain_task(usage_task).await;
        drain_task(notes_task).await;
        drain_task(&mut remote_cleanup_task).await;
        drain_task(&mut reaper_task).await;
    }

    child_reaper.ensure_reaped_until(deadline).await;
    socket::remove_socket(socket_path).map_err(|error| error.to_string())
}

async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        result = wait_for_term() => result,
    }
}

async fn wait_for_term() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        signal.recv().await;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::{net::UnixListener, process::Command, sync::Notify};

    #[tokio::test]
    async fn remote_cleanup_starts_in_parallel_with_ipc_drain() {
        let directory = tempdir().expect("runtime directory");
        let socket_path = directory.path().join("appd.sock");
        let _listener = UnixListener::bind(&socket_path).expect("listener");
        let probe = Arc::new(Notify::new());
        let server_probe = Arc::clone(&probe);
        let mut server_task = tokio::spawn(async move {
            server_probe.notified().await;
            Ok::<(), localdesk_ipc::ServerError>(())
        });
        let mut telemetry_task = tokio::spawn(async {});
        let mut network_task = tokio::spawn(async {});
        let mut usage_task = tokio::spawn(async {});
        let mut notes_task = tokio::spawn(async {});
        let (telemetry_shutdown_tx, _telemetry_shutdown_rx) = watch::channel(false);
        let (network_shutdown_tx, _network_shutdown_rx) = watch::channel(false);
        let (usage_shutdown_tx, _usage_shutdown_rx) = watch::channel(false);
        let (notes_shutdown_tx, _notes_shutdown_rx) = watch::channel(false);
        let (ipc_shutdown_tx, _ipc_shutdown_rx) = watch::channel(false);
        let (kill_ack_tx, kill_ack_rx) = oneshot::channel();
        kill_ack_tx.send(()).expect("kill acknowledgement");
        let started = Instant::now();

        shutdown_runtime_until(
            RuntimeHandles {
                server_task: &mut server_task,
                telemetry_task: &mut telemetry_task,
                network_task: &mut network_task,
                usage_task: &mut usage_task,
                notes_task: &mut notes_task,
                telemetry_shutdown_tx,
                network_shutdown_tx,
                usage_shutdown_tx,
                notes_shutdown_tx,
                notes_handle: NotesHandle::unavailable_for_test(),
                remote_runtime: RemoteRuntime::unavailable_for_test("test_runtime_unavailable")
                    .with_shutdown_probe_for_test(probe),
                ipc_shutdown_tx,
                kill_ack_rx,
                child_reaper: ChildReaper::new(),
                socket_path: &socket_path,
            },
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("shutdown");

        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn shutdown_accepts_a_consumed_network_task_and_cleans_socket_last() {
        let directory = tempdir().expect("runtime directory");
        let socket_path = directory.path().join("appd.sock");
        let _listener = UnixListener::bind(&socket_path).expect("listener");
        let server_task = tokio::spawn(async {
            std::future::pending::<Result<(), localdesk_ipc::ServerError>>().await
        });
        let telemetry_task = tokio::spawn(async { std::future::pending::<()>().await });
        let network_task = tokio::spawn(async {});
        let usage_task = tokio::spawn(async { std::future::pending::<()>().await });
        let notes_task = tokio::spawn(async { std::future::pending::<()>().await });
        let (telemetry_shutdown_tx, _telemetry_shutdown_rx) = watch::channel(false);
        let (network_shutdown_tx, _network_shutdown_rx) = watch::channel(false);
        let (usage_shutdown_tx, _usage_shutdown_rx) = watch::channel(false);
        let (notes_shutdown_tx, _notes_shutdown_rx) = watch::channel(false);
        let (ipc_shutdown_tx, _ipc_shutdown_rx) = watch::channel(false);
        let (_kill_ack_tx, kill_ack_rx) = oneshot::channel();
        let child = Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("test child");
        let child_reaper = ChildReaper::new();
        child_reaper.install(child).await.expect("install child");
        let mut server_task = server_task;
        let mut telemetry_task = telemetry_task;
        let mut network_task = network_task;
        let mut usage_task = usage_task;
        let mut notes_task = notes_task;
        (&mut network_task)
            .await
            .expect("consume completed network task as run select does");
        let deadline = Instant::now() + Duration::from_millis(50);
        let started = Instant::now();

        shutdown_runtime_until(
            RuntimeHandles {
                server_task: &mut server_task,
                telemetry_task: &mut telemetry_task,
                network_task: &mut network_task,
                usage_task: &mut usage_task,
                notes_task: &mut notes_task,
                telemetry_shutdown_tx,
                network_shutdown_tx,
                usage_shutdown_tx,
                notes_shutdown_tx,
                notes_handle: NotesHandle::unavailable_for_test(),
                remote_runtime: RemoteRuntime::unavailable_for_test("test_runtime_unavailable"),
                ipc_shutdown_tx,
                kill_ack_rx,
                child_reaper,
                socket_path: &socket_path,
            },
            deadline,
        )
        .await
        .expect("shutdown");

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!socket_path.exists());
    }
}
