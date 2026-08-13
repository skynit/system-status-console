use std::{error::Error, fmt, path::Path, time::Duration};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter, types::Value,
};
use uuid::Uuid;

use crate::{
    CreateNote, DeletedFilter, Note, NoteDraft, NoteQuery, NoteRevision, NoteSort, NoteStatus,
    RetentionPolicy, RetentionResult, SaveNote, migration,
    model::{
        MAX_BODY_BYTES, MAX_QUERY_LIMIT, MAX_TAG_CHARS, MAX_TAGS, MAX_TITLE_CHARS, validate_date,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteConflict {
    pub expected_revision: u64,
    pub current: Note,
    pub submitted: NoteDraft,
}

pub enum NotesError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    NotFound { id: String },
    Conflict(Box<NoteConflict>),
    Validation { field: &'static str, reason: String },
    UnsupportedSchema { found: u32, supported: u32 },
    MigrationBackup { reason: &'static str },
    CorruptData { field: &'static str, value: String },
}

impl fmt::Debug for NotesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => formatter.debug_tuple("Sql").field(error).finish(),
            Self::Json(error) => formatter.debug_tuple("Json").field(error).finish(),
            Self::NotFound { id } => formatter.debug_struct("NotFound").field("id", id).finish(),
            Self::Conflict(conflict) => formatter
                .debug_struct("Conflict")
                .field("id", &conflict.current.id)
                .field("expected_revision", &conflict.expected_revision)
                .field("current_revision", &conflict.current.revision)
                .finish_non_exhaustive(),
            Self::Validation { field, reason } => formatter
                .debug_struct("Validation")
                .field("field", field)
                .field("reason", reason)
                .finish(),
            Self::UnsupportedSchema { found, supported } => formatter
                .debug_struct("UnsupportedSchema")
                .field("found", found)
                .field("supported", supported)
                .finish(),
            Self::MigrationBackup { reason } => formatter
                .debug_struct("MigrationBackup")
                .field("reason", reason)
                .finish(),
            Self::CorruptData { field, value } => formatter
                .debug_struct("CorruptData")
                .field("field", field)
                .field("value", value)
                .finish(),
        }
    }
}

impl fmt::Display for NotesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "notes database error: {error}"),
            Self::Json(error) => write!(formatter, "notes serialization error: {error}"),
            Self::NotFound { id } => write!(formatter, "note {id} was not found"),
            Self::Conflict(conflict) => write!(
                formatter,
                "note {} revision conflict: expected {}, current {}",
                conflict.current.id, conflict.expected_revision, conflict.current.revision
            ),
            Self::Validation { field, reason } => {
                write!(formatter, "invalid {field}: {reason}")
            }
            Self::UnsupportedSchema { found, supported } => write!(
                formatter,
                "notes schema version {found} is newer than supported version {supported}"
            ),
            Self::MigrationBackup { reason } => {
                write!(formatter, "notes migration backup failed: {reason}")
            }
            Self::CorruptData { field, value } => {
                write!(formatter, "invalid persisted {field}: {value}")
            }
        }
    }
}

impl Error for NotesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for NotesError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<serde_json::Error> for NotesError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Owns the sole mutable SQLite connection used by appd.
///
/// Mutation methods require `&mut self`; callers should keep one instance in
/// the appd persistence owner and expose only typed operations to other layers.
pub struct NotesRepository {
    connection: Connection,
}

impl fmt::Debug for NotesRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotesRepository")
            .finish_non_exhaustive()
    }
}

impl NotesRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NotesError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(path, flags)?;
        Self::from_connection(connection)
    }

    pub fn open_in_memory() -> Result<Self, NotesError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, NotesError> {
        connection.busy_timeout(Duration::from_secs(2))?;
        ensure_database_integrity(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;\nPRAGMA synchronous = FULL;")?;
        migration::ensure_supported(&connection)?;
        migration::migrate(&mut connection)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<u32, NotesError> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn checkpoint(&self) -> Result<(), NotesError> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    pub fn create(&mut self, input: CreateNote, now_ms: i64) -> Result<Note, NotesError> {
        let draft = normalize_draft(input.into())?;
        let note = Note {
            id: Uuid::new_v4().to_string(),
            title: draft.title,
            body_markdown: draft.body_markdown,
            diary_date: draft.diary_date,
            tags: draft.tags,
            status: draft.status,
            pinned: draft.pinned,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            deleted_at_ms: None,
            revision: 1,
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_note(&transaction, &note)?;
        replace_tags(&transaction, &note.id, &note.tags)?;
        insert_revision(&transaction, &note, now_ms)?;
        transaction.commit()?;
        Ok(note)
    }

    pub fn get(&self, id: &str) -> Result<Note, NotesError> {
        load_note(&self.connection, id)
    }

    pub fn save(&mut self, input: SaveNote, now_ms: i64) -> Result<Note, NotesError> {
        let submitted = normalize_draft(input.draft)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_note(&transaction, &input.id)?;
        if current.revision != input.expected_revision {
            return Err(NotesError::Conflict(Box::new(NoteConflict {
                expected_revision: input.expected_revision,
                current,
                submitted,
            })));
        }
        if same_draft(&current, &submitted) {
            return Ok(current);
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| NotesError::Validation {
                field: "revision",
                reason: "revision exhausted".to_owned(),
            })?;
        let saved = Note {
            id: current.id,
            title: submitted.title,
            body_markdown: submitted.body_markdown,
            diary_date: submitted.diary_date,
            tags: submitted.tags,
            status: submitted.status,
            pinned: submitted.pinned,
            created_at_ms: current.created_at_ms,
            updated_at_ms: now_ms.max(current.updated_at_ms),
            deleted_at_ms: current.deleted_at_ms,
            revision,
        };
        update_note(&transaction, &saved, input.expected_revision)?;
        replace_tags(&transaction, &saved.id, &saved.tags)?;
        insert_revision(&transaction, &saved, now_ms)?;
        transaction.commit()?;
        Ok(saved)
    }

    pub fn soft_delete(
        &mut self,
        id: &str,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<Note, NotesError> {
        self.set_deleted(id, expected_revision, Some(now_ms), now_ms)
    }

    pub fn restore(
        &mut self,
        id: &str,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<Note, NotesError> {
        self.set_deleted(id, expected_revision, None, now_ms)
    }

    fn set_deleted(
        &mut self,
        id: &str,
        expected_revision: u64,
        deleted_at_ms: Option<i64>,
        now_ms: i64,
    ) -> Result<Note, NotesError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_note(&transaction, id)?;
        if current.revision != expected_revision {
            return Err(NotesError::Conflict(Box::new(NoteConflict {
                expected_revision,
                submitted: draft_from_note(&current),
                current,
            })));
        }
        if current.deleted_at_ms == deleted_at_ms
            || (deleted_at_ms.is_some() && current.deleted_at_ms.is_some())
        {
            return Ok(current);
        }
        let mut saved = current;
        saved.revision = saved
            .revision
            .checked_add(1)
            .ok_or_else(|| NotesError::Validation {
                field: "revision",
                reason: "revision exhausted".to_owned(),
            })?;
        saved.deleted_at_ms = deleted_at_ms;
        saved.updated_at_ms = now_ms.max(saved.updated_at_ms);
        update_note(&transaction, &saved, expected_revision)?;
        insert_revision(&transaction, &saved, now_ms)?;
        transaction.commit()?;
        Ok(saved)
    }

    pub fn revisions(&self, id: &str) -> Result<Vec<NoteRevision>, NotesError> {
        if !note_exists(&self.connection, id)? {
            return Err(NotesError::NotFound { id: id.to_owned() });
        }
        let mut statement = self.connection.prepare(
            "SELECT note_id, title, body_markdown, diary_date, tags_json, status, pinned,\n                    created_at_ms, updated_at_ms, deleted_at_ms, revision, recorded_at_ms\n             FROM note_revisions WHERE note_id = ?1 ORDER BY revision DESC",
        )?;
        let rows = statement.query_map([id], revision_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn query(&self, query: &NoteQuery) -> Result<Vec<Note>, NotesError> {
        let mut notes = Vec::new();
        self.visit_query(query, |note| {
            notes.push(note);
            Ok(())
        })?;
        Ok(notes)
    }

    pub(crate) fn visit_query(
        &self,
        query: &NoteQuery,
        mut visitor: impl FnMut(Note) -> Result<(), NotesError>,
    ) -> Result<(), NotesError> {
        validate_query(query)?;
        let mut sql = String::from(
            "SELECT n.id, n.title, n.body_markdown, n.diary_date, n.status, n.pinned,\n                    n.created_at_ms, n.updated_at_ms, n.deleted_at_ms, n.revision\n             FROM notes n WHERE 1 = 1",
        );
        let mut values = Vec::<Value>::new();
        match query.deleted {
            DeletedFilter::Exclude => sql.push_str(" AND n.deleted_at_ms IS NULL"),
            DeletedFilter::Only => sql.push_str(" AND n.deleted_at_ms IS NOT NULL"),
            DeletedFilter::Include => {}
        }
        if let Some(status) = query.status {
            sql.push_str(" AND n.status = ?");
            values.push(Value::Text(status.as_str().to_owned()));
        }
        if let Some(from) = &query.diary_date_from {
            sql.push_str(" AND n.diary_date >= ?");
            values.push(Value::Text(from.clone()));
        }
        if let Some(to) = &query.diary_date_to {
            sql.push_str(" AND n.diary_date <= ?");
            values.push(Value::Text(to.clone()));
        }
        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(
                " AND (instr(lower(n.title), lower(?)) > 0\n                     OR instr(lower(n.body_markdown), lower(?)) > 0\n                     OR EXISTS (SELECT 1 FROM note_tags st\n                                WHERE st.note_id = n.id\n                                  AND instr(lower(st.display_tag), lower(?)) > 0))",
            );
            for _ in 0..3 {
                values.push(Value::Text(search.to_owned()));
            }
        }
        for tag in &query.tags {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM note_tags ft\n                             WHERE ft.note_id = n.id AND ft.tag_key = ?)",
            );
            values.push(Value::Text(normalize_tag_key(tag)?));
        }
        sql.push_str(match query.sort {
            NoteSort::UpdatedDesc => " ORDER BY n.pinned DESC, n.updated_at_ms DESC, n.id ASC",
            NoteSort::CreatedDesc => " ORDER BY n.pinned DESC, n.created_at_ms DESC, n.id ASC",
            NoteSort::TitleAsc => " ORDER BY n.pinned DESC, lower(n.title) ASC, n.id ASC",
            NoteSort::DiaryDateDesc => {
                " ORDER BY n.pinned DESC, n.diary_date IS NULL ASC, n.diary_date DESC, n.updated_at_ms DESC, n.id ASC"
            }
        });
        sql.push_str(" LIMIT ? OFFSET ?");
        values.push(Value::Integer(i64::from(query.limit)));
        values.push(Value::Integer(i64::from(query.offset)));

        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(values.iter()))?;
        while let Some(row) = rows.next()? {
            let mut note = note_from_row(row)?;
            note.tags = load_tags(&self.connection, &note.id)?;
            visitor(note)?;
        }
        Ok(())
    }

    pub fn apply_retention(
        &mut self,
        policy: RetentionPolicy,
    ) -> Result<RetentionResult, NotesError> {
        if policy.keep_latest_revisions == 0 {
            return Err(NotesError::Validation {
                field: "keep_latest_revisions",
                reason: "must be at least 1".to_owned(),
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let purged_notes = if let Some(before) = policy.purge_deleted_before_ms {
            transaction.execute(
                "DELETE FROM notes WHERE deleted_at_ms IS NOT NULL AND deleted_at_ms <= ?1",
                [before],
            )? as u64
        } else {
            0
        };
        let pruned_revisions = if let Some(before) = policy.prune_revisions_before_ms {
            transaction.execute(
                "DELETE FROM note_revisions\n                 WHERE recorded_at_ms <= ?1\n                   AND (note_id, revision) IN (\n                       SELECT note_id, revision FROM (\n                           SELECT note_id, revision,\n                                  row_number() OVER (PARTITION BY note_id ORDER BY revision DESC) AS position\n                           FROM note_revisions\n                       ) WHERE position > ?2\n                   )",
                params![before, policy.keep_latest_revisions],
            )? as u64
        } else {
            0
        };
        transaction.commit()?;
        Ok(RetentionResult {
            purged_notes,
            pruned_revisions,
        })
    }
}

fn ensure_database_integrity(connection: &Connection) -> Result<(), NotesError> {
    let result = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(|_| NotesError::CorruptData {
            field: "database",
            value: "quick_check_failed".to_owned(),
        })?;
    if result != "ok" {
        return Err(NotesError::CorruptData {
            field: "database",
            value: "quick_check_failed".to_owned(),
        });
    }
    Ok(())
}

fn normalize_draft(mut draft: NoteDraft) -> Result<NoteDraft, NotesError> {
    if draft.title.chars().count() > MAX_TITLE_CHARS {
        return Err(validation(
            "title",
            format!("must not exceed {MAX_TITLE_CHARS} characters"),
        ));
    }
    if draft.body_markdown.len() > MAX_BODY_BYTES {
        return Err(validation(
            "body_markdown",
            format!("must not exceed {MAX_BODY_BYTES} bytes"),
        ));
    }
    if let Some(date) = draft.diary_date.as_deref()
        && !validate_date(date)
    {
        return Err(validation("diary_date", "must be a valid YYYY-MM-DD date"));
    }
    if draft.tags.len() > MAX_TAGS {
        return Err(validation(
            "tags",
            format!("must not contain more than {MAX_TAGS} tags"),
        ));
    }
    let mut tags = Vec::<(String, String)>::new();
    for tag in draft.tags {
        let display = tag.trim();
        if display.is_empty() {
            return Err(validation("tags", "tags must not be empty"));
        }
        if display.chars().count() > MAX_TAG_CHARS {
            return Err(validation(
                "tags",
                format!("each tag must not exceed {MAX_TAG_CHARS} characters"),
            ));
        }
        let key = normalize_tag_key(display)?;
        if !tags.iter().any(|(existing, _)| existing == &key) {
            tags.push((key, display.to_owned()));
        }
    }
    tags.sort_by(|left, right| left.0.cmp(&right.0));
    draft.tags = tags.into_iter().map(|(_, display)| display).collect();
    Ok(draft)
}

fn normalize_tag_key(tag: &str) -> Result<String, NotesError> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(validation("tags", "tags must not be empty"));
    }
    Ok(tag.to_lowercase())
}

fn validate_query(query: &NoteQuery) -> Result<(), NotesError> {
    if query.limit == 0 || query.limit > MAX_QUERY_LIMIT {
        return Err(validation(
            "limit",
            format!("must be between 1 and {MAX_QUERY_LIMIT}"),
        ));
    }
    for (field, date) in [
        ("diary_date_from", query.diary_date_from.as_deref()),
        ("diary_date_to", query.diary_date_to.as_deref()),
    ] {
        if let Some(date) = date
            && !validate_date(date)
        {
            return Err(validation(field, "must be a valid YYYY-MM-DD date"));
        }
    }
    if let (Some(from), Some(to)) = (&query.diary_date_from, &query.diary_date_to)
        && from > to
    {
        return Err(validation("diary_date", "from must not be after to"));
    }
    if query
        .search
        .as_ref()
        .is_some_and(|value| value.chars().count() > 512)
    {
        return Err(validation("search", "must not exceed 512 characters"));
    }
    if query.tags.len() > MAX_TAGS {
        return Err(validation(
            "tags",
            format!("must not contain more than {MAX_TAGS} tags"),
        ));
    }
    Ok(())
}

fn insert_note(transaction: &Transaction<'_>, note: &Note) -> Result<(), NotesError> {
    let revision = revision_to_i64(note.revision)?;
    transaction.execute(
        "INSERT INTO notes\n         (id, title, body_markdown, diary_date, status, pinned, created_at_ms, updated_at_ms, deleted_at_ms, revision)\n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            note.id,
            note.title,
            note.body_markdown,
            note.diary_date,
            note.status.as_str(),
            note.pinned,
            note.created_at_ms,
            note.updated_at_ms,
            note.deleted_at_ms,
            revision,
        ],
    )?;
    Ok(())
}

fn update_note(
    transaction: &Transaction<'_>,
    note: &Note,
    expected_revision: u64,
) -> Result<(), NotesError> {
    let revision = revision_to_i64(note.revision)?;
    let expected_revision = revision_to_i64(expected_revision)?;
    let changed = transaction.execute(
        "UPDATE notes SET\n            title = ?2, body_markdown = ?3, diary_date = ?4, status = ?5, pinned = ?6,\n            updated_at_ms = ?7, deleted_at_ms = ?8, revision = ?9\n         WHERE id = ?1 AND revision = ?10",
        params![
            note.id,
            note.title,
            note.body_markdown,
            note.diary_date,
            note.status.as_str(),
            note.pinned,
            note.updated_at_ms,
            note.deleted_at_ms,
            revision,
            expected_revision,
        ],
    )?;
    if changed != 1 {
        return Err(NotesError::Sql(rusqlite::Error::ExecuteReturnedResults));
    }
    Ok(())
}

fn replace_tags(
    transaction: &Transaction<'_>,
    note_id: &str,
    tags: &[String],
) -> Result<(), NotesError> {
    transaction.execute("DELETE FROM note_tags WHERE note_id = ?1", [note_id])?;
    for display in tags {
        transaction.execute(
            "INSERT INTO note_tags (note_id, tag_key, display_tag) VALUES (?1, ?2, ?3)",
            params![note_id, normalize_tag_key(display)?, display],
        )?;
    }
    Ok(())
}

fn insert_revision(
    transaction: &Transaction<'_>,
    note: &Note,
    recorded_at_ms: i64,
) -> Result<(), NotesError> {
    let tags_json = serde_json::to_string(&note.tags)?;
    let revision = revision_to_i64(note.revision)?;
    transaction.execute(
        "INSERT INTO note_revisions\n         (note_id, revision, title, body_markdown, diary_date, tags_json, status, pinned,\n          created_at_ms, updated_at_ms, deleted_at_ms, recorded_at_ms)\n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            note.id,
            revision,
            note.title,
            note.body_markdown,
            note.diary_date,
            tags_json,
            note.status.as_str(),
            note.pinned,
            note.created_at_ms,
            note.updated_at_ms,
            note.deleted_at_ms,
            recorded_at_ms,
        ],
    )?;
    Ok(())
}

fn load_note(connection: &Connection, id: &str) -> Result<Note, NotesError> {
    let mut note = connection
        .query_row(
            "SELECT id, title, body_markdown, diary_date, status, pinned,\n                    created_at_ms, updated_at_ms, deleted_at_ms, revision\n             FROM notes WHERE id = ?1",
            [id],
            note_from_row,
        )
        .optional()?
        .ok_or_else(|| NotesError::NotFound { id: id.to_owned() })?;
    note.tags = load_tags(connection, id)?;
    Ok(note)
}

fn load_tags(connection: &Connection, id: &str) -> Result<Vec<String>, NotesError> {
    let mut statement = connection
        .prepare("SELECT display_tag FROM note_tags WHERE note_id = ?1 ORDER BY tag_key ASC")?;
    let tags = statement.query_map([id], |row| row.get(0))?;
    tags.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn note_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    let status: String = row.get(4)?;
    let revision: i64 = row.get(9)?;
    Ok(Note {
        id: row.get(0)?,
        title: row.get(1)?,
        body_markdown: row.get(2)?,
        diary_date: row.get(3)?,
        tags: Vec::new(),
        status: NoteStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(InvalidPersistedValue(status)),
            )
        })?,
        pinned: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        deleted_at_ms: row.get(8)?,
        revision: u64::try_from(revision).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
    })
}

fn revision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRevision> {
    let tags_json: String = row.get(4)?;
    let status: String = row.get(5)?;
    let revision: i64 = row.get(10)?;
    Ok(NoteRevision {
        note: Note {
            id: row.get(0)?,
            title: row.get(1)?,
            body_markdown: row.get(2)?,
            diary_date: row.get(3)?,
            tags: serde_json::from_str(&tags_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            status: NoteStatus::parse(&status).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(InvalidPersistedValue(status)),
                )
            })?,
            pinned: row.get(6)?,
            created_at_ms: row.get(7)?,
            updated_at_ms: row.get(8)?,
            deleted_at_ms: row.get(9)?,
            revision: u64::try_from(revision).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
        },
        recorded_at_ms: row.get(11)?,
    })
}

fn note_exists(connection: &Connection, id: &str) -> Result<bool, NotesError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

fn same_draft(note: &Note, draft: &NoteDraft) -> bool {
    note.title == draft.title
        && note.body_markdown == draft.body_markdown
        && note.diary_date == draft.diary_date
        && note.tags == draft.tags
        && note.status == draft.status
        && note.pinned == draft.pinned
}

fn draft_from_note(note: &Note) -> NoteDraft {
    NoteDraft {
        title: note.title.clone(),
        body_markdown: note.body_markdown.clone(),
        diary_date: note.diary_date.clone(),
        tags: note.tags.clone(),
        status: note.status,
        pinned: note.pinned,
    }
}

fn validation(field: &'static str, reason: impl Into<String>) -> NotesError {
    NotesError::Validation {
        field,
        reason: reason.into(),
    }
}

fn revision_to_i64(revision: u64) -> Result<i64, NotesError> {
    i64::try_from(revision).map_err(|_| validation("revision", "must fit SQLite INTEGER"))
}

#[derive(Debug)]
struct InvalidPersistedValue(String);

impl fmt::Display for InvalidPersistedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid persisted value: {}", self.0)
    }
}

impl Error for InvalidPersistedValue {}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn disk_repository_refuses_a_symlink_database_path() {
        let directory = tempdir().expect("temporary directory");
        let target = directory.path().join("target.sqlite3");
        drop(NotesRepository::open(&target).expect("target repository"));
        let link = directory.path().join("notes.sqlite3");
        symlink(&target, &link).expect("database symlink");

        let error = NotesRepository::open(&link).expect_err("nofollow database open");
        assert!(matches!(error, NotesError::Sql(_)));
    }
}
