use crate::{CapabilityMatrix, ProfileId, RemoteEntry, RemotePath, RemoteProtocol, SafeReason};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

pub const MAX_REMOTE_DIRECTORY_PAGE_SIZE: u8 = 2;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteDirectoryQuery {
    pub session_id: SessionId,
    pub path: RemotePath,
    pub offset: u32,
    pub limit: u8,
}

impl RemoteDirectoryQuery {
    pub fn validate(&self) -> Result<(), SessionCommandError> {
        if self.limit == 0 || self.limit > MAX_REMOTE_DIRECTORY_PAGE_SIZE {
            return Err(SessionCommandError::InvalidPageLimit);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteDirectoryPage {
    pub session_id: SessionId,
    pub path: RemotePath,
    pub offset: u32,
    pub entries: Vec<RemoteEntry>,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum RemoteSessionCommand {
    Connect {
        profile_id: ProfileId,
    },
    Disconnect {
        session_id: SessionId,
    },
    List {
        query: RemoteDirectoryQuery,
    },
    Stat {
        session_id: SessionId,
        path: RemotePath,
    },
    CreateDirectory {
        session_id: SessionId,
        path: RemotePath,
    },
    Rename {
        session_id: SessionId,
        from: RemotePath,
        to: RemotePath,
    },
    Delete {
        session_id: SessionId,
        path: RemotePath,
    },
}

impl RemoteSessionCommand {
    pub fn validate(&self) -> Result<(), SessionCommandError> {
        match self {
            Self::List { query } => query.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum RemoteSessionResult {
    Session(RemoteSession),
    DirectoryPage(RemoteDirectoryPage),
    Entry(RemoteEntry),
    Deleted { session_id: SessionId },
    Disconnected { session_id: SessionId },
}

impl RemoteSessionResult {
    pub fn validate_for(&self, command: &RemoteSessionCommand) -> Result<(), SessionCommandError> {
        match (self, command) {
            (Self::Session(session), RemoteSessionCommand::Connect { profile_id })
                if session.profile_id == *profile_id =>
            {
                Ok(())
            }
            (
                Self::Disconnected { session_id },
                RemoteSessionCommand::Disconnect {
                    session_id: requested,
                },
            ) if session_id == requested => Ok(()),
            (Self::DirectoryPage(page), RemoteSessionCommand::List { query }) => {
                query.validate()?;
                if page.session_id != query.session_id
                    || page.path != query.path
                    || page.offset != query.offset
                    || page.entries.len() > usize::from(query.limit)
                    || page.next_offset.is_some_and(|next| {
                        next != query
                            .offset
                            .saturating_add(u32::try_from(page.entries.len()).unwrap_or(u32::MAX))
                    })
                {
                    return Err(SessionCommandError::InvalidResult);
                }
                Ok(())
            }
            (Self::Entry(entry), RemoteSessionCommand::Stat { path, .. })
            | (Self::Entry(entry), RemoteSessionCommand::CreateDirectory { path, .. })
                if entry.path == *path =>
            {
                Ok(())
            }
            (Self::Entry(entry), RemoteSessionCommand::Rename { to, .. }) if entry.path == *to => {
                Ok(())
            }
            (
                Self::Deleted { session_id },
                RemoteSessionCommand::Delete {
                    session_id: requested,
                    ..
                },
            ) if session_id == requested => Ok(()),
            _ => Err(SessionCommandError::InvalidResult),
        }
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum SessionCommandError {
    #[error("remote directory page limit is outside the hard bound")]
    InvalidPageLimit,
    #[error("remote session result does not match its request")]
    InvalidResult,
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum AdapterAvailability {
    Healthy,
    Degraded(SafeReason),
    Unsupported(SafeReason),
    Unreachable(SafeReason),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStateKind {
    Disconnected,
    Connecting,
    Authenticating,
    Ready,
    Degraded,
    Reconnecting,
    Closing,
    Unsupported,
    Unreachable,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Authenticating,
    Ready,
    Degraded { reason: SafeReason },
    Reconnecting { attempt: u32 },
    Closing,
    Unsupported { reason: SafeReason },
    Unreachable { reason: SafeReason },
}

impl ConnectionState {
    pub const fn kind(&self) -> ConnectionStateKind {
        match self {
            Self::Disconnected => ConnectionStateKind::Disconnected,
            Self::Connecting => ConnectionStateKind::Connecting,
            Self::Authenticating => ConnectionStateKind::Authenticating,
            Self::Ready => ConnectionStateKind::Ready,
            Self::Degraded { .. } => ConnectionStateKind::Degraded,
            Self::Reconnecting { .. } => ConnectionStateKind::Reconnecting,
            Self::Closing => ConnectionStateKind::Closing,
            Self::Unsupported { .. } => ConnectionStateKind::Unsupported,
            Self::Unreachable { .. } => ConnectionStateKind::Unreachable,
        }
    }

    pub const fn can_transition_to(&self, next: &Self) -> bool {
        let current = self.kind();
        let next = next.kind();
        if current as u8 == next as u8 {
            return true;
        }
        matches!(
            (current, next),
            (
                ConnectionStateKind::Disconnected,
                ConnectionStateKind::Connecting
            ) | (
                ConnectionStateKind::Connecting,
                ConnectionStateKind::Authenticating
            ) | (ConnectionStateKind::Connecting, ConnectionStateKind::Ready)
                | (
                    ConnectionStateKind::Connecting,
                    ConnectionStateKind::Degraded
                )
                | (
                    ConnectionStateKind::Connecting,
                    ConnectionStateKind::Unsupported
                )
                | (
                    ConnectionStateKind::Connecting,
                    ConnectionStateKind::Unreachable
                )
                | (
                    ConnectionStateKind::Authenticating,
                    ConnectionStateKind::Ready
                )
                | (
                    ConnectionStateKind::Authenticating,
                    ConnectionStateKind::Degraded
                )
                | (
                    ConnectionStateKind::Authenticating,
                    ConnectionStateKind::Unreachable
                )
                | (ConnectionStateKind::Ready, ConnectionStateKind::Degraded)
                | (
                    ConnectionStateKind::Ready,
                    ConnectionStateKind::Reconnecting
                )
                | (ConnectionStateKind::Ready, ConnectionStateKind::Closing)
                | (ConnectionStateKind::Degraded, ConnectionStateKind::Ready)
                | (
                    ConnectionStateKind::Degraded,
                    ConnectionStateKind::Reconnecting
                )
                | (ConnectionStateKind::Degraded, ConnectionStateKind::Closing)
                | (
                    ConnectionStateKind::Reconnecting,
                    ConnectionStateKind::Connecting
                )
                | (
                    ConnectionStateKind::Reconnecting,
                    ConnectionStateKind::Ready
                )
                | (
                    ConnectionStateKind::Reconnecting,
                    ConnectionStateKind::Degraded
                )
                | (
                    ConnectionStateKind::Reconnecting,
                    ConnectionStateKind::Unreachable
                )
                | (
                    ConnectionStateKind::Unreachable,
                    ConnectionStateKind::Reconnecting
                )
                | (_, ConnectionStateKind::Closing)
                | (
                    ConnectionStateKind::Closing,
                    ConnectionStateKind::Disconnected
                )
                | (
                    ConnectionStateKind::Unsupported,
                    ConnectionStateKind::Disconnected
                )
                | (
                    ConnectionStateKind::Unreachable,
                    ConnectionStateKind::Disconnected
                )
        )
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSession {
    pub id: SessionId,
    pub profile_id: ProfileId,
    pub protocol: RemoteProtocol,
    pub state: ConnectionState,
    pub capabilities: CapabilityMatrix,
    pub opened_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl RemoteSession {
    pub fn transition(
        &mut self,
        next: ConnectionState,
        at_unix_ms: i64,
    ) -> Result<bool, SessionTransitionError> {
        if at_unix_ms < self.updated_at_unix_ms {
            return Err(SessionTransitionError::TimestampRegressed);
        }
        if !self.state.can_transition_to(&next) {
            return Err(SessionTransitionError::InvalidTransition {
                from: self.state.kind(),
                to: next.kind(),
            });
        }
        if self.state == next {
            return Ok(false);
        }
        self.state = next;
        self.updated_at_unix_ms = at_unix_ms;
        Ok(true)
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum SessionTransitionError {
    #[error("connection state cannot transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: ConnectionStateKind,
        to: ConnectionStateKind,
    },
    #[error("session timestamp moved backwards")]
    TimestampRegressed,
}
