mod common;

use localdesk_notes::{CURRENT_SCHEMA_VERSION, MIGRATION_BACKUP_SUFFIX, NotesRepository};
use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{ffi::OsString, path::Path};
use tempfile::tempdir;

fn backup_path(path: &Path, version: u32) -> std::path::PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(format!(".v{version}{MIGRATION_BACKUP_SUFFIX}"));
    name.into()
}

fn downgrade_to_version_one(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX notes_updated_idx;
             DROP INDEX notes_created_idx;
             DROP INDEX notes_diary_idx;
             DROP INDEX notes_status_idx;
             DROP INDEX note_tags_key_idx;
             DROP INDEX note_revisions_retention_idx;
             PRAGMA user_version = 1;",
        )
        .unwrap();
}

#[test]
fn migrates_empty_database_and_reopens_without_data_loss() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");

    let repository = NotesRepository::open(&path).unwrap();
    assert_eq!(repository.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(repository);

    let reopened = NotesRepository::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

    let connection = Connection::open(&path).unwrap();
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(schema.contains("'completed'"));
}

#[test]
fn rejects_a_database_from_a_newer_schema() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("future.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    assert_eq!(
        connection
            .pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))
            .unwrap(),
        "delete"
    );
    drop(connection);
    let future_bytes = std::fs::read(&path).unwrap();

    let error = NotesRepository::open(&path).unwrap_err();
    assert!(error.to_string().contains("newer than supported"));
    assert_eq!(std::fs::read(&path).unwrap(), future_bytes);
    assert!(!path.with_extension("sqlite3-wal").exists());
    assert!(!path.with_extension("sqlite3-shm").exists());
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))
            .unwrap(),
        "delete"
    );
}

#[test]
fn corrupt_database_is_rejected_without_rebuild_or_backup() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corrupt.sqlite3");
    let bytes = b"not a sqlite database";
    std::fs::write(&path, bytes).unwrap();

    let error = NotesRepository::open(&path).unwrap_err();
    assert!(matches!(
        error,
        localdesk_notes::NotesError::CorruptData {
            field: "database",
            ..
        }
    ));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!backup_path(&path, 1).exists());
    assert!(!path.with_extension("sqlite3-wal").exists());
    assert!(!path.with_extension("sqlite3-shm").exists());
}

#[test]
fn upgrades_version_one_in_place_and_preserves_notes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("version-one.sqlite3");
    let mut repository = NotesRepository::open(&path).unwrap();
    let note = repository
        .create(
            common::create_note("迁移", "保留正文", None, &["数据库"]),
            100,
        )
        .unwrap();
    drop(repository);

    downgrade_to_version_one(&path);

    let wal_writer = Connection::open(&path).unwrap();
    wal_writer
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             UPDATE notes SET title = 'WAL中的迁移标题' WHERE id = (SELECT id FROM notes LIMIT 1);",
        )
        .unwrap();

    let upgraded = NotesRepository::open(&path).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(upgraded.get(&note.id).unwrap().title, "WAL中的迁移标题");

    let backup_path = backup_path(&path, 1);
    let backup = Connection::open_with_flags(
        &backup_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    assert_eq!(
        backup
            .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row("SELECT title FROM notes WHERE id = ?1", [&note.id], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "WAL中的迁移标题"
    );
    assert_eq!(
        backup
            .pragma_query_value::<String, _>(None, "quick_check", |row| row.get(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn valid_existing_migration_backup_is_reused_after_an_interrupted_upgrade() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");
    let repository = NotesRepository::open(&path).unwrap();
    drop(repository);
    downgrade_to_version_one(&path);

    let backup_path = backup_path(&path, 1);
    let source = Connection::open(&path).unwrap();
    source
        .execute("VACUUM main INTO ?1", [backup_path.to_str().unwrap()])
        .unwrap();
    drop(source);
    #[cfg(unix)]
    std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::read(&backup_path).unwrap();

    let upgraded = NotesRepository::open(&path).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(std::fs::read(&backup_path).unwrap(), before);
}

#[test]
fn invalid_existing_migration_backup_blocks_upgrade_and_preserves_version_one() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");
    let repository = NotesRepository::open(&path).unwrap();
    drop(repository);
    downgrade_to_version_one(&path);
    let backup_path = backup_path(&path, 1);
    std::fs::write(&backup_path, b"not a sqlite backup").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = NotesRepository::open(&path).unwrap_err();
    assert!(matches!(
        error,
        localdesk_notes::NotesError::MigrationBackup {
            reason: "notes_migration_backup_invalid"
        }
    ));
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        1
    );
    assert_eq!(std::fs::read(&backup_path).unwrap(), b"not a sqlite backup");
}

#[cfg(unix)]
#[test]
fn symlink_migration_backup_is_never_followed_or_overwritten() {
    use std::os::unix::fs::symlink;

    let directory = tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");
    let repository = NotesRepository::open(&path).unwrap();
    drop(repository);
    downgrade_to_version_one(&path);

    let target = directory.path().join("outside-target");
    std::fs::write(&target, b"must remain untouched").unwrap();
    let backup_path = backup_path(&path, 1);
    symlink(&target, &backup_path).unwrap();

    let error = NotesRepository::open(&path).unwrap_err();
    assert!(matches!(
        error,
        localdesk_notes::NotesError::MigrationBackup {
            reason: "notes_migration_backup_unsafe"
        }
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"must remain untouched");
    assert!(
        std::fs::symlink_metadata(&backup_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        1
    );
}
