#![allow(dead_code)]

use localdesk_notes::{CreateNote, NoteDraft, NoteStatus};

pub fn create_note(title: &str, body: &str, date: Option<&str>, tags: &[&str]) -> CreateNote {
    CreateNote {
        title: title.to_owned(),
        body_markdown: body.to_owned(),
        diary_date: date.map(str::to_owned),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        status: NoteStatus::Active,
        pinned: false,
    }
}

pub fn draft(title: &str, body: &str, date: Option<&str>, tags: &[&str]) -> NoteDraft {
    NoteDraft {
        title: title.to_owned(),
        body_markdown: body.to_owned(),
        diary_date: date.map(str::to_owned),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        status: NoteStatus::Active,
        pinned: false,
    }
}
