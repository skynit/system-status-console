use std::fmt;
use std::io;

#[derive(Debug)]
pub enum FtpError {
    Configuration(String),
    Policy(String),
    Protocol(String),
    Cancelled,
    DeadlineExceeded,
    Remote {
        code: Option<u32>,
        failure: FtpFailureKind,
        reason: String,
    },
    Io(io::Error),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FtpFailureKind {
    Transport,
    Trust,
    Authentication,
    PermissionDenied,
    NotFound,
    Timeout,
    Protocol,
}

impl FtpError {
    pub(crate) fn remote(error: &curl::Error, code: Option<u32>) -> Self {
        let failure = if error.is_peer_failed_verification()
            || error.is_ssl_certproblem()
            || error.is_ssl_cacert()
            || error.is_ssl_cacert_badfile()
            || error.is_ssl_issuer_error()
        {
            FtpFailureKind::Trust
        } else if error.is_login_denied() || code == Some(530) {
            FtpFailureKind::Authentication
        } else if error.is_remote_access_denied() {
            FtpFailureKind::PermissionDenied
        } else if error.is_operation_timedout() {
            FtpFailureKind::Timeout
        } else if error.is_quote_error() {
            FtpFailureKind::Protocol
        } else {
            FtpFailureKind::Transport
        };
        Self::Remote {
            code,
            failure,
            reason: error.description().to_owned(),
        }
    }
}

impl fmt::Display for FtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(reason) => write!(formatter, "invalid FTP configuration: {reason}"),
            Self::Policy(reason) => {
                write!(formatter, "FTP policy rejected the operation: {reason}")
            }
            Self::Protocol(reason) => {
                write!(formatter, "FTP protocol verification failed: {reason}")
            }
            Self::Cancelled => formatter.write_str("FTP operation was cancelled"),
            Self::DeadlineExceeded => formatter.write_str("FTP operation deadline elapsed"),
            Self::Remote { code, reason, .. } => match code {
                Some(code) => write!(formatter, "FTP server returned {code}: {reason}"),
                None => write!(formatter, "FTP transport failed: {reason}"),
            },
            Self::Io(error) => write!(formatter, "local filesystem operation failed: {error}"),
        }
    }
}

impl std::error::Error for FtpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FtpError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<curl::Error> for FtpError {
    fn from(error: curl::Error) -> Self {
        Self::remote(&error, None)
    }
}
