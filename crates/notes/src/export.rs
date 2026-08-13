use std::io::{self, Write};

use crate::{Note, NotesError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}

const EXPORT_SCHEMA: &str = "localdesk.notes.export.v1";
const EXPORT_LIMIT_FIELD: &str = "export";
const EXPORT_LIMIT_REASON: &str = "serialized export exceeds transport budget";

pub fn export_note(note: &Note, format: ExportFormat) -> Result<String, NotesError> {
    export_notes(std::slice::from_ref(note), format)
}

pub fn export_notes(notes: &[Note], format: ExportFormat) -> Result<String, NotesError> {
    let mut exporter = BoundedExporter::new(format, usize::MAX)?;
    for note in notes {
        exporter.push(note)?;
    }
    exporter.finish()
}

pub(crate) struct BoundedExporter {
    format: ExportFormat,
    max_bytes: usize,
    note_count: usize,
    content: Vec<u8>,
}

impl BoundedExporter {
    pub(crate) fn new(format: ExportFormat, max_bytes: usize) -> Result<Self, NotesError> {
        let mut exporter = Self {
            format,
            max_bytes,
            note_count: 0,
            content: Vec::new(),
        };
        if format == ExportFormat::Json {
            exporter.append("{\"schema\":\"")?;
            exporter.append(EXPORT_SCHEMA)?;
            exporter.append("\",\"notes\":[")?;
        }
        Ok(exporter)
    }

    pub(crate) fn push(&mut self, note: &Note) -> Result<(), NotesError> {
        let checkpoint = self.content.len();
        let separator = match (self.format, self.note_count) {
            (_, 0) => "",
            (ExportFormat::Json, _) => ",",
            (ExportFormat::Markdown, _) => "\n\n---\n\n",
        };
        self.append(separator)?;
        let result = match self.format {
            ExportFormat::Json => self.push_json(note),
            ExportFormat::Markdown => self.push_markdown(note),
        };
        if let Err(error) = result {
            self.content.truncate(checkpoint);
            return Err(error);
        }
        self.note_count += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<String, NotesError> {
        if self.format == ExportFormat::Json {
            self.append("]}")?;
        }
        String::from_utf8(self.content).map_err(|error| NotesError::CorruptData {
            field: "export",
            value: error.to_string(),
        })
    }

    fn append(&mut self, fragment: &str) -> Result<(), NotesError> {
        let required = self
            .content
            .len()
            .checked_add(fragment.len())
            .ok_or_else(export_too_large)?;
        if required > self.max_bytes {
            return Err(export_too_large());
        }
        self.content.extend_from_slice(fragment.as_bytes());
        Ok(())
    }

    fn push_json(&mut self, note: &Note) -> Result<(), NotesError> {
        let content_limit = self.max_bytes.checked_sub(2).ok_or_else(export_too_large)?;
        let mut writer = BoundedStringWriter {
            content: &mut self.content,
            max_bytes: content_limit,
            overflowed: false,
        };
        match serde_json::to_writer(&mut writer, note) {
            Ok(()) => Ok(()),
            Err(_) if writer.overflowed => Err(export_too_large()),
            Err(error) => Err(NotesError::Json(error)),
        }
    }

    fn push_markdown(&mut self, note: &Note) -> Result<(), NotesError> {
        let title = serde_json::to_string(&note.title)?;
        let tags = serde_json::to_string(&note.tags)?;
        self.append("---\nschema: ")?;
        self.append(EXPORT_SCHEMA)?;
        self.append("\nid: ")?;
        self.append(&note.id)?;
        self.append("\nrevision: ")?;
        self.append(&note.revision.to_string())?;
        self.append("\nstatus: ")?;
        self.append(note.status.as_str())?;
        self.append("\ndiary_date: ")?;
        self.append(note.diary_date.as_deref().unwrap_or("null"))?;
        self.append("\ntags: ")?;
        self.append(&tags)?;
        self.append("\ntitle: ")?;
        self.append(&title)?;
        self.append("\n---\n\n# ")?;
        self.append(&note.title)?;
        self.append("\n\n")?;
        self.append(&note.body_markdown)
    }
}

struct BoundedStringWriter<'a> {
    content: &'a mut Vec<u8>,
    max_bytes: usize,
    overflowed: bool,
}

impl Write for BoundedStringWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(required) = self.content.len().checked_add(bytes.len()) else {
            self.overflowed = true;
            return Err(io::Error::other(EXPORT_LIMIT_REASON));
        };
        if required > self.max_bytes {
            self.overflowed = true;
            return Err(io::Error::other(EXPORT_LIMIT_REASON));
        }
        self.content.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn is_export_too_large(error: &NotesError) -> bool {
    matches!(
        error,
        NotesError::Validation { field, reason }
            if *field == EXPORT_LIMIT_FIELD && reason == EXPORT_LIMIT_REASON
    )
}

fn export_too_large() -> NotesError {
    NotesError::Validation {
        field: EXPORT_LIMIT_FIELD,
        reason: EXPORT_LIMIT_REASON.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoteStatus;

    fn note(body: &str) -> Note {
        Note {
            id: "018f5f7b-f2fa-7ca2-a4f0-0bb7588cc203".to_owned(),
            title: "title".to_owned(),
            body_markdown: body.to_owned(),
            diary_date: None,
            tags: Vec::new(),
            status: NoteStatus::Draft,
            pinned: false,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
            revision: 1,
        }
    }

    #[test]
    fn bounded_export_rejects_before_appending_an_oversized_note() {
        let mut exporter = BoundedExporter::new(ExportFormat::Json, 128).expect("start");
        let error = exporter
            .push(&note(&"\0".repeat(128)))
            .expect_err("export limit");
        assert!(is_export_too_large(&error));
        assert!(exporter.content.len() <= 128);
    }
}
