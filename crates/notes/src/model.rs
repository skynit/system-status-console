use serde::{Deserialize, Serialize};

pub const MAX_TITLE_CHARS: usize = 512;
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TAGS: usize = 64;
pub const MAX_TAG_CHARS: usize = 64;
pub const MAX_QUERY_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    Draft,
    Active,
    Completed,
    Archived,
}

impl NoteStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body_markdown: String,
    pub diary_date: Option<String>,
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub pinned: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
    pub revision: u64,
}

impl Note {
    pub fn checklist(&self) -> Vec<ChecklistItem> {
        checklist_items(&self.body_markdown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteDraft {
    pub title: String,
    pub body_markdown: String,
    pub diary_date: Option<String>,
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateNote {
    pub title: String,
    pub body_markdown: String,
    pub diary_date: Option<String>,
    pub tags: Vec<String>,
    pub status: NoteStatus,
    pub pinned: bool,
}

impl From<CreateNote> for NoteDraft {
    fn from(value: CreateNote) -> Self {
        Self {
            title: value.title,
            body_markdown: value.body_markdown,
            diary_date: value.diary_date,
            tags: value.tags,
            status: value.status,
            pinned: value.pinned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveNote {
    pub id: String,
    pub expected_revision: u64,
    pub draft: NoteDraft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteRevision {
    pub note: Note,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeletedFilter {
    #[default]
    Exclude,
    Include,
    Only,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteSort {
    #[default]
    UpdatedDesc,
    CreatedDesc,
    TitleAsc,
    DiaryDateDesc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteQuery {
    pub search: Option<String>,
    pub diary_date_from: Option<String>,
    pub diary_date_to: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<NoteStatus>,
    pub deleted: DeletedFilter,
    pub sort: NoteSort,
    pub limit: u32,
    pub offset: u32,
}

impl Default for NoteQuery {
    fn default() -> Self {
        Self {
            search: None,
            diary_date_from: None,
            diary_date_to: None,
            tags: Vec::new(),
            status: None,
            deleted: DeletedFilter::Exclude,
            sort: NoteSort::UpdatedDesc,
            limit: 100,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub purge_deleted_before_ms: Option<i64>,
    pub prune_revisions_before_ms: Option<i64>,
    pub keep_latest_revisions: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionResult {
    pub purged_notes: u64,
    pub pruned_revisions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub index: usize,
    pub line: usize,
    pub checked: bool,
    pub text: String,
}

pub fn checklist_items(markdown: &str) -> Vec<ChecklistItem> {
    markdown
        .lines()
        .enumerate()
        .filter_map(|(line, source)| {
            checklist_marker(source).map(|(checked, text)| (line, checked, text))
        })
        .enumerate()
        .map(|(index, (line, checked, text))| ChecklistItem {
            index,
            line,
            checked,
            text: text.to_owned(),
        })
        .collect()
}

pub fn set_checklist_item(markdown: &str, item_index: usize, checked: bool) -> Option<String> {
    let target_line = checklist_items(markdown).get(item_index)?.line;
    let mut output = String::with_capacity(markdown.len());
    for (line_index, line) in markdown.split_inclusive('\n').enumerate() {
        if line_index == target_line {
            let marker = checklist_marker_offset(line)?;
            output.push_str(&line[..marker + 1]);
            output.push(if checked { 'x' } else { ' ' });
            output.push_str(&line[marker + 2..]);
        } else {
            output.push_str(line);
        }
    }
    Some(output)
}

fn checklist_marker(line: &str) -> Option<(bool, &str)> {
    let marker = checklist_marker_offset(line)?;
    let checked = matches!(line.as_bytes()[marker + 1], b'x' | b'X');
    Some((checked, line[marker + 3..].trim_start()))
}

fn checklist_marker_offset(line: &str) -> Option<usize> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - trimmed.len();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 5 || !matches!(bytes[0], b'-' | b'*' | b'+') || bytes[1] != b' ' {
        return None;
    }
    if bytes[2] != b'[' || !matches!(bytes[3], b' ' | b'x' | b'X') || bytes[4] != b']' {
        return None;
    }
    Some(indent + 2)
}

pub(crate) fn validate_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[0..4].parse::<u32>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..10].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
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
    day >= 1 && day <= max_day
}
