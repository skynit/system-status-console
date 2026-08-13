use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use localdesk_remote_core::RemoteIoControl;

use crate::transport::{LibcurlTransport, Transport};
use crate::{FtpConfig, FtpError, RemotePath};

#[derive(Debug)]
pub struct FtpAdapter {
    core: AdapterCore<LibcurlTransport>,
}

impl FtpAdapter {
    /// Creates an adapter after validating all connection and policy settings.
    ///
    /// # Errors
    ///
    /// Returns [`FtpError::Configuration`] or [`FtpError::Policy`] for invalid settings.
    pub fn new(config: FtpConfig) -> Result<Self, FtpError> {
        config.validate()?;
        Ok(Self {
            core: AdapterCore::new(LibcurlTransport::new(config)),
        })
    }

    /// Returns the server's raw `MLSD` directory bytes.
    ///
    /// # Errors
    ///
    /// Returns a protocol, transport, or policy error when listing fails.
    pub fn list(&self, directory: &RemotePath) -> Result<Vec<u8>, FtpError> {
        self.core.transport.list(directory, None)
    }

    /// Opens, authenticates, and verifies one FTP control session with `NOOP`.
    ///
    /// # Errors
    ///
    /// Returns a trust, authentication, protocol, or transport error when the endpoint cannot be
    /// proven usable under the configured policy.
    pub fn probe(&self) -> Result<(), FtpError> {
        self.core.transport.commands(&["NOOP".into()], None)
    }

    pub(crate) fn probe_controlled(&self, control: &RemoteIoControl) -> Result<(), FtpError> {
        self.core
            .transport
            .commands(&["NOOP".into()], Some(control))
    }

    /// Returns the size reported by FTP `SIZE`, or `None` when no regular file was identified.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error when the query itself fails.
    pub fn stat_size(&self, path: &RemotePath) -> Result<Option<u64>, FtpError> {
        self.core.transport.remote_size(path, None)
    }

    /// Reads one bounded byte range and returns it with the verified total file size.
    ///
    /// # Errors
    ///
    /// Returns a configuration, protocol, or transport error if the range cannot be read exactly.
    pub fn read_chunk(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        self.core
            .transport
            .read_chunk(remote, offset, max_bytes, None)
    }

    pub(crate) fn read_chunk_controlled(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
        control: &RemoteIoControl,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        self.core
            .transport
            .read_chunk(remote, offset, max_bytes, Some(control))
    }

    /// Creates one remote directory.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error when `MKD` fails.
    pub fn create_directory(&self, directory: &RemotePath) -> Result<(), FtpError> {
        self.core.command("MKD", directory)
    }

    /// Removes one empty remote directory.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error when `RMD` fails.
    pub fn remove_directory(&self, directory: &RemotePath) -> Result<(), FtpError> {
        self.core.command("RMD", directory)
    }

    /// Deletes one remote file.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error when `DELE` fails.
    pub fn delete_file(&self, file: &RemotePath) -> Result<(), FtpError> {
        self.core.command("DELE", file)
    }

    /// Deletes a file with `DELE`, falling back to `RMD` for an empty directory.
    ///
    /// # Errors
    ///
    /// Returns the final FTP error if the path is neither a deletable file nor empty directory.
    pub fn delete_path(&self, path: &RemotePath) -> Result<(), FtpError> {
        self.delete_file(path)
            .or_else(|_| self.remove_directory(path))
    }

    /// Renames one remote path with an `RNFR`/`RNTO` pair.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error if either command fails.
    pub fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), FtpError> {
        self.core.transport.commands(
            &[
                format!("RNFR {}", from.as_str()),
                format!("RNTO {}", to.as_str()),
            ],
            None,
        )
    }

    /// Downloads into a sibling `.part` file and atomically renames it on success.
    ///
    /// # Errors
    ///
    /// Returns a local I/O, protocol, transport, or policy error. A failed transfer keeps
    /// the `.part` file so a later call with `resume` can continue it.
    pub fn download(
        &self,
        remote: &RemotePath,
        destination: &Path,
        resume: bool,
    ) -> Result<(), FtpError> {
        self.core.download(remote, destination, resume)
    }

    /// Uploads into a remote `.part` path and renames it after verifying its final size.
    ///
    /// # Errors
    ///
    /// Returns a local I/O, protocol, transport, or policy error. With `resume`, the
    /// adapter queries the remote `.part` size. Libcurl then uses FTP upload-resume semantics:
    /// `SIZE`, local seek, and `APPE`.
    pub fn upload(&self, source: &Path, remote: &RemotePath, resume: bool) -> Result<(), FtpError> {
        self.core.upload(source, remote, resume)
    }

    /// Uploads through an explicitly supplied remote `.part` path and then renames it.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested resume offset differs from the remote temporary size,
    /// the final temporary size cannot be verified, or local I/O/upload/rename fails.
    pub fn upload_with_temporary(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
    ) -> Result<(), FtpError> {
        self.core
            .upload_with_temporary(source, temporary, final_path, resume_from, None)
    }

    pub(crate) fn upload_with_temporary_controlled(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
        control: &RemoteIoControl,
    ) -> Result<(), FtpError> {
        self.core
            .upload_with_temporary(source, temporary, final_path, resume_from, Some(control))
    }
}

#[derive(Debug)]
struct AdapterCore<T> {
    transport: T,
}

impl<T: Transport> AdapterCore<T> {
    fn new(transport: T) -> Self {
        Self { transport }
    }

    fn command(&self, verb: &str, path: &RemotePath) -> Result<(), FtpError> {
        self.transport
            .commands(&[format!("{verb} {}", path.as_str())], None)
    }

    fn download(
        &self,
        remote: &RemotePath,
        destination: &Path,
        resume: bool,
    ) -> Result<(), FtpError> {
        let part = local_part_path(destination)?;
        reject_symlink(&part)?;
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        if resume {
            options.append(true);
        } else {
            options.truncate(true);
        }
        let mut output = options.open(&part)?;
        let offset = if resume { output.metadata()?.len() } else { 0 };
        self.transport.download(remote, offset, &mut output, None)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&part, destination)?;
        Ok(())
    }

    fn upload(&self, source: &Path, remote: &RemotePath, resume: bool) -> Result<(), FtpError> {
        reject_symlink(source)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() {
            return Err(FtpError::Policy(
                "upload source must be a regular file".into(),
            ));
        }
        let remote_part = remote.part_path()?;
        let offset = if resume {
            self.transport.remote_size(&remote_part, None)?.unwrap_or(0)
        } else {
            0
        };
        if offset > metadata.len() {
            return Err(FtpError::Protocol(format!(
                "remote .part size {offset} exceeds local size {}",
                metadata.len()
            )));
        }
        let mut input = File::open(source)?;
        self.upload_part_and_commit(
            &mut input,
            metadata.len(),
            offset,
            &remote_part,
            remote,
            None,
        )
    }

    fn upload_with_temporary(
        &self,
        source: &Path,
        temporary: &RemotePath,
        final_path: &RemotePath,
        resume_from: Option<u64>,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        reject_symlink(source)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() {
            return Err(FtpError::Policy(
                "upload source must be a regular file".into(),
            ));
        }
        let offset = match resume_from {
            Some(expected) => {
                let actual = self.transport.remote_size(temporary, control)?.unwrap_or(0);
                if actual != expected {
                    return Err(FtpError::Protocol(format!(
                        "remote .part size {actual} does not match requested resume offset {expected}"
                    )));
                }
                expected
            }
            None => 0,
        };
        if offset > metadata.len() {
            return Err(FtpError::Protocol(format!(
                "resume offset {offset} exceeds local size {}",
                metadata.len()
            )));
        }
        let mut input = File::open(source)?;
        self.upload_part_and_commit(
            &mut input,
            metadata.len(),
            offset,
            temporary,
            final_path,
            control,
        )
    }

    fn upload_part_and_commit(
        &self,
        input: &mut File,
        local_size: u64,
        offset: u64,
        temporary: &RemotePath,
        final_path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        self.transport
            .upload(input, local_size, offset, temporary, control)?;
        let uploaded_size = self
            .transport
            .remote_size(temporary, control)?
            .ok_or_else(|| {
                FtpError::Protocol(
                "FTP SIZE could not verify the remote .part after upload; refusing final rename"
                    .into(),
            )
            })?;
        if uploaded_size != local_size {
            return Err(FtpError::Protocol(format!(
                "remote .part size {uploaded_size} after upload does not match local size {local_size}; refusing final rename"
            )));
        }

        // FTP provides no identity token tying this SIZE result to the following RNFR/RNTO.
        // The check narrows incomplete-upload risk but a server-side TOCTOU remains; callers must
        // not interpret this commit as identity-safe or advertise endpoint atomicity.
        self.transport.commands(
            &[
                format!("RNFR {}", temporary.as_str()),
                format!("RNTO {}", final_path.as_str()),
            ],
            control,
        )
    }
}

fn local_part_path(destination: &Path) -> Result<PathBuf, FtpError> {
    let name = destination.file_name().ok_or_else(|| {
        FtpError::Configuration("download destination must have a filename".into())
    })?;
    let mut part_name = name.to_os_string();
    part_name.push(".part");
    Ok(destination.with_file_name(part_name))
}

fn reject_symlink(path: &Path) -> Result<(), FtpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(FtpError::Policy(format!(
            "refusing to follow symlink {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use tempfile::tempdir;

    use super::*;
    use crate::transport::ReadSeek;

    #[derive(Debug, Default)]
    struct FakeTransport {
        download_body: Vec<u8>,
        remote_size: Cell<Option<u64>>,
        preserve_remote_size_on_upload: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Transport for FakeTransport {
        fn list(
            &self,
            _: &RemotePath,
            control: Option<&RemoteIoControl>,
        ) -> Result<Vec<u8>, FtpError> {
            check_fake_control(control)?;
            Ok(Vec::new())
        }

        fn commands(
            &self,
            commands: &[String],
            control: Option<&RemoteIoControl>,
        ) -> Result<(), FtpError> {
            check_fake_control(control)?;
            self.calls.borrow_mut().extend_from_slice(commands);
            Ok(())
        }

        fn remote_size(
            &self,
            path: &RemotePath,
            control: Option<&RemoteIoControl>,
        ) -> Result<Option<u64>, FtpError> {
            check_fake_control(control)?;
            self.calls
                .borrow_mut()
                .push(format!("SIZE {}", path.as_str()));
            Ok(self.remote_size.get())
        }

        fn read_chunk(
            &self,
            _: &RemotePath,
            offset: u64,
            max_bytes: u32,
            control: Option<&RemoteIoControl>,
        ) -> Result<(Vec<u8>, u64), FtpError> {
            check_fake_control(control)?;
            let size = self.download_body.len() as u64;
            let start = usize::try_from(offset)
                .map_err(|_| FtpError::Protocol("test offset overflow".into()))?;
            let end = start
                .saturating_add(max_bytes as usize)
                .min(self.download_body.len());
            Ok((self.download_body[start..end].to_vec(), size))
        }

        fn download(
            &self,
            remote: &RemotePath,
            offset: u64,
            output: &mut dyn std::io::Write,
            control: Option<&RemoteIoControl>,
        ) -> Result<(), FtpError> {
            check_fake_control(control)?;
            self.calls
                .borrow_mut()
                .push(format!("REST {offset}; RETR {}", remote.as_str()));
            let offset = usize::try_from(offset)
                .map_err(|_| FtpError::Protocol("test offset overflow".into()))?;
            output.write_all(&self.download_body[offset..])?;
            Ok(())
        }

        fn upload(
            &self,
            local: &mut dyn ReadSeek,
            local_size: u64,
            offset: u64,
            remote_part: &RemotePath,
            control: Option<&RemoteIoControl>,
        ) -> Result<(), FtpError> {
            check_fake_control(control)?;
            local.seek(std::io::SeekFrom::Start(offset))?;
            let mut remaining = Vec::new();
            local.read_to_end(&mut remaining)?;
            self.calls.borrow_mut().push(format!(
                "RESUME {offset}; UPLOAD {}; bytes={}",
                remote_part.as_str(),
                remaining.len()
            ));
            if !self.preserve_remote_size_on_upload {
                self.remote_size.set(Some(local_size));
            }
            Ok(())
        }
    }

    fn check_fake_control(control: Option<&RemoteIoControl>) -> Result<(), FtpError> {
        let Some(control) = control else {
            return Ok(());
        };
        if control.is_cancelled() {
            return Err(FtpError::Cancelled);
        }
        if std::time::Instant::now() >= control.deadline() {
            return Err(FtpError::DeadlineExceeded);
        }
        Ok(())
    }

    #[test]
    fn fake_download_resumes_part_then_atomically_renames() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("report.bin");
        fs::write(directory.path().join("report.bin.part"), b"abc").unwrap();
        let core = AdapterCore::new(FakeTransport {
            download_body: b"abcdef".to_vec(),
            ..FakeTransport::default()
        });

        core.download(&RemotePath::new("/report.bin").unwrap(), &destination, true)
            .unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"abcdef");
        assert!(!directory.path().join("report.bin.part").exists());
        assert_eq!(
            core.transport.calls.into_inner(),
            ["REST 3; RETR /report.bin"]
        );
    }

    #[test]
    fn fake_upload_resumes_remote_part_and_renames_after_transfer() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("payload.bin");
        fs::write(&source, b"abcdef").unwrap();
        let core = AdapterCore::new(FakeTransport {
            remote_size: Cell::new(Some(2)),
            ..FakeTransport::default()
        });

        core.upload(&source, &RemotePath::new("/payload.bin").unwrap(), true)
            .unwrap();

        assert_eq!(
            core.transport.calls.into_inner(),
            [
                "SIZE /payload.bin.part",
                "RESUME 2; UPLOAD /payload.bin.part; bytes=4",
                "SIZE /payload.bin.part",
                "RNFR /payload.bin.part",
                "RNTO /payload.bin"
            ]
        );
    }

    #[test]
    fn upload_keeps_part_when_final_size_cannot_be_verified() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("payload.bin");
        fs::write(&source, b"abcdef").unwrap();
        let core = AdapterCore::new(FakeTransport {
            remote_size: Cell::new(Some(2)),
            preserve_remote_size_on_upload: true,
            ..FakeTransport::default()
        });

        let error = core
            .upload(&source, &RemotePath::new("/payload.bin").unwrap(), true)
            .unwrap_err();

        assert!(matches!(error, FtpError::Protocol(_)));
        assert_eq!(
            core.transport.calls.into_inner(),
            [
                "SIZE /payload.bin.part",
                "RESUME 2; UPLOAD /payload.bin.part; bytes=4",
                "SIZE /payload.bin.part"
            ]
        );
    }

    #[test]
    fn explicit_temporary_upload_rejects_resume_offset_drift() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("payload.bin");
        fs::write(&source, b"abcdef").unwrap();
        let core = AdapterCore::new(FakeTransport {
            remote_size: Cell::new(Some(2)),
            ..FakeTransport::default()
        });

        let error = core.upload_with_temporary(
            &source,
            &RemotePath::new("/payload.bin.part").unwrap(),
            &RemotePath::new("/payload.bin").unwrap(),
            Some(3),
            None,
        );

        assert!(error.is_err());
        assert_eq!(
            core.transport.calls.into_inner(),
            ["SIZE /payload.bin.part"]
        );
    }

    #[test]
    fn controlled_upload_rejects_cancellation_before_transport_io() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("payload.bin");
        fs::write(&source, b"abcdef").unwrap();
        let core = AdapterCore::new(FakeTransport::default());
        let control =
            RemoteIoControl::new(std::time::Instant::now() + std::time::Duration::from_secs(5));
        control.cancel();

        let error = core
            .upload_with_temporary(
                &source,
                &RemotePath::new("/payload.bin.part").unwrap(),
                &RemotePath::new("/payload.bin").unwrap(),
                Some(0),
                Some(&control),
            )
            .unwrap_err();

        assert!(matches!(error, FtpError::Cancelled));
        assert!(core.transport.calls.into_inner().is_empty());
    }

    #[test]
    fn controlled_upload_rejects_elapsed_deadline_before_transport_io() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("payload.bin");
        fs::write(&source, b"abcdef").unwrap();
        let core = AdapterCore::new(FakeTransport::default());
        let control = RemoteIoControl::new(std::time::Instant::now());

        let error = core
            .upload_with_temporary(
                &source,
                &RemotePath::new("/payload.bin.part").unwrap(),
                &RemotePath::new("/payload.bin").unwrap(),
                Some(0),
                Some(&control),
            )
            .unwrap_err();

        assert!(matches!(error, FtpError::DeadlineExceeded));
        assert!(core.transport.calls.into_inner().is_empty());
    }

    #[test]
    fn local_part_path_stays_next_to_destination() {
        let destination = Path::new("/tmp/out.tar");
        assert_eq!(
            local_part_path(destination).unwrap(),
            Path::new("/tmp/out.tar.part")
        );
    }
}
