use crate::{ProfileId, SafeReason};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RemotePath(String);

impl RemotePath {
    pub fn new(value: impl Into<String>) -> Result<Self, RemotePathError> {
        let value = value.into();
        if value.is_empty() || value.len() > 16 * 1024 || value.contains('\0') {
            return Err(RemotePathError::InvalidPath);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum RemotePathError {
    #[error("remote path must be non-empty, bounded, and contain no NUL")]
    InvalidPath,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteBookmark {
    pub profile_id: ProfileId,
    pub path: RemotePath,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    List,
    Stat,
    Read,
    Write,
    CreateDirectory,
    Rename,
    Delete,
    ResumeRead,
    ResumeWrite,
    AtomicRename,
    SetPermissions,
}

pub const FILE_OPERATIONS: &[FileOperation] = &[
    FileOperation::List,
    FileOperation::Stat,
    FileOperation::Read,
    FileOperation::Write,
    FileOperation::CreateDirectory,
    FileOperation::Rename,
    FileOperation::Delete,
    FileOperation::ResumeRead,
    FileOperation::ResumeWrite,
    FileOperation::AtomicRename,
    FileOperation::SetPermissions,
];

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Unsupported(SafeReason),
}

impl CapabilityStatus {
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationCapability {
    pub operation: FileOperation,
    pub status: CapabilityStatus,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityMatrix(Vec<OperationCapability>);

impl CapabilityMatrix {
    pub fn complete(
        capabilities: impl IntoIterator<Item = OperationCapability>,
    ) -> Result<Self, CapabilityMatrixError> {
        let capabilities: Vec<_> = capabilities.into_iter().collect();
        for operation in FILE_OPERATIONS {
            let count = capabilities
                .iter()
                .filter(|capability| capability.operation == *operation)
                .count();
            match count {
                0 => return Err(CapabilityMatrixError::Missing(*operation)),
                1 => {}
                _ => return Err(CapabilityMatrixError::Duplicate(*operation)),
            }
        }
        Ok(Self(capabilities))
    }

    pub fn status(&self, operation: FileOperation) -> &CapabilityStatus {
        &self
            .0
            .iter()
            .find(|capability| capability.operation == operation)
            .expect("complete capability matrix invariant")
            .status
    }

    pub fn iter(&self) -> impl Iterator<Item = &OperationCapability> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum CapabilityMatrixError {
    #[error("capability matrix is missing {0:?}")]
    Missing(FileOperation),
    #[error("capability matrix contains duplicate {0:?}")]
    Duplicate(FileOperation),
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectIdentity {
    pub size_bytes: Option<u64>,
    pub modified_at_unix_ms: Option<i64>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: RemotePath,
    pub kind: EntryKind,
    pub identity: ObjectIdentity,
    pub unix_mode: Option<u32>,
    pub capabilities: CapabilityMatrix,
}
