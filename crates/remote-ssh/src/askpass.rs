use localdesk_remote_core::{
    RemoteError, RemoteErrorKind, RemoteOperation, RetryDisposition, SafeReason,
};
use nix::{
    fcntl::{FcntlArg, SealFlag, fcntl},
    sys::memfd::{MFdFlags, memfd_create},
};
use std::{
    fs,
    io::{self, Seek, SeekFrom, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub(crate) const ASKPASS_HELPER_NAME: &str = "localdesk-ssh-askpass";
pub(crate) const ASKPASS_SECRET_ENV: &str = "LOCALDESK_SSH_ASKPASS_SECRET";
pub(crate) const ASKPASS_REQUIRE_ENV: &str = "SSH_ASKPASS_REQUIRE";
const MEMFD_NAME: &str = "localdesk-ssh-askpass-secret";
const MAX_SECRET_BYTES: usize = 8 * 1024;

pub(crate) struct AskpassSecret {
    file: fs::File,
    helper: PathBuf,
}

impl AskpassSecret {
    pub(crate) fn new(secret: &[u8]) -> Result<Self, RemoteError> {
        if secret.is_empty()
            || secret.len() > MAX_SECRET_BYTES
            || secret.contains(&0)
            || secret.contains(&b'\n')
            || secret.contains(&b'\r')
        {
            return Err(error(
                RemoteErrorKind::InvalidInput,
                "ssh_askpass_secret_invalid",
                RetryDisposition::Never,
            ));
        }
        let helper = production_helper_path()?;
        validate_helper_path(&helper)?;
        let descriptor = memfd_create(
            MEMFD_NAME,
            MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
        )
        .map_err(|_| transport("ssh_askpass_memfd_create_failed"))?;
        let mut file = fs::File::from(descriptor);
        file.write_all(secret)
            .map_err(|_| transport("ssh_askpass_memfd_write_failed"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| transport("ssh_askpass_memfd_seek_failed"))?;
        fcntl(
            file.as_fd(),
            FcntlArg::F_ADD_SEALS(
                SealFlag::F_SEAL_SEAL
                    | SealFlag::F_SEAL_SHRINK
                    | SealFlag::F_SEAL_GROW
                    | SealFlag::F_SEAL_WRITE,
            ),
        )
        .map_err(|_| transport("ssh_askpass_memfd_seal_failed"))?;
        Ok(Self { file, helper })
    }

    #[cfg(test)]
    pub(crate) fn with_helper(secret: &[u8], helper: PathBuf) -> Result<Self, RemoteError> {
        let descriptor = memfd_create(
            MEMFD_NAME,
            MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
        )
        .map_err(|_| transport("ssh_askpass_memfd_create_failed"))?;
        let mut file = fs::File::from(descriptor);
        file.write_all(secret)
            .map_err(|_| transport("ssh_askpass_memfd_write_failed"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| transport("ssh_askpass_memfd_seek_failed"))?;
        fcntl(
            file.as_fd(),
            FcntlArg::F_ADD_SEALS(
                SealFlag::F_SEAL_SEAL
                    | SealFlag::F_SEAL_SHRINK
                    | SealFlag::F_SEAL_GROW
                    | SealFlag::F_SEAL_WRITE,
            ),
        )
        .map_err(|_| transport("ssh_askpass_memfd_seal_failed"))?;
        Ok(Self { file, helper })
    }

    pub(crate) fn helper_path(&self) -> &Path {
        &self.helper
    }

    pub(crate) fn secret_path(&self) -> String {
        format!("/proc/{}/fd/{}", std::process::id(), self.file.as_raw_fd())
    }

    pub(crate) fn configure_std_command(&self, command: &mut std::process::Command) {
        command
            .env("SSH_ASKPASS", self.helper_path())
            .env(ASKPASS_REQUIRE_ENV, "force")
            .env(ASKPASS_SECRET_ENV, self.secret_path());
    }

    pub(crate) fn configure_tokio_command(&self, command: &mut tokio::process::Command) {
        command
            .env("SSH_ASKPASS", self.helper_path())
            .env(ASKPASS_REQUIRE_ENV, "force")
            .env(ASKPASS_SECRET_ENV, self.secret_path());
    }
}

impl std::fmt::Debug for AskpassSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AskpassSecret")
            .field("secret", &"<sealed-memfd>")
            .field("helper", &self.helper)
            .finish()
    }
}

fn production_helper_path() -> Result<PathBuf, RemoteError> {
    let executable =
        std::env::current_exe().map_err(|_| transport("ssh_askpass_helper_path_unavailable"))?;
    let parent = executable
        .parent()
        .ok_or_else(|| transport("ssh_askpass_helper_path_unavailable"))?;
    let sibling = parent.join(ASKPASS_HELPER_NAME);
    if sibling.exists() {
        return Ok(sibling);
    }
    if parent.file_name().is_some_and(|name| name == "deps")
        && let Some(target_directory) = parent.parent()
    {
        let cargo_sibling = target_directory.join(ASKPASS_HELPER_NAME);
        if cargo_sibling.exists() {
            return Ok(cargo_sibling);
        }
    }
    Ok(sibling)
}

fn validate_helper_path(path: &Path) -> Result<(), RemoteError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => unsupported("ssh_askpass_helper_missing"),
        io::ErrorKind::PermissionDenied => unsupported("ssh_askpass_helper_unexecutable"),
        _ => transport("ssh_askpass_helper_path_rejected"),
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(unsupported("ssh_askpass_helper_path_rejected"));
    }
    // The on-disk appd binary may be atomically replaced during development or upgrade.
    // /proc/self/exe still refers to the executable inode that owns this running process.
    let executable = fs::metadata("/proc/self/exe")
        .map_err(|_| transport("ssh_askpass_helper_path_unavailable"))?;
    if metadata.uid() != executable.uid() {
        return Err(unsupported("ssh_askpass_helper_path_rejected"));
    }
    Ok(())
}

fn error(kind: RemoteErrorKind, reason: &'static str, retry: RetryDisposition) -> RemoteError {
    RemoteError::new(
        kind,
        RemoteOperation::ResolveSecret,
        SafeReason::new(reason).expect("static safe reason"),
        retry,
    )
}

fn transport(reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::Transport,
        reason,
        RetryDisposition::Backoff,
    )
}

fn unsupported(reason: &'static str) -> RemoteError {
    error(
        RemoteErrorKind::Unsupported,
        reason,
        RetryDisposition::UserAction,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted_and_memfd_is_sealed() {
        let helper = std::env::current_exe().unwrap();
        let secret = AskpassSecret::with_helper(b"test-value", helper).unwrap();
        let debug = format!("{secret:?}");
        assert!(debug.contains("<sealed-memfd>"));
        assert!(!debug.contains("test-value"));
        assert!(fs::write(secret.secret_path(), b"changed").is_err());
    }
}
