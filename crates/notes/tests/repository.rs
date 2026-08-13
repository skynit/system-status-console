mod common;

use localdesk_notes::{
    DeletedFilter, NoteQuery, NotesError, NotesRepository, RetentionPolicy, SaveNote,
    set_checklist_item,
};
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn create_save_delete_restore_and_restart_use_one_note() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");
    let mut repository = NotesRepository::open(&path).unwrap();
    let created = repository
        .create(
            common::create_note(
                "今日",
                "- [ ] 完成部署\n\n正文",
                Some("2026-08-08"),
                &["工作"],
            ),
            100,
        )
        .unwrap();
    assert_eq!(created.revision, 1);
    assert_eq!(created.checklist()[0].text, "完成部署");

    let saved = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: common::draft(
                    "今日",
                    "- [x] 完成部署\n\n中文输入法组合文本：你好世界",
                    Some("2026-08-08"),
                    &["工作", "已完成"],
                ),
            },
            200,
        )
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(
        saved.body_markdown,
        "- [x] 完成部署\n\n中文输入法组合文本：你好世界"
    );

    let deleted = repository.soft_delete(&created.id, 2, 300).unwrap();
    assert_eq!(deleted.deleted_at_ms, Some(300));
    assert!(repository.query(&NoteQuery::default()).unwrap().is_empty());
    let restored = repository.restore(&created.id, 3, 400).unwrap();
    assert_eq!(restored.deleted_at_ms, None);
    drop(repository);

    let reopened = NotesRepository::open(&path).unwrap();
    let after_restart = reopened.get(&created.id).unwrap();
    assert_eq!(after_restart, restored);
    assert_eq!(
        reopened
            .query(&NoteQuery {
                deleted: DeletedFilter::Include,
                ..NoteQuery::default()
            })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn failed_revision_insert_rolls_back_current_note_atomically() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("atomic.sqlite3");
    let mut repository = NotesRepository::open(&path).unwrap();
    let created = repository
        .create(common::create_note("before", "原文", None, &[]), 100)
        .unwrap();

    let fault = Connection::open(&path).unwrap();
    fault
        .execute_batch(
            "CREATE TRIGGER fail_revision BEFORE INSERT ON note_revisions\n             BEGIN SELECT RAISE(ABORT, 'simulated crash boundary'); END;",
        )
        .unwrap();
    drop(fault);

    let result = repository.save(
        SaveNote {
            id: created.id.clone(),
            expected_revision: 1,
            draft: common::draft("after", "不应部分落盘", None, &[]),
        },
        200,
    );
    assert!(matches!(result, Err(NotesError::Sql(_))));
    assert_eq!(repository.get(&created.id).unwrap(), created);
}

#[test]
fn checklist_toggle_and_deleted_retention_are_transactional() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let created = repository
        .create(
            common::create_note("清单", "前言\n- [ ] 第一项\n- [x] 第二项", None, &[]),
            100,
        )
        .unwrap();
    let toggled = set_checklist_item(&created.body_markdown, 0, true).unwrap();
    assert_eq!(toggled, "前言\n- [x] 第一项\n- [x] 第二项");
    let saved = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: common::draft("清单", &toggled, None, &[]),
            },
            200,
        )
        .unwrap();
    assert!(saved.checklist().iter().all(|item| item.checked));
    let deleted = repository.soft_delete(&created.id, 2, 300).unwrap();

    let result = repository
        .apply_retention(RetentionPolicy {
            purge_deleted_before_ms: Some(300),
            prune_revisions_before_ms: None,
            keep_latest_revisions: 1,
        })
        .unwrap();
    assert_eq!(result.purged_notes, 1);
    assert!(matches!(
        repository.get(&deleted.id),
        Err(NotesError::NotFound { .. })
    ));
}
