mod common;

use localdesk_notes::{NotesError, NotesRepository, RetentionPolicy, SaveNote};

#[test]
fn stale_autosave_returns_current_and_preserves_submitted_ime_text() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let created = repository
        .create(common::create_note("草稿", "初始", None, &[]), 100)
        .unwrap();
    let current = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: common::draft("草稿", "先到达的保存", None, &[]),
            },
            200,
        )
        .unwrap();

    let submitted = common::draft("草稿", "仍在组合的中文：拼音输入", None, &[]);
    let error = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: submitted.clone(),
            },
            210,
        )
        .unwrap_err();
    let NotesError::Conflict(conflict) = error else {
        panic!("expected typed conflict");
    };
    assert_eq!(conflict.current, current);
    assert_eq!(conflict.submitted, submitted);
    assert_eq!(repository.get(&created.id).unwrap(), current);
}

#[test]
fn revisions_snapshot_tags_and_retention_keeps_latest() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let created = repository
        .create(common::create_note("v1", "one", None, &["alpha"]), 100)
        .unwrap();
    let second = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: common::draft("v2", "two", None, &["beta"]),
            },
            200,
        )
        .unwrap();
    repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 2,
                draft: common::draft("v3", "three", None, &["gamma"]),
            },
            300,
        )
        .unwrap();
    let revisions = repository.revisions(&created.id).unwrap();
    assert_eq!(revisions.len(), 3);
    assert_eq!(revisions[1].note, second);

    let result = repository
        .apply_retention(RetentionPolicy {
            purge_deleted_before_ms: None,
            prune_revisions_before_ms: Some(1_000),
            keep_latest_revisions: 1,
        })
        .unwrap();
    assert_eq!(result.pruned_revisions, 2);
    assert_eq!(repository.revisions(&created.id).unwrap().len(), 1);
}

#[test]
fn unchanged_autosave_does_not_create_revision_noise() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let created = repository
        .create(common::create_note("same", "same", None, &["same"]), 100)
        .unwrap();
    let unchanged = repository
        .save(
            SaveNote {
                id: created.id.clone(),
                expected_revision: 1,
                draft: common::draft("same", "same", None, &["same"]),
            },
            200,
        )
        .unwrap();
    assert_eq!(unchanged, created);
    assert_eq!(repository.revisions(&created.id).unwrap().len(), 1);
}
