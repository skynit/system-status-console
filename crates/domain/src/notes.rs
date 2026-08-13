use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const NOTES_SCHEMA_VERSION: u16 = 1;
pub const MAX_NOTE_TITLE_CHARS: usize = 512;
pub const MAX_NOTE_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NOTE_TAGS: usize = 64;
pub const MAX_NOTE_TAG_CHARS: usize = 64;
pub const MAX_NOTE_QUERY_LIMIT: u32 = 64;
pub const MAX_NOTE_QUERY_OFFSET: u32 = 100_000;
pub const MAX_NOTE_SEARCH_CHARS: usize = 512;
pub const NOTE_CONTENT_CHUNK_BYTES: usize = 45_056;
pub const MAX_NOTE_CONTENT_BASE64_BYTES: usize = 60_076;
pub const MAX_NOTE_UPLOAD_SESSIONS: usize = 4;
pub const MAX_NOTE_STAGED_BYTES: usize = 16 * 1024 * 1024;
/// The export stream has 600 frames total; start/end leave 598 content frames.
pub const MAX_NOTE_EXPORT_DATA_FRAMES: usize = 598;
pub const MAX_NOTE_EXPORT_BYTES: usize = NOTE_CONTENT_CHUNK_BYTES * MAX_NOTE_EXPORT_DATA_FRAMES;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    Draft,
    Active,
    Completed,
    Archived,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteDeletedFilter {
    #[default]
    Exclude,
    Include,
    Only,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteSort {
    #[default]
    UpdatedDesc,
    CreatedDesc,
    TitleAsc,
    DiaryDateDesc,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDraftMeta {
    pub title: String,
    pub diary_date: Option<String>,
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub pinned: bool,
}

impl NoteDraftMeta {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.chars().count() > MAX_NOTE_TITLE_CHARS {
            return Err("note_title_exceeds_512_characters");
        }
        if self.title.contains('\0') {
            return Err("note_title_contains_nul");
        }
        if self.tags.len() > MAX_NOTE_TAGS {
            return Err("note_tags_exceeds_64");
        }
        if self.tags.iter().any(|tag| {
            tag.is_empty() || tag.chars().count() > MAX_NOTE_TAG_CHARS || tag.contains('\0')
        }) {
            return Err("note_tag_invalid");
        }
        if let Some(date) = &self.diary_date
            && !valid_calendar_date(date)
        {
            return Err("note_diary_date_invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub diary_date: Option<String>,
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub pinned: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
    pub revision: u64,
    pub body_bytes: u32,
    pub body_sha256: String,
}

impl NoteSummary {
    pub fn validate(&self) -> Result<(), &'static str> {
        if Uuid::parse_str(&self.id).is_err() {
            return Err("note_id_invalid");
        }
        NoteDraftMeta {
            title: self.title.clone(),
            diary_date: self.diary_date.clone(),
            tags: self.tags.clone(),
            status: self.status,
            pinned: self.pinned,
        }
        .validate()?;
        if self.revision == 0 || self.body_bytes as usize > MAX_NOTE_BODY_BYTES {
            return Err("note_summary_revision_or_size_invalid");
        }
        validate_sha256(&self.body_sha256)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDocument {
    pub summary: NoteSummary,
    pub body_markdown: String,
}

impl NoteDocument {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.summary.validate()?;
        if self.body_markdown.len() != self.summary.body_bytes as usize {
            return Err("note_document_body_length_mismatch");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteQuery {
    pub search: Option<String>,
    pub diary_date_from: Option<String>,
    pub diary_date_to: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<NoteStatus>,
    pub deleted: NoteDeletedFilter,
    pub sort: NoteSort,
    pub limit: u32,
    pub offset: u32,
}

impl NoteQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=MAX_NOTE_QUERY_LIMIT).contains(&self.limit) {
            return Err("note_query_limit_invalid");
        }
        if self.offset > MAX_NOTE_QUERY_OFFSET {
            return Err("note_query_offset_invalid");
        }
        if self.search.as_ref().is_some_and(|search| {
            search.chars().count() > MAX_NOTE_SEARCH_CHARS || search.contains('\0')
        }) {
            return Err("note_query_search_invalid");
        }
        if self.tags.len() > MAX_NOTE_TAGS
            || self.tags.iter().any(|tag| {
                tag.is_empty() || tag.chars().count() > MAX_NOTE_TAG_CHARS || tag.contains('\0')
            })
        {
            return Err("note_query_tags_invalid");
        }
        for date in [&self.diary_date_from, &self.diary_date_to]
            .into_iter()
            .flatten()
        {
            if !valid_calendar_date(date) {
                return Err("note_query_date_invalid");
            }
        }
        Ok(())
    }
}

impl Default for NoteQuery {
    fn default() -> Self {
        Self {
            search: None,
            diary_date_from: None,
            diary_date_to: None,
            tags: Vec::new(),
            status: None,
            deleted: NoteDeletedFilter::Exclude,
            sort: NoteSort::UpdatedDesc,
            limit: 64,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotePage {
    pub query: NoteQuery,
    pub notes: Vec<NoteSummary>,
    pub has_more: bool,
    pub next_offset: Option<u32>,
}

impl NotePage {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.query.validate()?;
        if self.notes.len() > self.query.limit as usize
            || self.notes.iter().any(|note| note.validate().is_err())
        {
            return Err("note_page_records_invalid");
        }
        let expected_next = if self.has_more {
            Some(
                self.query
                    .offset
                    .checked_add(self.notes.len() as u32)
                    .ok_or("note_page_next_offset_overflow")?,
            )
        } else {
            None
        };
        if self.next_offset != expected_next {
            return Err("note_page_next_offset_invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoteWriteIntent {
    Create,
    Save {
        id: String,
        expected_revision: u64,
        autosave: bool,
    },
}

impl NoteWriteIntent {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Create => Ok(()),
            Self::Save {
                id,
                expected_revision,
                ..
            } if Uuid::parse_str(id).is_ok() && *expected_revision > 0 => Ok(()),
            Self::Save { .. } => Err("note_write_intent_invalid"),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteExportFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum NotesCommand {
    List {
        query: NoteQuery,
    },
    Get {
        id: String,
    },
    WriteInline {
        intent: NoteWriteIntent,
        meta: NoteDraftMeta,
        body_markdown: String,
    },
    Delete {
        id: String,
        expected_revision: u64,
    },
    Restore {
        id: String,
        expected_revision: u64,
    },
    Export {
        query: NoteQuery,
        format: NoteExportFormat,
    },
    UploadBegin {
        intent: NoteWriteIntent,
        meta: NoteDraftMeta,
        expected_total_bytes: u32,
        body_sha256: String,
    },
    UploadAppend {
        upload_id: Uuid,
        sequence: u32,
        offset: u32,
        data_base64: String,
    },
    UploadCommit {
        upload_id: Uuid,
        intent: NoteWriteIntent,
    },
    UploadAbort {
        upload_id: Uuid,
    },
}

impl NotesCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::List { query } | Self::Export { query, .. } => query.validate(),
            Self::Get { id } | Self::Delete { id, .. } | Self::Restore { id, .. } => {
                if Uuid::parse_str(id).is_ok() {
                    Ok(())
                } else {
                    Err("note_id_invalid")
                }
            }
            Self::WriteInline {
                intent,
                meta,
                body_markdown,
            } => {
                intent.validate()?;
                meta.validate()?;
                if body_markdown.len() > MAX_NOTE_BODY_BYTES {
                    return Err("note_body_exceeds_4_mib");
                }
                Ok(())
            }
            Self::UploadBegin {
                intent,
                meta,
                expected_total_bytes,
                body_sha256,
            } => {
                intent.validate()?;
                meta.validate()?;
                if *expected_total_bytes as usize > MAX_NOTE_BODY_BYTES {
                    return Err("note_body_exceeds_4_mib");
                }
                validate_sha256(body_sha256)
            }
            Self::UploadAppend {
                upload_id,
                data_base64,
                ..
            } => {
                if upload_id.is_nil() || data_base64.len() > MAX_NOTE_CONTENT_BASE64_BYTES {
                    return Err("note_upload_append_invalid");
                }
                Ok(())
            }
            Self::UploadCommit { upload_id, intent } => {
                intent.validate()?;
                if upload_id.is_nil() {
                    Err("note_upload_id_invalid")
                } else {
                    Ok(())
                }
            }
            Self::UploadAbort { upload_id } => {
                if upload_id.is_nil() {
                    Err("note_upload_id_invalid")
                } else {
                    Ok(())
                }
            }
        }?;
        if matches!(
            self,
            Self::Delete {
                expected_revision: 0,
                ..
            } | Self::Restore {
                expected_revision: 0,
                ..
            }
        ) {
            return Err("note_expected_revision_invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NoteMutationResult {
    Stored(NoteSummary),
    Deleted(NoteSummary),
    Restored(NoteSummary),
    Conflict {
        expected_revision: u64,
        current: NoteSummary,
    },
    UploadBegun {
        upload_id: Uuid,
        max_chunk_raw_bytes: u32,
    },
    UploadAccepted {
        upload_id: Uuid,
        next_sequence: u32,
        next_offset: u32,
    },
    UploadAborted {
        upload_id: Uuid,
    },
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteExport {
    pub format: NoteExportFormat,
    pub content: String,
    pub content_bytes: u32,
    pub content_sha256: String,
}

impl NoteExport {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.content.len() != self.content_bytes as usize
            || self.content.len() > MAX_NOTE_EXPORT_BYTES
        {
            return Err("note_export_length_invalid");
        }
        validate_sha256(&self.content_sha256)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NotesOutput {
    Mutation(NoteMutationResult),
    Page(NotePage),
    Document(NoteDocument),
    Export(NoteExport),
}

pub fn validate_sha256(value: &str) -> Result<(), &'static str> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err("note_sha256_invalid")
    }
}

fn valid_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return false;
    }
    let Ok(year) = value[0..4].parse::<u32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => return false,
    };
    (1..=max_day).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_validation_rejects_nonexistent_dates() {
        let query = NoteQuery {
            diary_date_from: Some("2026-02-29".to_owned()),
            ..NoteQuery::default()
        };
        assert_eq!(query.validate(), Err("note_query_date_invalid"));
        let query = NoteQuery {
            diary_date_from: Some("2028-02-29".to_owned()),
            ..NoteQuery::default()
        };
        assert!(query.validate().is_ok());
    }

    #[test]
    fn upload_limits_and_sha_are_exact() {
        assert_eq!(NOTE_CONTENT_CHUNK_BYTES, 45_056);
        assert_eq!(MAX_NOTE_CONTENT_BASE64_BYTES, 60_076);
        assert_eq!(MAX_NOTE_EXPORT_DATA_FRAMES, 598);
        assert_eq!(MAX_NOTE_EXPORT_BYTES, 26_943_488);
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
    }
}
