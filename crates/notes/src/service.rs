use base64::{Engine as _, engine::general_purpose::STANDARD};
use localdesk_domain::{
    MAX_NOTE_BODY_BYTES, MAX_NOTE_EXPORT_BYTES, MAX_NOTE_STAGED_BYTES, MAX_NOTE_UPLOAD_SESSIONS,
    NOTE_CONTENT_CHUNK_BYTES, NoteDeletedFilter as PublicDeletedFilter,
    NoteDocument as PublicDocument, NoteDraftMeta, NoteExport, NoteExportFormat,
    NoteMutationResult, NotePage, NoteQuery as PublicQuery, NoteSort as PublicSort,
    NoteStatus as PublicStatus, NoteSummary, NoteWriteIntent, NotesCommand, NotesOutput,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    error::Error,
    fmt::{self, Write as _},
    time::{Duration, Instant},
};
use uuid::Uuid;

use crate::{
    CreateNote, DeletedFilter, ExportFormat, Note, NoteDraft, NoteQuery, NoteSort, NoteStatus,
    NotesError, NotesRepository, SaveNote,
    export::{BoundedExporter, is_export_too_large},
};

pub const NOTE_UPLOAD_IDLE_TTL: Duration = Duration::from_secs(60);

pub struct NotesService {
    repository: NotesRepository,
    uploads: HashMap<Uuid, PendingUpload>,
    reserved_staged_bytes: usize,
    accepting_uploads: bool,
}

impl fmt::Debug for NotesService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotesService")
            .field("active_uploads", &self.uploads.len())
            .field("reserved_staged_bytes", &self.reserved_staged_bytes)
            .field("accepting_uploads", &self.accepting_uploads)
            .finish_non_exhaustive()
    }
}

struct PendingUpload {
    intent: NoteWriteIntent,
    meta: NoteDraftMeta,
    expected_total_bytes: usize,
    expected_sha256: String,
    bytes: Vec<u8>,
    next_sequence: u32,
    last_activity: Instant,
}

#[derive(Debug)]
pub enum NotesServiceError {
    InvalidCommand(&'static str),
    Repository(NotesError),
    UploadCapacity,
    UploadNotFound,
    UploadSequence,
    UploadOffset,
    UploadChunk,
    UploadLength,
    UploadHash,
    UploadUtf8,
    UploadClosed,
    ExportTooLarge,
}

impl NotesServiceError {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::InvalidCommand(reason) => reason,
            Self::Repository(NotesError::NotFound { .. }) => "note_not_found",
            Self::Repository(NotesError::UnsupportedSchema { .. }) => "notes_schema_unsupported",
            Self::Repository(NotesError::MigrationBackup { reason }) => reason,
            Self::Repository(NotesError::Conflict(_)) => "note_revision_conflict",
            Self::Repository(NotesError::Validation { .. }) => "note_validation_failed",
            Self::Repository(NotesError::CorruptData { .. }) => "notes_database_corrupt",
            Self::Repository(NotesError::Sql(_) | NotesError::Json(_)) => "notes_storage_failed",
            Self::UploadCapacity => "note_upload_capacity_exceeded",
            Self::UploadNotFound => "note_upload_not_found",
            Self::UploadSequence => "note_upload_sequence_invalid",
            Self::UploadOffset => "note_upload_offset_invalid",
            Self::UploadChunk => "note_upload_chunk_invalid",
            Self::UploadLength => "note_upload_length_mismatch",
            Self::UploadHash => "note_upload_hash_mismatch",
            Self::UploadUtf8 => "note_upload_utf8_invalid",
            Self::UploadClosed => "note_uploads_closed",
            Self::ExportTooLarge => "note_export_too_large",
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Repository(NotesError::Sql(_)) | Self::UploadCapacity
        )
    }
}

impl fmt::Display for NotesServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason_code())
    }
}

impl Error for NotesServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            _ => None,
        }
    }
}

impl From<NotesError> for NotesServiceError {
    fn from(value: NotesError) -> Self {
        Self::Repository(value)
    }
}

impl NotesService {
    pub fn new(repository: NotesRepository) -> Self {
        Self {
            repository,
            uploads: HashMap::new(),
            reserved_staged_bytes: 0,
            accepting_uploads: true,
        }
    }

    pub fn execute(
        &mut self,
        command: NotesCommand,
        now_ms: i64,
        now: Instant,
    ) -> Result<NotesOutput, NotesServiceError> {
        command
            .validate()
            .map_err(NotesServiceError::InvalidCommand)?;
        self.cleanup_expired(now);
        match command {
            NotesCommand::List { query } => self.list(query),
            NotesCommand::Get { id } => self.get(&id),
            NotesCommand::WriteInline {
                intent,
                meta,
                body_markdown,
            } => self.commit_draft(intent, meta, body_markdown, now_ms),
            NotesCommand::Delete {
                id,
                expected_revision,
            } => self.delete(&id, expected_revision, now_ms),
            NotesCommand::Restore {
                id,
                expected_revision,
            } => self.restore(&id, expected_revision, now_ms),
            NotesCommand::Export { query, format } => self.export(query, format),
            NotesCommand::UploadBegin {
                intent,
                meta,
                expected_total_bytes,
                body_sha256,
            } => self.begin_upload(
                intent,
                meta,
                expected_total_bytes as usize,
                body_sha256,
                now,
            ),
            NotesCommand::UploadAppend {
                upload_id,
                sequence,
                offset,
                data_base64,
            } => self.append_upload(upload_id, sequence, offset, data_base64, now),
            NotesCommand::UploadCommit { upload_id, intent } => {
                self.commit_upload(upload_id, intent, now_ms)
            }
            NotesCommand::UploadAbort { upload_id } => {
                Ok(NotesOutput::Mutation(self.abort_upload(upload_id)))
            }
        }
    }

    pub fn cleanup_expired(&mut self, now: Instant) -> usize {
        let expired = self
            .uploads
            .iter()
            .filter_map(|(id, upload)| {
                now.checked_duration_since(upload.last_activity)
                    .is_some_and(|age| age >= NOTE_UPLOAD_IDLE_TTL)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in &expired {
            self.remove_upload(*id);
        }
        expired.len()
    }

    pub fn begin_shutdown(&mut self) {
        self.accepting_uploads = false;
    }

    pub fn shutdown(&mut self) -> Result<(), NotesServiceError> {
        self.begin_shutdown();
        self.uploads.clear();
        self.reserved_staged_bytes = 0;
        self.repository.checkpoint()?;
        Ok(())
    }

    pub fn active_uploads(&self) -> usize {
        self.uploads.len()
    }

    pub fn reserved_staged_bytes(&self) -> usize {
        self.reserved_staged_bytes
    }

    fn list(&self, query: PublicQuery) -> Result<NotesOutput, NotesServiceError> {
        let mut repository_query = repository_query(&query);
        repository_query.limit = query.limit + 1;
        let mut notes = self.repository.query(&repository_query)?;
        let has_more = notes.len() > query.limit as usize;
        if has_more {
            notes.truncate(query.limit as usize);
        }
        let summaries = notes.iter().map(note_summary).collect::<Vec<_>>();
        let next_offset = has_more
            .then(|| query.offset.checked_add(summaries.len() as u32))
            .flatten();
        let page = NotePage {
            query,
            notes: summaries,
            has_more,
            next_offset,
        };
        page.validate().map_err(NotesServiceError::InvalidCommand)?;
        Ok(NotesOutput::Page(page))
    }

    fn get(&self, id: &str) -> Result<NotesOutput, NotesServiceError> {
        let note = self.repository.get(id)?;
        let document = note_document(&note);
        document
            .validate()
            .map_err(NotesServiceError::InvalidCommand)?;
        Ok(NotesOutput::Document(document))
    }

    fn delete(
        &mut self,
        id: &str,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<NotesOutput, NotesServiceError> {
        match self.repository.soft_delete(id, expected_revision, now_ms) {
            Ok(note) => Ok(NotesOutput::Mutation(NoteMutationResult::Deleted(
                note_summary(&note),
            ))),
            Err(NotesError::Conflict(conflict)) => {
                Ok(NotesOutput::Mutation(NoteMutationResult::Conflict {
                    expected_revision: conflict.expected_revision,
                    current: note_summary(&conflict.current),
                }))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn restore(
        &mut self,
        id: &str,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<NotesOutput, NotesServiceError> {
        match self.repository.restore(id, expected_revision, now_ms) {
            Ok(note) => Ok(NotesOutput::Mutation(NoteMutationResult::Restored(
                note_summary(&note),
            ))),
            Err(NotesError::Conflict(conflict)) => {
                Ok(NotesOutput::Mutation(NoteMutationResult::Conflict {
                    expected_revision: conflict.expected_revision,
                    current: note_summary(&conflict.current),
                }))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn export(
        &self,
        query: PublicQuery,
        format: NoteExportFormat,
    ) -> Result<NotesOutput, NotesServiceError> {
        let export_format = match format {
            NoteExportFormat::Markdown => ExportFormat::Markdown,
            NoteExportFormat::Json => ExportFormat::Json,
        };
        let mut exporter =
            BoundedExporter::new(export_format, MAX_NOTE_EXPORT_BYTES).map_err(map_export_error)?;
        self.repository
            .visit_query(&repository_query(&query), |note| exporter.push(&note))
            .map_err(map_export_error)?;
        let content = exporter.finish().map_err(map_export_error)?;
        Ok(NotesOutput::Export(NoteExport {
            format,
            content_bytes: content.len() as u32,
            content_sha256: sha256_hex(content.as_bytes()),
            content,
        }))
    }

    fn begin_upload(
        &mut self,
        intent: NoteWriteIntent,
        meta: NoteDraftMeta,
        expected_total_bytes: usize,
        expected_sha256: String,
        now: Instant,
    ) -> Result<NotesOutput, NotesServiceError> {
        if !self.accepting_uploads {
            return Err(NotesServiceError::UploadClosed);
        }
        if self.uploads.len() >= MAX_NOTE_UPLOAD_SESSIONS
            || self
                .reserved_staged_bytes
                .checked_add(expected_total_bytes)
                .is_none_or(|total| total > MAX_NOTE_STAGED_BYTES)
        {
            return Err(NotesServiceError::UploadCapacity);
        }
        let upload_id = Uuid::new_v4();
        self.reserved_staged_bytes += expected_total_bytes;
        self.uploads.insert(
            upload_id,
            PendingUpload {
                intent,
                meta,
                expected_total_bytes,
                expected_sha256,
                bytes: Vec::with_capacity(expected_total_bytes),
                next_sequence: 0,
                last_activity: now,
            },
        );
        Ok(NotesOutput::Mutation(NoteMutationResult::UploadBegun {
            upload_id,
            max_chunk_raw_bytes: NOTE_CONTENT_CHUNK_BYTES as u32,
        }))
    }

    fn append_upload(
        &mut self,
        upload_id: Uuid,
        sequence: u32,
        offset: u32,
        data_base64: String,
        now: Instant,
    ) -> Result<NotesOutput, NotesServiceError> {
        let decoded = match STANDARD.decode(data_base64.as_bytes()) {
            Ok(decoded) if !decoded.is_empty() && decoded.len() <= NOTE_CONTENT_CHUNK_BYTES => {
                decoded
            }
            _ => {
                self.remove_upload(upload_id);
                return Err(NotesServiceError::UploadChunk);
            }
        };
        let Some(upload) = self.uploads.get_mut(&upload_id) else {
            return Err(NotesServiceError::UploadNotFound);
        };
        if sequence != upload.next_sequence {
            self.remove_upload(upload_id);
            return Err(NotesServiceError::UploadSequence);
        }
        if offset as usize != upload.bytes.len() {
            self.remove_upload(upload_id);
            return Err(NotesServiceError::UploadOffset);
        }
        if upload
            .bytes
            .len()
            .checked_add(decoded.len())
            .is_none_or(|length| length > upload.expected_total_bytes)
        {
            self.remove_upload(upload_id);
            return Err(NotesServiceError::UploadLength);
        }
        upload.bytes.extend_from_slice(&decoded);
        upload.next_sequence = upload
            .next_sequence
            .checked_add(1)
            .ok_or(NotesServiceError::UploadSequence)?;
        upload.last_activity = now;
        Ok(NotesOutput::Mutation(NoteMutationResult::UploadAccepted {
            upload_id,
            next_sequence: upload.next_sequence,
            next_offset: upload.bytes.len() as u32,
        }))
    }

    fn commit_upload(
        &mut self,
        upload_id: Uuid,
        intent: NoteWriteIntent,
        now_ms: i64,
    ) -> Result<NotesOutput, NotesServiceError> {
        let upload = self
            .remove_upload(upload_id)
            .ok_or(NotesServiceError::UploadNotFound)?;
        if upload.intent != intent {
            return Err(NotesServiceError::InvalidCommand(
                "note_upload_intent_mismatch",
            ));
        }
        if upload.bytes.len() != upload.expected_total_bytes {
            return Err(NotesServiceError::UploadLength);
        }
        if sha256_hex(&upload.bytes) != upload.expected_sha256 {
            return Err(NotesServiceError::UploadHash);
        }
        let body_markdown =
            String::from_utf8(upload.bytes).map_err(|_| NotesServiceError::UploadUtf8)?;
        self.commit_draft(upload.intent, upload.meta, body_markdown, now_ms)
    }

    fn abort_upload(&mut self, upload_id: Uuid) -> NoteMutationResult {
        self.remove_upload(upload_id);
        NoteMutationResult::UploadAborted { upload_id }
    }

    fn remove_upload(&mut self, upload_id: Uuid) -> Option<PendingUpload> {
        let upload = self.uploads.remove(&upload_id)?;
        self.reserved_staged_bytes = self
            .reserved_staged_bytes
            .saturating_sub(upload.expected_total_bytes);
        Some(upload)
    }

    fn commit_draft(
        &mut self,
        intent: NoteWriteIntent,
        meta: NoteDraftMeta,
        body_markdown: String,
        now_ms: i64,
    ) -> Result<NotesOutput, NotesServiceError> {
        if body_markdown.len() > MAX_NOTE_BODY_BYTES {
            return Err(NotesServiceError::InvalidCommand("note_body_exceeds_4_mib"));
        }
        let draft = NoteDraft {
            title: meta.title,
            body_markdown,
            diary_date: meta.diary_date,
            tags: meta.tags,
            status: note_status(meta.status),
            pinned: meta.pinned,
        };
        let result = match intent {
            NoteWriteIntent::Create => self.repository.create(
                CreateNote {
                    title: draft.title,
                    body_markdown: draft.body_markdown,
                    diary_date: draft.diary_date,
                    tags: draft.tags,
                    status: draft.status,
                    pinned: draft.pinned,
                },
                now_ms,
            ),
            NoteWriteIntent::Save {
                id,
                expected_revision,
                ..
            } => self.repository.save(
                SaveNote {
                    id,
                    expected_revision,
                    draft,
                },
                now_ms,
            ),
        };
        match result {
            Ok(note) => Ok(NotesOutput::Mutation(NoteMutationResult::Stored(
                note_summary(&note),
            ))),
            Err(NotesError::Conflict(conflict)) => {
                Ok(NotesOutput::Mutation(NoteMutationResult::Conflict {
                    expected_revision: conflict.expected_revision,
                    current: note_summary(&conflict.current),
                }))
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn map_export_error(error: NotesError) -> NotesServiceError {
    if is_export_too_large(&error) {
        NotesServiceError::ExportTooLarge
    } else {
        NotesServiceError::Repository(error)
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn note_summary(note: &Note) -> NoteSummary {
    NoteSummary {
        id: note.id.clone(),
        title: note.title.clone(),
        diary_date: note.diary_date.clone(),
        tags: note.tags.clone(),
        status: public_status(note.status),
        pinned: note.pinned,
        created_at_ms: note.created_at_ms,
        updated_at_ms: note.updated_at_ms,
        deleted_at_ms: note.deleted_at_ms,
        revision: note.revision,
        body_bytes: note.body_markdown.len() as u32,
        body_sha256: sha256_hex(note.body_markdown.as_bytes()),
    }
}

fn note_document(note: &Note) -> PublicDocument {
    PublicDocument {
        summary: note_summary(note),
        body_markdown: note.body_markdown.clone(),
    }
}

fn repository_query(query: &PublicQuery) -> NoteQuery {
    NoteQuery {
        search: query.search.clone(),
        diary_date_from: query.diary_date_from.clone(),
        diary_date_to: query.diary_date_to.clone(),
        tags: query.tags.clone(),
        status: query.status.map(note_status),
        deleted: match query.deleted {
            PublicDeletedFilter::Exclude => DeletedFilter::Exclude,
            PublicDeletedFilter::Include => DeletedFilter::Include,
            PublicDeletedFilter::Only => DeletedFilter::Only,
        },
        sort: match query.sort {
            PublicSort::UpdatedDesc => NoteSort::UpdatedDesc,
            PublicSort::CreatedDesc => NoteSort::CreatedDesc,
            PublicSort::TitleAsc => NoteSort::TitleAsc,
            PublicSort::DiaryDateDesc => NoteSort::DiaryDateDesc,
        },
        limit: query.limit,
        offset: query.offset,
    }
}

fn note_status(status: PublicStatus) -> NoteStatus {
    match status {
        PublicStatus::Draft => NoteStatus::Draft,
        PublicStatus::Active => NoteStatus::Active,
        PublicStatus::Completed => NoteStatus::Completed,
        PublicStatus::Archived => NoteStatus::Archived,
    }
}

fn public_status(status: NoteStatus) -> PublicStatus {
    match status {
        NoteStatus::Draft => PublicStatus::Draft,
        NoteStatus::Active => PublicStatus::Active,
        NoteStatus::Completed => PublicStatus::Completed,
        NoteStatus::Archived => PublicStatus::Archived,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> NoteDraftMeta {
        NoteDraftMeta {
            title: "日记".to_owned(),
            diary_date: Some("2026-08-09".to_owned()),
            tags: vec!["测试".to_owned()],
            status: PublicStatus::Active,
            pinned: false,
        }
    }

    #[test]
    fn chunk_upload_handles_utf8_boundaries_and_commits_once() {
        let repository = NotesRepository::open_in_memory().expect("repository");
        let mut service = NotesService::new(repository);
        let body = "前台日记".repeat(10_000);
        let bytes = body.as_bytes();
        let now = Instant::now();
        let begun = service
            .execute(
                NotesCommand::UploadBegin {
                    intent: NoteWriteIntent::Create,
                    meta: meta(),
                    expected_total_bytes: bytes.len() as u32,
                    body_sha256: sha256_hex(bytes),
                },
                1,
                now,
            )
            .expect("begin");
        let NotesOutput::Mutation(NoteMutationResult::UploadBegun { upload_id, .. }) = begun else {
            panic!("upload begun result");
        };
        for (sequence, chunk) in bytes.chunks(7).enumerate() {
            service
                .execute(
                    NotesCommand::UploadAppend {
                        upload_id,
                        sequence: sequence as u32,
                        offset: (sequence * 7) as u32,
                        data_base64: STANDARD.encode(chunk),
                    },
                    1,
                    now,
                )
                .expect("append");
        }
        let stored = service
            .execute(
                NotesCommand::UploadCommit {
                    upload_id,
                    intent: NoteWriteIntent::Create,
                },
                2,
                now,
            )
            .expect("commit");
        let NotesOutput::Mutation(NoteMutationResult::Stored(summary)) = stored else {
            panic!("stored result");
        };
        let NotesOutput::Document(document) = service
            .execute(NotesCommand::Get { id: summary.id }, 3, now)
            .expect("get")
        else {
            panic!("document");
        };
        assert_eq!(document.body_markdown, body);
        assert_eq!(document.summary.revision, 1);
    }

    #[test]
    fn invalid_append_destroys_reserved_session() {
        let repository = NotesRepository::open_in_memory().expect("repository");
        let mut service = NotesService::new(repository);
        let now = Instant::now();
        let begun = service
            .execute(
                NotesCommand::UploadBegin {
                    intent: NoteWriteIntent::Create,
                    meta: meta(),
                    expected_total_bytes: 3,
                    body_sha256: sha256_hex(b"abc"),
                },
                1,
                now,
            )
            .expect("begin");
        let NotesOutput::Mutation(NoteMutationResult::UploadBegun { upload_id, .. }) = begun else {
            panic!("upload begun");
        };
        let error = service
            .execute(
                NotesCommand::UploadAppend {
                    upload_id,
                    sequence: 1,
                    offset: 0,
                    data_base64: STANDARD.encode(b"abc"),
                },
                1,
                now,
            )
            .expect_err("bad sequence");
        assert_eq!(error.reason_code(), "note_upload_sequence_invalid");
        assert_eq!(service.active_uploads(), 0);
        assert_eq!(service.reserved_staged_bytes(), 0);
    }

    #[test]
    fn empty_append_is_rejected_without_renewing_the_session() {
        let repository = NotesRepository::open_in_memory().expect("repository");
        let mut service = NotesService::new(repository);
        let now = Instant::now();
        let begun = service
            .execute(
                NotesCommand::UploadBegin {
                    intent: NoteWriteIntent::Create,
                    meta: meta(),
                    expected_total_bytes: 3,
                    body_sha256: sha256_hex(b"abc"),
                },
                1,
                now,
            )
            .expect("begin");
        let NotesOutput::Mutation(NoteMutationResult::UploadBegun { upload_id, .. }) = begun else {
            panic!("upload begun");
        };

        let error = service
            .execute(
                NotesCommand::UploadAppend {
                    upload_id,
                    sequence: 0,
                    offset: 0,
                    data_base64: String::new(),
                },
                1,
                now + NOTE_UPLOAD_IDLE_TTL - Duration::from_millis(1),
            )
            .expect_err("empty append");

        assert_eq!(error.reason_code(), "note_upload_chunk_invalid");
        assert_eq!(service.active_uploads(), 0);
        assert_eq!(service.reserved_staged_bytes(), 0);
    }

    #[test]
    fn maximum_public_page_has_stable_tail_pagination_without_duplicates() {
        let mut repository = NotesRepository::open_in_memory().expect("repository");
        for now_ms in 1..=65 {
            repository
                .create(
                    CreateNote {
                        title: format!("note-{now_ms:02}"),
                        body_markdown: String::new(),
                        diary_date: None,
                        tags: Vec::new(),
                        status: NoteStatus::Active,
                        pinned: false,
                    },
                    now_ms,
                )
                .expect("create note");
        }
        let mut service = NotesService::new(repository);
        let now = Instant::now();

        let NotesOutput::Page(first) = service
            .execute(
                NotesCommand::List {
                    query: PublicQuery::default(),
                },
                66,
                now,
            )
            .expect("first page")
        else {
            panic!("page output");
        };
        assert_eq!(first.notes.len(), 64);
        assert!(first.has_more);
        assert_eq!(first.next_offset, Some(64));

        let tail_query = PublicQuery {
            offset: 64,
            ..PublicQuery::default()
        };
        let NotesOutput::Page(tail) = service
            .execute(NotesCommand::List { query: tail_query }, 67, now)
            .expect("tail page")
        else {
            panic!("page output");
        };
        assert_eq!(tail.notes.len(), 1);
        assert!(!tail.has_more);
        assert_eq!(tail.next_offset, None);
        assert!(first.notes.iter().all(|note| note.id != tail.notes[0].id));

        let empty_query = PublicQuery {
            offset: 65,
            ..PublicQuery::default()
        };
        let NotesOutput::Page(empty) = service
            .execute(NotesCommand::List { query: empty_query }, 68, now)
            .expect("empty tail")
        else {
            panic!("page output");
        };
        assert!(empty.notes.is_empty());
        assert!(!empty.has_more);
        assert_eq!(empty.next_offset, None);
    }

    #[test]
    fn json_export_stops_at_the_transport_budget() {
        let mut repository = NotesRepository::open_in_memory().expect("repository");
        let body = "\0".repeat(MAX_NOTE_BODY_BYTES);
        for now_ms in [1, 2] {
            repository
                .create(
                    CreateNote {
                        title: format!("large-{now_ms}"),
                        body_markdown: body.clone(),
                        diary_date: None,
                        tags: Vec::new(),
                        status: NoteStatus::Draft,
                        pinned: false,
                    },
                    now_ms,
                )
                .expect("large note");
        }
        let mut service = NotesService::new(repository);

        let error = service
            .execute(
                NotesCommand::Export {
                    query: PublicQuery::default(),
                    format: NoteExportFormat::Json,
                },
                3,
                Instant::now(),
            )
            .expect_err("bounded export");

        assert_eq!(error.reason_code(), "note_export_too_large");
    }
}
