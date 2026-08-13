mod common;

use localdesk_notes::{ExportFormat, NotesRepository, export_note};
use serde_json::Value;

#[test]
fn exports_raw_markdown_and_typed_json_without_mutating_note() {
    let mut repository = NotesRepository::open_in_memory().unwrap();
    let note = repository
        .create(
            common::create_note(
                "发布记录",
                "- [x] 保留 Markdown\n\n**完成**",
                Some("2026-08-08"),
                &["发布"],
            ),
            100,
        )
        .unwrap();

    let markdown = export_note(&note, ExportFormat::Markdown).unwrap();
    assert!(markdown.contains("schema: localdesk.notes.export.v1"));
    assert!(markdown.ends_with("- [x] 保留 Markdown\n\n**完成**"));

    let json = export_note(&note, ExportFormat::Json).unwrap();
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema"], "localdesk.notes.export.v1");
    assert_eq!(value["notes"][0]["id"], note.id);
    assert_eq!(value["notes"][0]["body_markdown"], note.body_markdown);
    assert_eq!(repository.get(&note.id).unwrap(), note);
}
