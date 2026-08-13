use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Clone, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SafeReason(String);

impl SafeReason {
    pub fn new(value: impl Into<String>) -> Result<Self, SafeReasonError> {
        let value = value.into();
        if value.is_empty() || value.len() > 96 {
            return Err(SafeReasonError::InvalidLength);
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        }) {
            return Err(SafeReasonError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SafeReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SafeReason").field(&self.0).finish()
    }
}

impl fmt::Display for SafeReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SafeReason {
    type Err = SafeReasonError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SafeReasonError {
    #[error("reason code must contain between 1 and 96 bytes")]
    InvalidLength,
    #[error("reason code may contain only lowercase ASCII, digits, dot, dash, and underscore")]
    InvalidCharacter,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorKind {
    Transport,
    Trust,
    Authentication,
    PermissionDenied,
    NotFound,
    Conflict,
    Unsupported,
    RateLimited,
    Timeout,
    RemoteProtocol,
    Cancelled,
    InvalidInput,
    SecretStore,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperation {
    Connect,
    Disconnect,
    List,
    Stat,
    Read,
    Write,
    CreateDirectory,
    Rename,
    Delete,
    Resume,
    ResolveSecret,
    DeleteSecret,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    Never,
    Backoff,
    Reauthenticate,
    UserAction,
}

impl RetryDisposition {
    pub const fn is_retryable(self) -> bool {
        !matches!(self, Self::Never)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteError {
    pub kind: RemoteErrorKind,
    pub operation: RemoteOperation,
    pub reason: SafeReason,
    pub retry: RetryDisposition,
}

impl RemoteError {
    pub fn new(
        kind: RemoteErrorKind,
        operation: RemoteOperation,
        reason: SafeReason,
        retry: RetryDisposition,
    ) -> Self {
        Self {
            kind,
            operation,
            reason,
            retry,
        }
    }

    pub const fn is_retryable(&self) -> bool {
        self.retry.is_retryable()
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "remote {:?} failed during {:?}: {}",
            self.kind, self.operation, self.reason
        )
    }
}

impl std::error::Error for RemoteError {}
