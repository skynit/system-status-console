mod common;

use localdesk_notes::{NoteQuery, NoteSort, NoteStatus, NotesRepository};

#[test]
fn diary_and_list_queries_return_the_same_note_entity() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let first = repository
        .create(
            common::create_note(
                "周报",
                "包含关键字 Alpha",
                Some("2026-08-08"),
                &["工作", "周报"],
            ),
            100,
        )
        .unwrap();
    repository
        .create(
            common::create_note("随手记", "普通正文", Some("2026-08-07"), &["个人"]),
            200,
        )
        .unwrap();

    let diary = repository
        .query(&NoteQuery {
            diary_date_from: Some("2026-08-08".to_owned()),
            diary_date_to: Some("2026-08-08".to_owned()),
            sort: NoteSort::DiaryDateDesc,
            ..NoteQuery::default()
        })
        .unwrap();
    let list = repository
        .query(&NoteQuery {
            search: Some("alpha".to_owned()),
            tags: vec!["工作".to_owned()],
            status: Some(NoteStatus::Active),
            sort: NoteSort::TitleAsc,
            ..NoteQuery::default()
        })
        .unwrap();
    assert_eq!(diary, vec![first.clone()]);
    assert_eq!(list, vec![first]);
}

#[test]
fn stable_sort_uses_pinned_then_timestamp_then_id() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let mut normal = common::create_note("normal", "", None, &[]);
    normal.pinned = false;
    let normal = repository.create(normal, 100).unwrap();
    let mut pinned = common::create_note("pinned", "", None, &[]);
    pinned.pinned = true;
    let pinned = repository.create(pinned, 100).unwrap();

    let notes = repository.query(&NoteQuery::default()).unwrap();
    assert_eq!(notes[0].id, pinned.id);
    assert_eq!(notes[1].id, normal.id);
}

#[test]
fn completed_status_roundtrips_and_remains_queryable() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let mut completed = common::create_note("完成记录", "已处理", Some("2026-08-13"), &[]);
    completed.status = NoteStatus::Completed;
    let stored = repository.create(completed, 100).unwrap();

    assert_eq!(stored.status, NoteStatus::Completed);
    let notes = repository
        .query(&NoteQuery {
            status: Some(NoteStatus::Completed),
            ..NoteQuery::default()
        })
        .unwrap();
    assert_eq!(notes, vec![stored]);
}
