use crate::{TransferId, TransferStateKind, TransferTask};
use rusqlite::{
    Connection, Error as SqliteError, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior,
    params,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

pub const SQLITE_TRANSFER_SCHEMA_VERSION: u32 = 1;
pub const MAX_TRANSFER_DOCUMENT_BYTES: usize = 256 * 1024;
pub const SQLITE_BEGIN_WRITE: &str = "BEGIN IMMEDIATE";
pub const SQLITE_COMPARE_AND_SWAP_TASK: &str = "UPDATE transfer_tasks SET revision = ?1, state = ?2, document = ?3, updated_at_unix_ms = ?4 WHERE task_id = ?5 AND revision = ?6";
pub const SQLITE_SOLE_WRITER_RULES: &[&str] = &[
    "appd owns the only writable SQLite connection",
    "all mutations run inside BEGIN IMMEDIATE transactions",
    "task replacement uses revision compare-and-swap",
    "migration failure preserves the prior database without silent rebuild",
    "running, pausing, and cancelling tasks recover as unverified failure",
    "profiles and task documents contain references and handles, never secret values",
];

pub const SQLITE_TRANSFER_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS transfer_tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state TEXT NOT NULL,
    document BLOB NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS transfer_tasks_state_updated_idx
    ON transfer_tasks(state, updated_at_unix_ms);
PRAGMA user_version = 1;
"#;

pub trait TransferStore: Send {
    fn load_all(&self) -> Result<Vec<TransferTask>, StoreError>;

    fn insert(&mut self, task: &TransferTask) -> Result<(), StoreError>;

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        task: &TransferTask,
    ) -> Result<(), StoreError>;
}

pub struct SqliteTransferStore {
    connection: Connection,
}

impl std::fmt::Debug for SqliteTransferStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteTransferStore")
            .finish_non_exhaustive()
    }
}

impl SqliteTransferStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let connection = Connection::open_with_flags(path, flags).map_err(map_sqlite_error)?;
        Self::from_connection(connection)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory().map_err(map_sqlite_error)?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, StoreError> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(map_sqlite_error)?;
        ensure_database_integrity(&connection)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if version > SQLITE_TRANSFER_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema);
        }
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nPRAGMA synchronous = FULL;",
            )
            .map_err(map_sqlite_error)?;
        match version {
            0 => {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(map_sqlite_error)?;
                transaction
                    .execute_batch(SQLITE_TRANSFER_SCHEMA_V1)
                    .map_err(map_sqlite_error)?;
                transaction.commit().map_err(map_sqlite_error)?;
            }
            SQLITE_TRANSFER_SCHEMA_VERSION => {}
            _ => return Err(StoreError::UnsupportedSchema),
        }
        Ok(Self { connection })
    }

    fn encode(task: &TransferTask) -> Result<Vec<u8>, StoreError> {
        let document = serde_json::to_vec(task).map_err(|_| StoreError::Corrupt)?;
        if document.len() > MAX_TRANSFER_DOCUMENT_BYTES {
            return Err(StoreError::DocumentTooLarge);
        }
        Ok(document)
    }

    fn decode_row(
        task_id: String,
        revision: i64,
        state: String,
        document: Vec<u8>,
        updated_at_unix_ms: i64,
    ) -> Result<TransferTask, StoreError> {
        if document.len() > MAX_TRANSFER_DOCUMENT_BYTES || revision < 0 {
            return Err(StoreError::Corrupt);
        }
        let task: TransferTask =
            serde_json::from_slice(&document).map_err(|_| StoreError::Corrupt)?;
        let revision = u64::try_from(revision).map_err(|_| StoreError::Corrupt)?;
        if task.id.as_uuid().to_string() != task_id
            || task.revision != revision
            || state_code(task.state.kind()) != state
            || task.updated_at_unix_ms != updated_at_unix_ms
        {
            return Err(StoreError::Corrupt);
        }
        Ok(task)
    }
}

fn ensure_database_integrity(connection: &Connection) -> Result<(), StoreError> {
    let result = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(map_sqlite_error)?;
    if result != "ok" {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

impl TransferStore for SqliteTransferStore {
    fn load_all(&self) -> Result<Vec<TransferTask>, StoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT task_id, revision, state, document, updated_at_unix_ms \
                 FROM transfer_tasks ORDER BY updated_at_unix_ms ASC, task_id ASC",
            )
            .map_err(map_sqlite_error)?;
        let mut rows = statement.query([]).map_err(map_sqlite_error)?;
        let mut tasks = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_error)? {
            tasks.push(Self::decode_row(
                row.get(0).map_err(map_sqlite_error)?,
                row.get(1).map_err(map_sqlite_error)?,
                row.get(2).map_err(map_sqlite_error)?,
                row.get(3).map_err(map_sqlite_error)?,
                row.get(4).map_err(map_sqlite_error)?,
            )?);
        }
        Ok(tasks)
    }

    fn insert(&mut self, task: &TransferTask) -> Result<(), StoreError> {
        let document = Self::encode(task)?;
        let revision = i64::try_from(task.revision).map_err(|_| StoreError::Corrupt)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO transfer_tasks \
                 (task_id, revision, state, document, updated_at_unix_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    task.id.as_uuid().to_string(),
                    revision,
                    state_code(task.state.kind()),
                    document,
                    task.updated_at_unix_ms,
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    StoreError::AlreadyExists
                } else {
                    map_sqlite_error(error)
                }
            })?;
        transaction.commit().map_err(map_sqlite_error)
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        task: &TransferTask,
    ) -> Result<(), StoreError> {
        let document = Self::encode(task)?;
        let expected_revision =
            i64::try_from(expected_revision).map_err(|_| StoreError::Corrupt)?;
        let revision = i64::try_from(task.revision).map_err(|_| StoreError::Corrupt)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;
        let changed = transaction
            .execute(
                SQLITE_COMPARE_AND_SWAP_TASK,
                params![
                    revision,
                    state_code(task.state.kind()),
                    document,
                    task.updated_at_unix_ms,
                    task.id.as_uuid().to_string(),
                    expected_revision,
                ],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            let current: Option<i64> = transaction
                .query_row(
                    "SELECT revision FROM transfer_tasks WHERE task_id = ?1",
                    [task.id.as_uuid().to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_sqlite_error)?;
            return Err(if current.is_some() {
                StoreError::RevisionConflict
            } else {
                StoreError::NotFound
            });
        }
        transaction.commit().map_err(map_sqlite_error)
    }
}

fn state_code(state: TransferStateKind) -> &'static str {
    match state {
        TransferStateKind::Queued => "queued",
        TransferStateKind::Running => "running",
        TransferStateKind::Pausing => "pausing",
        TransferStateKind::Paused => "paused",
        TransferStateKind::Cancelling => "cancelling",
        TransferStateKind::RetryScheduled => "retry_scheduled",
        TransferStateKind::Conflict => "conflict",
        TransferStateKind::Completed => "completed",
        TransferStateKind::Failed => "failed",
        TransferStateKind::Cancelled => "cancelled",
    }
}

fn is_constraint_violation(error: &SqliteError) -> bool {
    matches!(
        error,
        SqliteError::SqliteFailure(failure, _)
            if failure.code == ErrorCode::ConstraintViolation
    )
}

fn map_sqlite_error(error: SqliteError) -> StoreError {
    match error {
        SqliteError::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::SchemaChanged
            ) =>
        {
            StoreError::Corrupt
        }
        _ => StoreError::Unavailable,
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum StoreError {
    #[error("transfer task already exists")]
    AlreadyExists,
    #[error("transfer task was not found")]
    NotFound,
    #[error("transfer task revision conflict")]
    RevisionConflict,
    #[error("persistence is unavailable")]
    Unavailable,
    #[error("persisted transfer data is corrupt")]
    Corrupt,
    #[error("persisted transfer schema is unsupported")]
    UnsupportedSchema,
    #[error("persisted transfer task exceeds the document hard limit")]
    DocumentTooLarge,
}

#[derive(Debug, Default)]
pub struct InMemoryTransferStore {
    tasks: BTreeMap<TransferId, TransferTask>,
}

impl InMemoryTransferStore {
    pub fn with_tasks(tasks: impl IntoIterator<Item = TransferTask>) -> Self {
        Self {
            tasks: tasks.into_iter().map(|task| (task.id, task)).collect(),
        }
    }

    pub fn task(&self, id: TransferId) -> Option<&TransferTask> {
        self.tasks.get(&id)
    }
}

impl TransferStore for InMemoryTransferStore {
    fn load_all(&self) -> Result<Vec<TransferTask>, StoreError> {
        Ok(self.tasks.values().cloned().collect())
    }

    fn insert(&mut self, task: &TransferTask) -> Result<(), StoreError> {
        if self.tasks.contains_key(&task.id) {
            return Err(StoreError::AlreadyExists);
        }
        self.tasks.insert(task.id, task.clone());
        Ok(())
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        task: &TransferTask,
    ) -> Result<(), StoreError> {
        let current = self.tasks.get(&task.id).ok_or(StoreError::NotFound)?;
        if current.revision != expected_revision {
            return Err(StoreError::RevisionConflict);
        }
        self.tasks.insert(task.id, task.clone());
        Ok(())
    }
}
