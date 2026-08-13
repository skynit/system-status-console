use nix::{
    sys::socket::{getsockopt, sockopt::PeerCredentials},
    unistd::Uid,
};
use std::io;
use thiserror::Error;
use tokio::net::UnixStream;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub expected_uid: u32,
}

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("could not read peer credentials: {0}")]
    Credentials(#[source] nix::Error),
    #[error("peer uid {actual} does not match effective uid {expected}")]
    WrongUid { actual: u32, expected: u32 },
    #[error("peer credential support is unavailable on this platform")]
    UnsupportedPlatform,
}

pub fn verify_peer_uid(stream: &UnixStream) -> Result<PeerIdentity, PeerError> {
    #[cfg(target_os = "linux")]
    {
        let credentials = getsockopt(stream, PeerCredentials).map_err(PeerError::Credentials)?;
        verify_peer_uid_value(credentials.uid(), Uid::effective().as_raw())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(PeerError::UnsupportedPlatform)
    }
}

fn verify_peer_uid_value(uid: u32, expected_uid: u32) -> Result<PeerIdentity, PeerError> {
    if uid != expected_uid {
        return Err(PeerError::WrongUid {
            actual: uid,
            expected: expected_uid,
        });
    }
    Ok(PeerIdentity { uid, expected_uid })
}

impl From<PeerError> for io::Error {
    fn from(error: PeerError) -> Self {
        io::Error::new(io::ErrorKind::PermissionDenied, error)
    }
}

#[cfg(test)]
mod tests {
    use super::{PeerError, PeerIdentity, verify_peer_uid_value};

    #[test]
    fn peer_uid_validation_accepts_injected_effective_uid() {
        let identity = verify_peer_uid_value(1000, 1000).expect("matching uid");
        assert_eq!(
            identity,
            PeerIdentity {
                uid: 1000,
                expected_uid: 1000,
            }
        );
        assert!(matches!(
            verify_peer_uid_value(1001, 1000),
            Err(PeerError::WrongUid {
                actual: 1001,
                expected: 1000
            })
        ));
    }
}
