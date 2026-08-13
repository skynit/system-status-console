use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::repository::NotesError;

pub const CURRENT_SCHEMA_VERSION: u32 = 3;
pub const MIGRATION_BACKUP_SUFFIX: &str = ".bak";

const MIGRATION_1: &str = r#"
CREATE TABLE notes (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    diary_date TEXT,
    status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'archived')),
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    deleted_at_ms INTEGER,
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;

CREATE TABLE note_revisions (
    note_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    title TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    diary_date TEXT,
    tags_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'archived')),
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    deleted_at_ms INTEGER,
    recorded_at_ms INTEGER NOT NULL,
    PRIMARY KEY (note_id, revision),
    FOREIGN KEY (note_id) REFERENCES notes(id) ON DELETE CASCADE
) STRICT;

CREATE TABLE note_tags (
    note_id TEXT NOT NULL,
    tag_key TEXT NOT NULL,
    display_tag TEXT NOT NULL,
    PRIMARY KEY (note_id, tag_key),
    FOREIGN KEY (note_id) REFERENCES notes(id) ON DELETE CASCADE
) STRICT;
"#;

const MIGRATION_2: &str = r#"
CREATE INDEX notes_updated_idx ON notes (updated_at_ms DESC, id ASC);
CREATE INDEX notes_created_idx ON notes (created_at_ms DESC, id ASC);
CREATE INDEX notes_diary_idx ON notes (diary_date DESC, updated_at_ms DESC, id ASC);
CREATE INDEX notes_status_idx ON notes (status, deleted_at_ms, updated_at_ms DESC);
CREATE INDEX note_tags_key_idx ON note_tags (tag_key, note_id);
CREATE INDEX note_revisions_retention_idx
    ON note_revisions (note_id, recorded_at_ms DESC, revision DESC);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE notes_v3 (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    diary_date TEXT,
    status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'completed', 'archived')),
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    deleted_at_ms INTEGER,
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;

CREATE TABLE note_revisions_v3 (
    note_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    title TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    diary_date TEXT,
    tags_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'completed', 'archived')),
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    deleted_at_ms INTEGER,
    recorded_at_ms INTEGER NOT NULL,
    PRIMARY KEY (note_id, revision),
    FOREIGN KEY (note_id) REFERENCES notes_v3(id) ON DELETE CASCADE
) STRICT;

CREATE TABLE note_tags_v3 (
    note_id TEXT NOT NULL,
    tag_key TEXT NOT NULL,
    display_tag TEXT NOT NULL,
    PRIMARY KEY (note_id, tag_key),
    FOREIGN KEY (note_id) REFERENCES notes_v3(id) ON DELETE CASCADE
) STRICT;

INSERT INTO notes_v3 SELECT * FROM notes;
INSERT INTO note_revisions_v3 SELECT * FROM note_revisions;
INSERT INTO note_tags_v3 SELECT * FROM note_tags;
DROP TABLE note_revisions;
DROP TABLE note_tags;
DROP TABLE notes;
ALTER TABLE notes_v3 RENAME TO notes;
ALTER TABLE note_revisions_v3 RENAME TO note_revisions;
ALTER TABLE note_tags_v3 RENAME TO note_tags;

CREATE INDEX notes_updated_idx ON notes (updated_at_ms DESC, id ASC);
CREATE INDEX notes_created_idx ON notes (created_at_ms DESC, id ASC);
CREATE INDEX notes_diary_idx ON notes (diary_date DESC, updated_at_ms DESC, id ASC);
CREATE INDEX notes_status_idx ON notes (status, deleted_at_ms, updated_at_ms DESC);
CREATE INDEX note_tags_key_idx ON note_tags (tag_key, note_id);
CREATE INDEX note_revisions_retention_idx
    ON note_revisions (note_id, recorded_at_ms DESC, revision DESC);
"#;

pub(crate) fn ensure_supported(connection: &Connection) -> Result<(), NotesError> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(NotesError::UnsupportedSchema {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(())
}

pub(crate) fn migrate(connection: &mut Connection) -> Result<(), NotesError> {
    ensure_supported(connection)?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > 0 && version < CURRENT_SCHEMA_VERSION {
        ensure_migration_backup(connection, version)?;
    }
    for (target, sql) in [
        (1_u32, MIGRATION_1),
        (2_u32, MIGRATION_2),
        (3_u32, MIGRATION_3),
    ] {
        if version >= target {
            continue;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", target)?;
        transaction.commit()?;
    }
    Ok(())
}

fn ensure_migration_backup(connection: &Connection, version: u32) -> Result<(), NotesError> {
    let Some(source) = connection.path().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let source = Path::new(source);
    let backup = migration_backup_path(source, version);
    if backup.exists() {
        return validate_migration_backup(&backup, version);
    }

    let temporary = temporary_backup_path(&backup);
    let result = create_migration_backup(connection, &temporary, &backup, version);
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_migration_backup(
    connection: &Connection,
    temporary: &Path,
    backup: &Path,
    version: u32,
) -> Result<(), NotesError> {
    let temporary_text = temporary.to_str().ok_or(NotesError::MigrationBackup {
        reason: "notes_migration_backup_path_invalid",
    })?;
    connection
        .execute("VACUUM main INTO ?1", params![temporary_text])
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_snapshot_failed",
        })?;
    let mut permissions = fs::metadata(temporary)
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_metadata_failed",
        })?
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
    }
    fs::set_permissions(temporary, permissions).map_err(|_| NotesError::MigrationBackup {
        reason: "notes_migration_backup_permissions_failed",
    })?;
    validate_migration_backup(temporary, version)?;
    fs::File::open(temporary)
        .and_then(|file| file.sync_all())
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_sync_failed",
        })?;

    match fs::hard_link(temporary, backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_migration_backup(backup, version)?;
        }
        Err(_) => {
            return Err(NotesError::MigrationBackup {
                reason: "notes_migration_backup_publish_failed",
            });
        }
    }
    sync_parent_directory(backup)?;
    Ok(())
}

fn validate_migration_backup(path: &Path, expected_version: u32) -> Result<(), NotesError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| NotesError::MigrationBackup {
        reason: "notes_migration_backup_metadata_failed",
    })?;
    if !metadata.file_type().is_file() {
        return Err(NotesError::MigrationBackup {
            reason: "notes_migration_backup_unsafe",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(NotesError::MigrationBackup {
                reason: "notes_migration_backup_unsafe",
            });
        }
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let backup =
        Connection::open_with_flags(path, flags).map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_invalid",
        })?;
    let version: u32 = backup
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_invalid",
        })?;
    let check: String = backup
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_invalid",
        })?;
    if version != expected_version || check != "ok" {
        return Err(NotesError::MigrationBackup {
            reason: "notes_migration_backup_invalid",
        });
    }
    Ok(())
}

fn migration_backup_path(source: &Path, version: u32) -> PathBuf {
    let mut name: OsString = source.as_os_str().to_owned();
    name.push(format!(".v{version}{MIGRATION_BACKUP_SUFFIX}"));
    PathBuf::from(name)
}

fn temporary_backup_path(backup: &Path) -> PathBuf {
    let mut name: OsString = backup.as_os_str().to_owned();
    name.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    PathBuf::from(name)
}

fn sync_parent_directory(path: &Path) -> Result<(), NotesError> {
    let parent = path.parent().ok_or(NotesError::MigrationBackup {
        reason: "notes_migration_backup_path_invalid",
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| NotesError::MigrationBackup {
            reason: "notes_migration_backup_sync_failed",
        })
}
