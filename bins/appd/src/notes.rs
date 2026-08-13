use localdesk_domain::{CapabilityRuntimeState, NotesCommand, NotesOutput};
use localdesk_notes::{NotesError, NotesRepository, NotesService, NotesServiceError};
use nix::unistd::Uid;
use std::{
    fs::{self, DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{oneshot, watch};

const COMMAND_CAPACITY: usize = 32;
const COMMAND_DEADLINE: Duration = Duration::from_secs(5);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(1);
const DATABASE_FILE: &str = "notes.sqlite3";
const STATE_DIRECTORY: &str = "localdesk";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NotesRuntimeError {
    pub code: &'static str,
    pub reason: &'static str,
    pub retryable: bool,
}

impl NotesRuntimeError {
    const fn new(code: &'static str, reason: &'static str, retryable: bool) -> Self {
        Self {
            code,
            reason,
            retryable,
        }
    }

    fn from_service(error: &NotesServiceError) -> Self {
        let code = match error {
            NotesServiceError::InvalidCommand(_) => "invalid_request",
            NotesServiceError::Repository(localdesk_notes::NotesError::NotFound { .. }) => {
                "note_not_found"
            }
            NotesServiceError::Repository(_) => "notes_storage_error",
            _ => "notes_command_failed",
        };
        Self::new(code, error.reason_code(), error.retryable())
    }
}

#[derive(Clone)]
pub struct NotesHandle {
    shared: Arc<RwLock<NotesRuntimeState>>,
    accepting_uploads: Arc<AtomicBool>,
}

struct NotesRuntimeState {
    capability: CapabilityRuntimeState,
    sender: Option<mpsc::SyncSender<WorkerCommand>>,
}

impl NotesHandle {
    #[cfg(test)]
    pub fn unavailable_for_test() -> Self {
        Self {
            shared: Arc::new(RwLock::new(NotesRuntimeState {
                capability: CapabilityRuntimeState::unreachable("notes_worker_unavailable"),
                sender: None,
            })),
            accepting_uploads: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn capability_state(&self) -> CapabilityRuntimeState {
        self.shared
            .read()
            .map(|state| state.capability.clone())
            .unwrap_or_else(|_| CapabilityRuntimeState::unreachable("notes_state_poisoned"))
    }

    pub async fn execute(&self, command: NotesCommand) -> Result<NotesOutput, NotesRuntimeError> {
        command
            .validate()
            .map_err(|reason| NotesRuntimeError::new("invalid_request", reason, false))?;
        if matches!(command, NotesCommand::UploadBegin { .. })
            && !self.accepting_uploads.load(Ordering::Acquire)
        {
            return Err(NotesRuntimeError::new(
                "notes_shutting_down",
                "note_uploads_closed",
                true,
            ));
        }
        let sender = self.sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .try_send(WorkerCommand::Execute(command, reply_tx))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => NotesRuntimeError::new(
                    "notes_capacity_exceeded",
                    "notes_command_queue_full",
                    true,
                ),
                mpsc::TrySendError::Disconnected(_) => NotesRuntimeError::new(
                    "notes_provider_unavailable",
                    "notes_worker_unavailable",
                    true,
                ),
            })?;
        tokio::time::timeout(COMMAND_DEADLINE, reply_rx)
            .await
            .map_err(|_| {
                NotesRuntimeError::new("notes_provider_timeout", "notes_command_timeout", true)
            })?
            .map_err(|_| {
                NotesRuntimeError::new(
                    "notes_provider_unavailable",
                    "notes_worker_unavailable",
                    true,
                )
            })?
    }

    pub async fn begin_shutdown(&self) {
        self.accepting_uploads.store(false, Ordering::Release);
        let Ok(sender) = self.sender() else {
            return;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if send_control(sender, WorkerCommand::BeginShutdown(reply_tx))
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(COMMAND_DEADLINE, reply_rx).await;
        }
    }

    fn sender(&self) -> Result<mpsc::SyncSender<WorkerCommand>, NotesRuntimeError> {
        self.shared
            .read()
            .map_err(|_| {
                NotesRuntimeError::new("notes_runtime_error", "notes_state_poisoned", false)
            })?
            .sender
            .clone()
            .ok_or_else(|| {
                NotesRuntimeError::new(
                    "notes_provider_unavailable",
                    "notes_worker_unavailable",
                    true,
                )
            })
    }
}

pub struct NotesSupervisor {
    shared: Arc<RwLock<NotesRuntimeState>>,
    accepting_uploads: Arc<AtomicBool>,
    database_path: Result<PathBuf, NotesRuntimeError>,
}

impl NotesSupervisor {
    pub fn from_environment() -> Self {
        Self::new(notes_database_path_from_environment())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn from_state_base_for_test(base: &Path) -> Self {
        Self::new(prepare_database_path(base, Uid::effective().as_raw()))
    }

    fn new(database_path: Result<PathBuf, NotesRuntimeError>) -> Self {
        Self {
            shared: Arc::new(RwLock::new(NotesRuntimeState {
                capability: CapabilityRuntimeState::degraded("notes_warming_up"),
                sender: None,
            })),
            accepting_uploads: Arc::new(AtomicBool::new(true)),
            database_path,
        }
    }

    pub fn handle(&self) -> NotesHandle {
        NotesHandle {
            shared: Arc::clone(&self.shared),
            accepting_uploads: Arc::clone(&self.accepting_uploads),
        }
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let database_path = match self.database_path {
            Ok(path) => path,
            Err(error) => {
                publish_capability(
                    &self.shared,
                    CapabilityRuntimeState::unreachable(error.reason),
                );
                wait_for_shutdown(&mut shutdown).await;
                return;
            }
        };
        let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        if let Ok(mut state) = self.shared.write() {
            state.sender = Some(sender.clone());
        }
        let worker_shared = Arc::clone(&self.shared);
        let worker = match thread::Builder::new()
            .name("localdesk-notes".to_owned())
            .spawn(move || notes_worker(database_path, receiver, &worker_shared))
        {
            Ok(worker) => worker,
            Err(_) => {
                publish_capability(
                    &self.shared,
                    CapabilityRuntimeState::unreachable("notes_worker_start_failed"),
                );
                clear_sender(&self.shared);
                wait_for_shutdown(&mut shutdown).await;
                return;
            }
        };

        wait_for_shutdown(&mut shutdown).await;
        self.accepting_uploads.store(false, Ordering::Release);
        let _ = send_control(sender, WorkerCommand::Shutdown).await;
        clear_sender(&self.shared);
        publish_capability(
            &self.shared,
            CapabilityRuntimeState::unreachable("notes_shutting_down"),
        );
        let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    }
}

enum WorkerCommand {
    Execute(
        NotesCommand,
        oneshot::Sender<Result<NotesOutput, NotesRuntimeError>>,
    ),
    BeginShutdown(oneshot::Sender<()>),
    Shutdown,
}

fn notes_worker(
    database_path: PathBuf,
    receiver: mpsc::Receiver<WorkerCommand>,
    shared: &Arc<RwLock<NotesRuntimeState>>,
) {
    let expected_uid = Uid::effective().as_raw();
    let repository = match NotesRepository::open(&database_path) {
        Ok(repository) => repository,
        Err(error) => {
            publish_capability(shared, notes_open_failure_capability(&error));
            return;
        }
    };
    if validate_runtime_files(&database_path, expected_uid).is_err() {
        publish_capability(
            shared,
            CapabilityRuntimeState::unreachable("notes_database_unsafe"),
        );
        return;
    }
    let mut service = NotesService::new(repository);
    publish_capability(shared, CapabilityRuntimeState::healthy("notes_ready"));

    loop {
        match receiver.recv_timeout(CLEANUP_INTERVAL) {
            Ok(WorkerCommand::Execute(command, reply)) => {
                let result = if validate_runtime_files(&database_path, expected_uid).is_err() {
                    Err(NotesRuntimeError::new(
                        "notes_state_unsafe",
                        "notes_database_unsafe",
                        false,
                    ))
                } else {
                    service
                        .execute(command, unix_time_ms(), Instant::now())
                        .map_err(|error| NotesRuntimeError::from_service(&error))
                };
                let _ = reply.send(result);
            }
            Ok(WorkerCommand::BeginShutdown(reply)) => {
                service.begin_shutdown();
                let _ = reply.send(());
            }
            Ok(WorkerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = service.shutdown();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                service.cleanup_expired(Instant::now());
            }
        }
    }
}

fn notes_open_failure_capability(error: &NotesError) -> CapabilityRuntimeState {
    match error {
        NotesError::UnsupportedSchema { .. } => {
            CapabilityRuntimeState::unsupported("notes_schema_unsupported")
        }
        NotesError::MigrationBackup { reason } => CapabilityRuntimeState::degraded(*reason),
        NotesError::CorruptData { .. } => {
            CapabilityRuntimeState::unreachable("notes_database_corrupt")
        }
        _ => CapabilityRuntimeState::unreachable("notes_database_unavailable"),
    }
}

async fn send_control(
    sender: mpsc::SyncSender<WorkerCommand>,
    command: WorkerCommand,
) -> Result<(), ()> {
    tokio::time::timeout(
        COMMAND_DEADLINE,
        tokio::task::spawn_blocking(move || sender.send(command).map_err(|_| ())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?
}

fn unix_time_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

fn publish_capability(shared: &Arc<RwLock<NotesRuntimeState>>, state: CapabilityRuntimeState) {
    if let Ok(mut runtime) = shared.write() {
        runtime.capability = state;
    }
}

fn clear_sender(shared: &Arc<RwLock<NotesRuntimeState>>) {
    if let Ok(mut runtime) = shared.write() {
        runtime.sender = None;
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

fn notes_database_path_from_environment() -> Result<PathBuf, NotesRuntimeError> {
    let base = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            NotesRuntimeError::new(
                "notes_state_unavailable",
                "home_directory_unavailable",
                false,
            )
        })?;
        PathBuf::from(home).join(".local/state")
    };
    prepare_database_path(&base, Uid::effective().as_raw())
}

fn prepare_database_path(base: &Path, expected_uid: u32) -> Result<PathBuf, NotesRuntimeError> {
    if !base.is_absolute() {
        return Err(NotesRuntimeError::new(
            "notes_state_unsafe",
            "notes_state_path_not_absolute",
            false,
        ));
    }
    validate_private_directory(base, expected_uid, "notes_state_directory_unsafe")?;
    let directory = base.join(STATE_DIRECTORY);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => validate_private_directory_metadata(
            &metadata,
            expected_uid,
            "notes_state_directory_unsafe",
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Err(create_error) = DirBuilder::new().mode(0o700).create(&directory)
                && create_error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(NotesRuntimeError::new(
                    "notes_state_unavailable",
                    "notes_state_directory_create_failed",
                    true,
                ));
            }
            validate_private_directory(&directory, expected_uid, "notes_state_directory_unsafe")?;
        }
        Err(_) => {
            return Err(NotesRuntimeError::new(
                "notes_state_unavailable",
                "notes_state_directory_metadata_failed",
                true,
            ));
        }
    }

    let database = directory.join(DATABASE_FILE);
    match fs::symlink_metadata(&database) {
        Ok(metadata) => {
            validate_private_file_metadata(&metadata, expected_uid, "notes_database_unsafe")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Err(create_error) = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&database)
                && create_error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(NotesRuntimeError::new(
                    "notes_state_unavailable",
                    "notes_database_create_failed",
                    true,
                ));
            }
            validate_private_file(&database, expected_uid, "notes_database_unsafe")?;
        }
        Err(_) => {
            return Err(NotesRuntimeError::new(
                "notes_state_unavailable",
                "notes_database_metadata_failed",
                true,
            ));
        }
    }
    Ok(database)
}

fn validate_runtime_files(database: &Path, expected_uid: u32) -> Result<(), NotesRuntimeError> {
    validate_private_file(database, expected_uid, "notes_database_unsafe")?;
    let database_name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            NotesRuntimeError::new("notes_state_unsafe", "notes_database_name_invalid", false)
        })?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = database.with_file_name(format!("{database_name}{suffix}"));
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => {
                validate_private_file_metadata(&metadata, expected_uid, "notes_sidecar_unsafe")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(NotesRuntimeError::new(
                    "notes_state_unavailable",
                    "notes_sidecar_metadata_failed",
                    true,
                ));
            }
        }
    }
    Ok(())
}

fn validate_private_directory(
    path: &Path,
    expected_uid: u32,
    reason: &'static str,
) -> Result<(), NotesRuntimeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| NotesRuntimeError::new("notes_state_unavailable", reason, true))?;
    validate_private_directory_metadata(&metadata, expected_uid, reason)
}

fn validate_private_directory_metadata(
    metadata: &fs::Metadata,
    expected_uid: u32,
    reason: &'static str,
) -> Result<(), NotesRuntimeError> {
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_dir()
        || metadata.uid() != expected_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(NotesRuntimeError::new("notes_state_unsafe", reason, false));
    }
    Ok(())
}

fn validate_private_file(
    path: &Path,
    expected_uid: u32,
    reason: &'static str,
) -> Result<(), NotesRuntimeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| NotesRuntimeError::new("notes_state_unavailable", reason, true))?;
    validate_private_file_metadata(&metadata, expected_uid, reason)
}

fn validate_private_file_metadata(
    metadata: &fs::Metadata,
    expected_uid: u32,
    reason: &'static str,
) -> Result<(), NotesRuntimeError> {
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.uid() != expected_uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(NotesRuntimeError::new("notes_state_unsafe", reason, false));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_domain::{NoteDraftMeta, NoteStatus, NoteWriteIntent};
    use std::os::unix::fs::symlink;

    #[test]
    fn state_path_creation_is_private_and_rejects_symlink_database() {
        let directory = tempfile::tempdir().expect("state base");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("base mode");
        let path = prepare_database_path(directory.path(), Uid::effective().as_raw())
            .expect("database path");
        assert_eq!(
            fs::symlink_metadata(path.parent().expect("parent"))
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(&path)
                .expect("database metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::remove_file(&path).expect("remove database");
        symlink("target", &path).expect("symlink");
        assert_eq!(
            prepare_database_path(directory.path(), Uid::effective().as_raw())
                .expect_err("unsafe database")
                .reason,
            "notes_database_unsafe"
        );
    }

    #[tokio::test]
    async fn begin_shutdown_rejects_new_uploads_without_rejecting_reads() {
        let shared = Arc::new(RwLock::new(NotesRuntimeState {
            capability: CapabilityRuntimeState::healthy("notes_ready"),
            sender: None,
        }));
        let accepting_uploads = Arc::new(AtomicBool::new(false));
        let handle = NotesHandle {
            shared,
            accepting_uploads,
        };
        let error = handle
            .execute(NotesCommand::UploadBegin {
                intent: NoteWriteIntent::Create,
                meta: NoteDraftMeta {
                    title: String::new(),
                    diary_date: None,
                    tags: Vec::new(),
                    status: NoteStatus::Draft,
                    pinned: false,
                },
                expected_total_bytes: 0,
                body_sha256: format!("{:064x}", 0),
            })
            .await
            .expect_err("uploads closed");
        assert_eq!(error.reason, "note_uploads_closed");
    }

    #[test]
    fn notes_open_errors_preserve_schema_backup_and_corruption_reasons() {
        let cases = [
            (
                NotesError::UnsupportedSchema {
                    found: 99,
                    supported: 2,
                },
                CapabilityRuntimeState::unsupported("notes_schema_unsupported"),
            ),
            (
                NotesError::MigrationBackup {
                    reason: "notes_migration_backup_invalid",
                },
                CapabilityRuntimeState::degraded("notes_migration_backup_invalid"),
            ),
            (
                NotesError::CorruptData {
                    field: "fixture",
                    value: "invalid".to_owned(),
                },
                CapabilityRuntimeState::unreachable("notes_database_corrupt"),
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(notes_open_failure_capability(&error), expected);
        }
    }
}
