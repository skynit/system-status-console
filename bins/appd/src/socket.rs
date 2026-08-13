use localdesk_ipc::FRAME_IDLE_TIMEOUT;
use nix::unistd::Uid;
use std::{
    fs::{self, FileType, Permissions},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio::{
    net::{UnixListener, UnixStream},
    time::timeout,
};

pub const SOCKET_DIRECTORY: &str = "localdesk";
pub const SOCKET_NAME: &str = "appd.sock";
const DIRECTORY_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;

#[derive(Debug)]
pub struct BoundSocket {
    pub listener: UnixListener,
    pub path: PathBuf,
}

#[derive(Debug, Error)]
pub enum SocketError {
    #[error("XDG_RUNTIME_DIR must be an absolute path")]
    InvalidRuntimeDir,
    #[error("runtime path is not owned by the effective uid: {path}")]
    WrongOwner { path: PathBuf },
    #[error("runtime path has unsafe permissions {actual:o}, expected {expected:o}: {path}")]
    UnsafePermissions {
        path: PathBuf,
        actual: u32,
        expected: u32,
    },
    #[error("runtime path is a symlink: {path}")]
    Symlink { path: PathBuf },
    #[error("runtime path is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("socket path is not a socket: {path}")]
    NotSocket { path: PathBuf },
    #[error("socket path is not owned by the effective uid: {path}")]
    SocketWrongOwner { path: PathBuf },
    #[error("an appd listener is already active: {path}")]
    ActiveListener { path: PathBuf },
    #[error("stale socket probe timed out: {path}")]
    ProbeTimeout { path: PathBuf },
    #[error("stale socket probe failed: {path}: {source}")]
    ProbeFailed { path: PathBuf, source: io::Error },
    #[error("socket path cleanup failed: {0}")]
    Cleanup(#[from] io::Error),
    #[error("socket bind failed: {0}")]
    Bind(#[source] io::Error),
}

pub fn runtime_dir_from_env() -> Result<PathBuf, SocketError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or(SocketError::InvalidRuntimeDir)?;
    validate_directory(&runtime_dir, DIRECTORY_MODE)?;
    Ok(runtime_dir)
}

pub async fn bind_appd_socket(runtime_dir: &Path) -> Result<BoundSocket, SocketError> {
    validate_directory(runtime_dir, DIRECTORY_MODE)?;
    let directory = runtime_dir.join(SOCKET_DIRECTORY);
    ensure_private_directory(&directory)?;
    let path = directory.join(SOCKET_NAME);

    prepare_socket_path(&path).await?;
    let listener = UnixListener::bind(&path).map_err(SocketError::Bind)?;
    set_socket_mode(&path)?;
    Ok(BoundSocket { listener, path })
}

pub fn remove_socket(path: &Path) -> Result<(), SocketError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(SocketError::Cleanup(error)),
    };
    reject_symlink(path, &metadata.file_type())?;
    if !metadata.file_type().is_socket() {
        return Err(SocketError::NotSocket {
            path: path.to_owned(),
        });
    }
    verify_owner(path, &metadata, true)?;
    fs::remove_file(path).map_err(SocketError::Cleanup)
}

async fn prepare_socket_path(path: &Path) -> Result<(), SocketError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(SocketError::Cleanup(error)),
    };
    reject_symlink(path, &metadata.file_type())?;
    if !metadata.file_type().is_socket() {
        return Err(SocketError::NotSocket {
            path: path.to_owned(),
        });
    }
    verify_owner(path, &metadata, true)?;

    match timeout(FRAME_IDLE_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(_stream)) => Err(SocketError::ActiveListener {
            path: path.to_owned(),
        }),
        Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => remove_socket(path),
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => remove_socket(path),
        Ok(Err(error)) => Err(SocketError::ProbeFailed {
            path: path.to_owned(),
            source: error,
        }),
        Err(_) => Err(SocketError::ProbeTimeout {
            path: path.to_owned(),
        }),
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), SocketError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            reject_symlink(path, &metadata.file_type())?;
            if !metadata.file_type().is_dir() {
                return Err(SocketError::NotDirectory {
                    path: path.to_owned(),
                });
            }
            verify_owner(path, &metadata, false)?;
            verify_mode(path, &metadata, DIRECTORY_MODE)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(SocketError::Cleanup)?;
            let metadata = fs::symlink_metadata(path).map_err(SocketError::Cleanup)?;
            reject_symlink(path, &metadata.file_type())?;
            verify_owner(path, &metadata, false)?;
            fs::set_permissions(path, Permissions::from_mode(DIRECTORY_MODE))
                .map_err(SocketError::Cleanup)?;
            let metadata = fs::symlink_metadata(path).map_err(SocketError::Cleanup)?;
            reject_symlink(path, &metadata.file_type())?;
            verify_mode(path, &metadata, DIRECTORY_MODE)?;
        }
        Err(error) => return Err(SocketError::Cleanup(error)),
    }
    Ok(())
}

fn validate_directory(path: &Path, expected_mode: u32) -> Result<(), SocketError> {
    if !path.is_absolute() {
        return Err(SocketError::InvalidRuntimeDir);
    }
    let metadata = fs::symlink_metadata(path).map_err(SocketError::Cleanup)?;
    reject_symlink(path, &metadata.file_type())?;
    if !metadata.file_type().is_dir() {
        return Err(SocketError::NotDirectory {
            path: path.to_owned(),
        });
    }
    verify_owner(path, &metadata, false)?;
    verify_mode(path, &metadata, expected_mode)
}

fn reject_symlink(path: &Path, file_type: &FileType) -> Result<(), SocketError> {
    if file_type.is_symlink() {
        return Err(SocketError::Symlink {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn verify_owner(
    path: &Path,
    metadata: &std::fs::Metadata,
    socket: bool,
) -> Result<(), SocketError> {
    verify_owner_with_expected_uid(path, metadata, socket, Uid::effective().as_raw())
}

fn verify_owner_with_expected_uid(
    path: &Path,
    metadata: &std::fs::Metadata,
    socket: bool,
    expected_uid: u32,
) -> Result<(), SocketError> {
    if metadata.uid() != expected_uid {
        return if socket {
            Err(SocketError::SocketWrongOwner {
                path: path.to_owned(),
            })
        } else {
            Err(SocketError::WrongOwner {
                path: path.to_owned(),
            })
        };
    }
    Ok(())
}

fn verify_mode(
    path: &Path,
    metadata: &std::fs::Metadata,
    expected: u32,
) -> Result<(), SocketError> {
    let actual = metadata.permissions().mode() & 0o777;
    if actual != expected {
        return Err(SocketError::UnsafePermissions {
            path: path.to_owned(),
            actual,
            expected,
        });
    }
    Ok(())
}

fn set_socket_mode(path: &Path) -> Result<(), SocketError> {
    fs::set_permissions(path, Permissions::from_mode(SOCKET_MODE)).map_err(SocketError::Cleanup)?;
    let metadata = fs::symlink_metadata(path).map_err(SocketError::Cleanup)?;
    reject_symlink(path, &metadata.file_type())?;
    if !metadata.file_type().is_socket() {
        return Err(SocketError::NotSocket {
            path: path.to_owned(),
        });
    }
    verify_owner(path, &metadata, true)?;
    verify_mode(path, &metadata, SOCKET_MODE)
}

#[cfg(test)]
mod tests {
    use super::{SocketError, verify_owner_with_expected_uid};
    use std::{fs, os::unix::fs::MetadataExt};
    use tempfile::tempdir;

    #[test]
    fn owner_validation_accepts_injected_effective_uid() {
        let directory = tempdir().expect("directory");
        let metadata = fs::metadata(directory.path()).expect("metadata");
        let expected_uid = metadata.uid();
        verify_owner_with_expected_uid(directory.path(), &metadata, false, expected_uid)
            .expect("matching owner");

        let error = verify_owner_with_expected_uid(
            directory.path(),
            &metadata,
            false,
            expected_uid.saturating_add(1),
        )
        .expect_err("wrong owner");
        assert!(matches!(error, SocketError::WrongOwner { .. }));
    }
}
