use crate::ProfileId;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_TERMINAL_IPC_BYTES: usize = 45_056;
pub const MAX_TERMINAL_DATA_BASE64_BYTES: usize = 60_076;
pub const MAX_TERMINAL_SIZE: u16 = 1_000;
pub const MAX_TERMINAL_PIXEL_DIMENSION: u16 = 32_767;
pub const MAX_TERMINAL_TRANSCRIPT_BYTES: u32 = 64 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TerminalSessionId(Uuid);

impl TerminalSessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for TerminalSessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSize {
    pub rows: u16,
    pub columns: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl TerminalSize {
    pub fn validate(self) -> Result<(), TerminalContractError> {
        if self.rows == 0
            || self.rows > MAX_TERMINAL_SIZE
            || self.columns == 0
            || self.columns > MAX_TERMINAL_SIZE
            || self.pixel_width > MAX_TERMINAL_PIXEL_DIMENSION
            || self.pixel_height > MAX_TERMINAL_PIXEL_DIMENSION
        {
            return Err(TerminalContractError::InvalidSize);
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TerminalData(String);

impl TerminalData {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TerminalContractError> {
        if bytes.is_empty() || bytes.len() > MAX_TERMINAL_IPC_BYTES {
            return Err(TerminalContractError::InvalidData);
        }
        Ok(Self(STANDARD.encode(bytes)))
    }

    pub fn decode(&self) -> Result<Vec<u8>, TerminalContractError> {
        if self.0.is_empty() || self.0.len() > MAX_TERMINAL_DATA_BASE64_BYTES {
            return Err(TerminalContractError::InvalidData);
        }
        let bytes = STANDARD
            .decode(&self.0)
            .map_err(|_| TerminalContractError::InvalidData)?;
        if bytes.is_empty() || bytes.len() > MAX_TERMINAL_IPC_BYTES {
            return Err(TerminalContractError::InvalidData);
        }
        Ok(bytes)
    }

    pub fn encoded(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TerminalData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalData")
            .field("encoded_bytes", &self.0.len())
            .field("data", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalCapabilities {
    pub max_output_chunk_bytes: u32,
    pub max_input_chunk_bytes: u32,
    pub max_transcript_bytes: u32,
    pub max_rows: u16,
    pub max_columns: u16,
    pub max_pixel_dimension: u16,
    pub nonblocking_output: bool,
    pub fixed_openssh_program: bool,
}

impl TerminalCapabilities {
    pub fn validate(self) -> Result<(), TerminalContractError> {
        if self.max_output_chunk_bytes == 0
            || self.max_output_chunk_bytes as usize > MAX_TERMINAL_IPC_BYTES
            || self.max_input_chunk_bytes == 0
            || self.max_input_chunk_bytes as usize > MAX_TERMINAL_IPC_BYTES
            || self.max_transcript_bytes == 0
            || self.max_transcript_bytes > MAX_TERMINAL_TRANSCRIPT_BYTES
            || self.max_rows == 0
            || self.max_rows > MAX_TERMINAL_SIZE
            || self.max_columns == 0
            || self.max_columns > MAX_TERMINAL_SIZE
            || self.max_pixel_dimension > MAX_TERMINAL_PIXEL_DIMENSION
            || !self.nonblocking_output
            || !self.fixed_openssh_program
        {
            return Err(TerminalContractError::InvalidCapabilities);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDisconnectReason {
    HostKeyChanged,
    HostKeyRevoked,
    HostKeyUnknown,
    AuthenticationFailed,
    NetworkUnreachable,
    ConnectionLost,
    OpenSshFailure,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum TerminalState {
    Running,
    Exited { code: Option<i32> },
    Disconnected { reason: TerminalDisconnectReason },
    ClosedByClient,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalStatus {
    pub state: TerminalState,
    pub transcript_retained_bytes: u32,
    pub transcript_dropped_bytes: u64,
}

impl TerminalStatus {
    pub fn validate(&self) -> Result<(), TerminalContractError> {
        if self.transcript_retained_bytes > MAX_TERMINAL_TRANSCRIPT_BYTES {
            return Err(TerminalContractError::InvalidResult);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum TerminalRead {
    Pending,
    Data(TerminalData),
    EndOfStream,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TerminalCommand {
    Open {
        profile_id: ProfileId,
        size: TerminalSize,
        accept_new_host_key: bool,
    },
    Read {
        session_id: TerminalSessionId,
        max_bytes: u32,
    },
    Stream {
        session_id: TerminalSessionId,
        max_bytes: u32,
    },
    Write {
        session_id: TerminalSessionId,
        data: TerminalData,
    },
    Resize {
        session_id: TerminalSessionId,
        size: TerminalSize,
    },
    Poll {
        session_id: TerminalSessionId,
    },
    Close {
        session_id: TerminalSessionId,
    },
}

impl TerminalCommand {
    pub fn validate(&self) -> Result<(), TerminalContractError> {
        match self {
            Self::Open { size, .. } | Self::Resize { size, .. } => size.validate(),
            Self::Read { max_bytes, .. } | Self::Stream { max_bytes, .. }
                if *max_bytes > 0 && *max_bytes as usize <= MAX_TERMINAL_IPC_BYTES =>
            {
                Ok(())
            }
            Self::Read { .. } | Self::Stream { .. } => Err(TerminalContractError::InvalidReadLimit),
            Self::Write { data, .. } => data.decode().map(|_| ()),
            Self::Poll { .. } | Self::Close { .. } => Ok(()),
        }
    }

    pub const fn session_id(&self) -> Option<TerminalSessionId> {
        match self {
            Self::Open { .. } => None,
            Self::Read { session_id, .. }
            | Self::Stream { session_id, .. }
            | Self::Write { session_id, .. }
            | Self::Resize { session_id, .. }
            | Self::Poll { session_id }
            | Self::Close { session_id } => Some(*session_id),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum TerminalResult {
    Opened {
        session_id: TerminalSessionId,
        capabilities: TerminalCapabilities,
        status: TerminalStatus,
    },
    Read {
        session_id: TerminalSessionId,
        output: TerminalRead,
    },
    Wrote {
        session_id: TerminalSessionId,
        accepted_bytes: u32,
    },
    Resized {
        session_id: TerminalSessionId,
    },
    Status {
        session_id: TerminalSessionId,
        status: TerminalStatus,
    },
    Closed {
        session_id: TerminalSessionId,
        status: TerminalStatus,
    },
}

impl TerminalResult {
    pub fn validate_for(&self, command: &TerminalCommand) -> Result<(), TerminalContractError> {
        match (self, command) {
            (
                Self::Opened {
                    session_id,
                    capabilities,
                    status,
                },
                TerminalCommand::Open { .. },
            ) if !session_id.as_uuid().is_nil()
                && capabilities.validate().is_ok()
                && status.validate().is_ok()
                && matches!(status.state, TerminalState::Running) =>
            {
                Ok(())
            }
            (
                Self::Read { session_id, output },
                TerminalCommand::Read {
                    session_id: expected,
                    max_bytes,
                },
            ) if session_id == expected
                && match output {
                    TerminalRead::Data(data) => data
                        .decode()
                        .is_ok_and(|bytes| bytes.len() <= *max_bytes as usize),
                    TerminalRead::Pending | TerminalRead::EndOfStream => true,
                } =>
            {
                Ok(())
            }
            (
                Self::Wrote {
                    session_id,
                    accepted_bytes,
                },
                TerminalCommand::Write {
                    session_id: expected,
                    data,
                },
            ) if session_id == expected
                && data
                    .decode()
                    .is_ok_and(|bytes| bytes.len() == *accepted_bytes as usize) =>
            {
                Ok(())
            }
            (
                Self::Resized { session_id },
                TerminalCommand::Resize {
                    session_id: expected,
                    ..
                },
            ) if session_id == expected => Ok(()),
            (
                Self::Status { session_id, status },
                TerminalCommand::Poll {
                    session_id: expected,
                },
            ) if session_id == expected && status.validate().is_ok() => Ok(()),
            (
                Self::Closed { session_id, status },
                TerminalCommand::Close {
                    session_id: expected,
                },
            ) if session_id == expected
                && status.validate().is_ok()
                && matches!(status.state, TerminalState::ClosedByClient) =>
            {
                Ok(())
            }
            _ => Err(TerminalContractError::InvalidResult),
        }
    }
}

#[derive(Debug, Clone, Copy, Error, Eq, PartialEq)]
pub enum TerminalContractError {
    #[error("terminal size is outside the hard bounds")]
    InvalidSize,
    #[error("terminal read limit is outside the hard bounds")]
    InvalidReadLimit,
    #[error("terminal data is empty, malformed, or exceeds the hard bound")]
    InvalidData,
    #[error("terminal capabilities are inconsistent with the public contract")]
    InvalidCapabilities,
    #[error("terminal result does not match its command")]
    InvalidResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_data_is_bounded_and_debug_redacted() {
        let data = TerminalData::from_bytes(b"secret-looking-terminal-input").expect("data");
        assert_eq!(
            data.decode().expect("decode"),
            b"secret-looking-terminal-input"
        );
        let debug = format!("{data:?}");
        assert!(!debug.contains("secret-looking"));
        assert!(TerminalData::from_bytes(&vec![0; MAX_TERMINAL_IPC_BYTES + 1]).is_err());
    }

    #[test]
    fn command_and_result_identity_are_exact() {
        let session_id = TerminalSessionId::from_uuid(Uuid::from_u128(1));
        let command = TerminalCommand::Write {
            session_id,
            data: TerminalData::from_bytes(b"input").expect("data"),
        };
        assert!(command.validate().is_ok());
        assert!(
            TerminalResult::Wrote {
                session_id,
                accepted_bytes: 5,
            }
            .validate_for(&command)
            .is_ok()
        );
    }
}
