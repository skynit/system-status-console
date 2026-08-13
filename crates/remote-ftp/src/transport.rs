use std::ffi::{CString, c_long};
use std::io::{Read, Seek, Write};
use std::time::{Duration, Instant};

use curl::easy::{Easy, SeekResult, SslVersion};
use localdesk_remote_core::RemoteIoControl;

use crate::config::{DataMode, FtpConfig, SecurityMode};
use crate::protocol::ProtocolTrace;
use crate::{FtpError, FtpFailureKind, RemotePath};

pub(crate) trait Transport {
    fn list(
        &self,
        path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<Vec<u8>, FtpError>;
    fn commands(
        &self,
        commands: &[String],
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError>;
    fn remote_size(
        &self,
        path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<Option<u64>, FtpError>;
    fn read_chunk(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
        control: Option<&RemoteIoControl>,
    ) -> Result<(Vec<u8>, u64), FtpError>;
    fn download(
        &self,
        remote: &RemotePath,
        offset: u64,
        output: &mut dyn Write,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError>;
    fn upload(
        &self,
        local: &mut dyn ReadSeek,
        local_size: u64,
        offset: u64,
        remote_part: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError>;
}

pub(crate) trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

#[derive(Debug)]
pub(crate) struct LibcurlTransport {
    config: FtpConfig,
}

impl LibcurlTransport {
    pub(crate) fn new(config: FtpConfig) -> Self {
        Self { config }
    }

    fn prepare(
        &self,
        path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<Easy, FtpError> {
        check_control(control)?;
        let mut easy = Easy::new();
        easy.url(&format!(
            "ftp://{}{}",
            self.config.authority(),
            path.encoded()
        ))?;
        easy.username(self.config.credentials.username())?;
        easy.password(self.config.credentials.password())?;
        easy.proxy("")?;
        easy.connect_timeout(bounded_timeout(self.config.connect_timeout, control)?)?;
        easy.timeout(bounded_timeout(self.config.operation_timeout, control)?)?;
        easy.progress(control.is_some())?;
        easy.verbose(true)?;
        easy.tcp_keepalive(true)?;

        setopt_long(&easy, curl_sys::CURLOPT_TRANSFERTEXT, 0)?;
        setopt_long(
            &easy,
            curl_sys::CURLOPT_FTP_RESPONSE_TIMEOUT,
            duration_seconds(self.config.connect_timeout),
        )?;

        match &self.config.security {
            SecurityMode::ExplicitFtps => {
                setopt_long(
                    &easy,
                    curl_sys::CURLOPT_USE_SSL,
                    c_long::from(curl_sys::CURLUSESSL_ALL),
                )?;
                // CURLFTPAUTH_TLS is 2 in libcurl's stable public ABI.
                setopt_long(&easy, curl_sys::CURLOPT_FTPSSLAUTH, 2)?;
                easy.ssl_verify_peer(true)?;
                easy.ssl_verify_host(true)?;
                easy.ssl_min_max_version(SslVersion::Tlsv12, SslVersion::Default)?;
                if let Some(certificate) = &self.config.ca_certificate_pem {
                    easy.ssl_cainfo_blob(certificate)?;
                }
            }
            SecurityMode::PlainFtp(_) => {
                setopt_long(
                    &easy,
                    curl_sys::CURLOPT_USE_SSL,
                    c_long::from(curl_sys::CURLUSESSL_NONE),
                )?;
            }
        }

        match &self.config.data_mode {
            DataMode::Passive => setopt_ptr(
                &easy,
                curl_sys::CURLOPT_FTPPORT,
                std::ptr::null::<std::ffi::c_char>(),
            )?,
            DataMode::Active { .. } => {
                let binding = self
                    .config
                    .data_mode
                    .active_binding()?
                    .expect("active mode always has a validated binding");
                setopt_string(&easy, curl_sys::CURLOPT_FTPPORT, &binding)?;
            }
        }
        Ok(easy)
    }

    fn finish(
        &self,
        easy: &Easy,
        result: Result<(), curl::Error>,
        trace: &ProtocolTrace,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        check_control(control)?;
        if matches!(self.config.security, SecurityMode::ExplicitFtps) {
            trace.verify_for_result(result.is_ok())?;
        }
        result.map_err(|error| FtpError::remote(&error, easy.response_code().ok()))
    }

    fn perform_collect(
        &self,
        mut easy: Easy,
        control: Option<&RemoteIoControl>,
    ) -> Result<Vec<u8>, FtpError> {
        let mut output = Vec::new();
        let mut trace = ProtocolTrace::default();
        let result = {
            let mut transfer = easy.transfer();
            transfer.write_function(|bytes| {
                output.extend_from_slice(bytes);
                Ok(bytes.len())
            })?;
            transfer.debug_function(|info, bytes| trace.observe(info, bytes))?;
            configure_progress(&mut transfer, control)?;
            transfer.perform()
        };
        self.finish(&easy, result, &trace, control)?;
        Ok(output)
    }

    fn perform_empty(
        &self,
        mut easy: Easy,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        let mut trace = ProtocolTrace::default();
        let result = {
            let mut transfer = easy.transfer();
            transfer.write_function(|bytes| Ok(bytes.len()))?;
            transfer.debug_function(|info, bytes| trace.observe(info, bytes))?;
            configure_progress(&mut transfer, control)?;
            transfer.perform()
        };
        self.finish(&easy, result, &trace, control)
    }
}

impl Transport for LibcurlTransport {
    fn list(
        &self,
        path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<Vec<u8>, FtpError> {
        let directory = if path.as_str().ends_with('/') {
            path.clone()
        } else {
            RemotePath::new(format!("{}/", path.as_str()))?
        };
        let mut easy = self.prepare(&directory, control)?;
        easy.custom_request("MLSD")?;
        self.perform_collect(easy, control)
    }

    fn commands(
        &self,
        commands: &[String],
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        let mut easy = self.prepare(&RemotePath::root(), control)?;
        easy.nobody(true)?;
        let command_list = CommandList::new(commands)?;
        setopt_ptr(&easy, curl_sys::CURLOPT_QUOTE, command_list.raw())?;
        self.perform_empty(easy, control)
    }

    fn remote_size(
        &self,
        path: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<Option<u64>, FtpError> {
        let mut easy = self.prepare(path, control)?;
        easy.nobody(true)?;
        let mut trace = ProtocolTrace::default();
        let result = {
            let mut transfer = easy.transfer();
            transfer.write_function(|bytes| Ok(bytes.len()))?;
            transfer.debug_function(|info, bytes| trace.observe(info, bytes))?;
            configure_progress(&mut transfer, control)?;
            transfer.perform()
        };
        let response = easy.response_code().ok();
        check_control(control)?;
        if matches!(self.config.security, SecurityMode::ExplicitFtps) {
            trace.verify_for_result(result.is_ok())?;
        }
        match result {
            Ok(()) => {
                let size = content_length_download(&easy)?;
                if size < 0 {
                    return Err(FtpError::Protocol(
                        "server returned an invalid remote file size".into(),
                    ));
                }
                Ok(Some(u64::try_from(size).map_err(|_| {
                    FtpError::Protocol("server returned a remote file size overflow".into())
                })?))
            }
            Err(_) if response == Some(550) => Ok(None),
            Err(error) => Err(FtpError::remote(&error, response)),
        }
    }

    fn read_chunk(
        &self,
        remote: &RemotePath,
        offset: u64,
        max_bytes: u32,
        control: Option<&RemoteIoControl>,
    ) -> Result<(Vec<u8>, u64), FtpError> {
        check_control(control)?;
        if max_bytes == 0 {
            return Err(FtpError::Configuration(
                "read chunk size must be greater than zero".into(),
            ));
        }
        let size = self
            .remote_size(remote, control)?
            .ok_or_else(|| FtpError::Remote {
                code: Some(550),
                failure: FtpFailureKind::NotFound,
                reason: "FTP SIZE did not identify a regular file".into(),
            })?;
        if offset > size {
            return Err(FtpError::Protocol(format!(
                "read offset {offset} exceeds remote size {size}"
            )));
        }
        if offset == size {
            return Ok((Vec::new(), size));
        }
        let end = offset
            .saturating_add(u64::from(max_bytes))
            .min(size)
            .saturating_sub(1);
        let mut easy = self.prepare(remote, control)?;
        easy.range(&format!("{offset}-{end}"))?;
        let bytes = self.perform_collect(easy, control)?;
        let expected = usize::try_from(end - offset + 1).map_err(|_| {
            FtpError::Protocol("requested FTP range length overflowed local usize".into())
        })?;
        if bytes.len() != expected {
            return Err(FtpError::Protocol(
                "server did not return the exact requested FTP byte range".into(),
            ));
        }
        if self.remote_size(remote, control)? != Some(size) {
            return Err(FtpError::Protocol(
                "remote file size changed during the FTP range read".into(),
            ));
        }
        Ok((bytes, size))
    }

    fn download(
        &self,
        remote: &RemotePath,
        offset: u64,
        output: &mut dyn Write,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        let mut easy = self.prepare(remote, control)?;
        if offset > 0 {
            easy.resume_from(offset)?;
        }
        let mut trace = ProtocolTrace::default();
        let mut io_error = None;
        let result = {
            let mut transfer = easy.transfer();
            transfer.write_function(|bytes| match output.write_all(bytes) {
                Ok(()) => Ok(bytes.len()),
                Err(error) => {
                    io_error = Some(error);
                    Ok(0)
                }
            })?;
            transfer.debug_function(|info, bytes| trace.observe(info, bytes))?;
            configure_progress(&mut transfer, control)?;
            transfer.perform()
        };
        if let Some(error) = io_error {
            return Err(FtpError::Io(error));
        }
        self.finish(&easy, result, &trace, control)
    }

    fn upload(
        &self,
        local: &mut dyn ReadSeek,
        local_size: u64,
        offset: u64,
        remote_part: &RemotePath,
        control: Option<&RemoteIoControl>,
    ) -> Result<(), FtpError> {
        let mut easy = self.prepare(remote_part, control)?;
        easy.upload(true)?;
        easy.in_filesize(local_size)?;
        if offset > 0 {
            easy.resume_from(offset)?;
        }

        let mut trace = ProtocolTrace::default();
        let local = std::cell::RefCell::new(local);
        let io_error = std::cell::RefCell::new(None);
        let result = {
            let mut transfer = easy.transfer();
            transfer.read_function(|buffer| match local.borrow_mut().read(buffer) {
                Ok(read) => Ok(read),
                Err(error) => {
                    *io_error.borrow_mut() = Some(error);
                    Err(curl::easy::ReadError::Abort)
                }
            })?;
            transfer.seek_function(|from| match local.borrow_mut().seek(from) {
                Ok(_) => SeekResult::Ok,
                Err(error) => {
                    *io_error.borrow_mut() = Some(error);
                    SeekResult::Fail
                }
            })?;
            transfer.debug_function(|info, bytes| trace.observe(info, bytes))?;
            configure_progress(&mut transfer, control)?;
            transfer.perform()
        };
        if let Some(error) = io_error.into_inner() {
            return Err(FtpError::Io(error));
        }
        self.finish(&easy, result, &trace, control)
    }
}

fn check_control(control: Option<&RemoteIoControl>) -> Result<(), FtpError> {
    let Some(control) = control else {
        return Ok(());
    };
    if control.is_cancelled() {
        return Err(FtpError::Cancelled);
    }
    if Instant::now() >= control.deadline() {
        return Err(FtpError::DeadlineExceeded);
    }
    Ok(())
}

fn bounded_timeout(
    configured: Duration,
    control: Option<&RemoteIoControl>,
) -> Result<Duration, FtpError> {
    check_control(control)?;
    Ok(control.map_or(configured, |control| {
        configured.min(
            control
                .deadline()
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        )
    }))
}

fn configure_progress<'data>(
    transfer: &mut curl::easy::Transfer<'_, 'data>,
    control: Option<&'data RemoteIoControl>,
) -> Result<(), FtpError> {
    if let Some(control) = control {
        transfer.progress_function(|_, _, _, _| {
            !control.is_cancelled() && Instant::now() < control.deadline()
        })?;
    }
    Ok(())
}

fn duration_seconds(duration: std::time::Duration) -> c_long {
    c_long::try_from(duration.as_secs()).unwrap_or(c_long::MAX)
}

fn setopt_long(easy: &Easy, option: curl_sys::CURLoption, value: c_long) -> Result<(), FtpError> {
    // SAFETY: `easy.raw()` is live for this call and this option accepts a C long.
    let code = unsafe { curl_sys::curl_easy_setopt(easy.raw(), option, value) };
    curl_code(code)
}

fn setopt_string(easy: &Easy, option: curl_sys::CURLoption, value: &str) -> Result<(), FtpError> {
    let value = CString::new(value)
        .map_err(|_| FtpError::Configuration("libcurl option contains NUL".into()))?;
    setopt_ptr(easy, option, value.as_ptr())
}

fn setopt_ptr<T>(
    easy: &Easy,
    option: curl_sys::CURLoption,
    value: *const T,
) -> Result<(), FtpError> {
    // SAFETY: `easy.raw()` is live and callers provide the pointer type required by option.
    // String options used here are copied by libcurl; slists stay alive through perform.
    let code = unsafe { curl_sys::curl_easy_setopt(easy.raw(), option, value) };
    curl_code(code)
}

fn curl_code(code: curl_sys::CURLcode) -> Result<(), FtpError> {
    if code == curl_sys::CURLE_OK {
        Ok(())
    } else {
        Err(curl::Error::new(code).into())
    }
}

fn content_length_download(easy: &Easy) -> Result<curl_sys::curl_off_t, FtpError> {
    const CURLINFO_CONTENT_LENGTH_DOWNLOAD_T: curl_sys::CURLINFO = 0x60_000F;
    let mut value: curl_sys::curl_off_t = -1;
    // SAFETY: the info selector requires a valid `curl_off_t*`; `easy` is live.
    let code = unsafe {
        curl_sys::curl_easy_getinfo(easy.raw(), CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &mut value)
    };
    curl_code(code)?;
    Ok(value)
}

struct CommandList {
    raw: *mut curl_sys::curl_slist,
}

impl CommandList {
    fn new(commands: &[String]) -> Result<Self, FtpError> {
        let mut list = Self {
            raw: std::ptr::null_mut(),
        };
        for command in commands {
            let command = CString::new(command.as_str())
                .map_err(|_| FtpError::Configuration("FTP command must not contain NUL".into()))?;
            // SAFETY: libcurl copies `command`; `list.raw` is null or a valid slist.
            let appended = unsafe { curl_sys::curl_slist_append(list.raw, command.as_ptr()) };
            if appended.is_null() {
                return Err(FtpError::Remote {
                    code: None,
                    failure: FtpFailureKind::Transport,
                    reason: "libcurl could not allocate an FTP command list".into(),
                });
            }
            list.raw = appended;
        }
        Ok(list)
    }

    fn raw(&self) -> *const curl_sys::curl_slist {
        self.raw
    }
}

impl Drop for CommandList {
    fn drop(&mut self) {
        // SAFETY: this type uniquely owns the slist returned by `curl_slist_append`.
        unsafe { curl_sys::curl_slist_free_all(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_timeout_distinguishes_cancelled_and_elapsed_controls() {
        let cancelled = RemoteIoControl::new(Instant::now() + Duration::from_secs(5));
        cancelled.cancel();
        assert!(matches!(
            bounded_timeout(Duration::from_secs(30), Some(&cancelled)),
            Err(FtpError::Cancelled)
        ));

        let elapsed = RemoteIoControl::new(Instant::now());
        assert!(matches!(
            bounded_timeout(Duration::from_secs(30), Some(&elapsed)),
            Err(FtpError::DeadlineExceeded)
        ));
    }

    #[test]
    fn bounded_timeout_never_exceeds_remaining_deadline() {
        let control = RemoteIoControl::new(Instant::now() + Duration::from_secs(1));
        let bounded = bounded_timeout(Duration::from_secs(30), Some(&control)).unwrap();

        assert!(bounded <= Duration::from_secs(1));
        assert!(bounded >= Duration::from_millis(1));
    }
}
