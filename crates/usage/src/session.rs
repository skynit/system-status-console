use std::collections::HashMap;
use std::env;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

pub const MAX_SESSION_ID_BYTES: usize = 128;
pub const MAX_LOGINCTL_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_LOGIND_EVENT_LINE_BYTES: usize = 64 * 1024;
pub const LOGINCTL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub active: bool,
    pub locked: bool,
    pub idle: bool,
}

impl SessionSnapshot {
    pub fn permits_accounting(&self) -> bool {
        self.active && !self.locked && !self.idle
    }
}

#[derive(Clone, Debug)]
pub struct LogindProbe {
    session_id: String,
}

impl LogindProbe {
    pub fn from_environment() -> Result<Self, SessionProbeError> {
        let session_id =
            env::var("XDG_SESSION_ID").map_err(|_| SessionProbeError::MissingSessionId)?;
        Self::new(session_id)
    }

    pub fn new(session_id: impl Into<String>) -> Result<Self, SessionProbeError> {
        let session_id = session_id.into();
        if session_id.is_empty()
            || session_id.len() > MAX_SESSION_ID_BYTES
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SessionProbeError::InvalidSessionId);
        }
        Ok(Self { session_id })
    }

    pub fn probe(&self) -> Result<SessionSnapshot, SessionProbeError> {
        let mut child = Command::new("loginctl")
            .args([
                "show-session",
                &self.session_id,
                "--no-pager",
                "--property=Active",
                "--property=LockedHint",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = collect_bounded(&mut child, LOGINCTL_TIMEOUT, MAX_LOGINCTL_OUTPUT_BYTES)?;
        if !output.status.success() {
            return Err(SessionProbeError::CommandFailed {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let stdout = String::from_utf8(output.stdout).map_err(SessionProbeError::Utf8)?;
        parse_logind_properties(&stdout)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Debug)]
pub struct LogindEventStream {
    child: Child,
    stdout: ChildStdout,
    input: Vec<u8>,
    object_path: String,
}

impl LogindEventStream {
    pub fn spawn(session_id: &str) -> Result<Self, SessionEventError> {
        LogindProbe::new(session_id).map_err(|_| SessionEventError::InvalidSessionId)?;
        let path = logind_session_object_path(session_id);
        let mut child = Command::new("gdbus")
            .args([
                "monitor",
                "--system",
                "--dest",
                "org.freedesktop.login1",
                "--object-path",
                &path,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or(SessionEventError::MissingStdout)?;
        set_nonblocking(stdout.as_raw_fd())?;
        Ok(Self {
            child,
            stdout,
            input: Vec::new(),
            object_path: path,
        })
    }

    /// Returns whether an authoritative accounting property edge was observed.
    ///
    /// The stream is filtered by gdbus to the selected logind session object.
    /// Property values are intentionally not interpreted. Only `Active`,
    /// `LockedHint` can change whether accounting is permitted, so unrelated
    /// session properties must not interrupt an active interval.
    pub fn poll_changed(&mut self, timeout: Duration) -> Result<bool, SessionEventError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            if let Some(line_end) = self.input.iter().position(|byte| *byte == b'\n') {
                let mut line = self.input.drain(..=line_end).collect::<Vec<_>>();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                if parse_logind_event_line(&line, &self.object_path)? {
                    return Ok(true);
                }
                continue;
            }
            validate_event_buffer(&self.input)?;

            let mut chunk = [0_u8; 4 * 1024];
            let mut read_any = false;
            loop {
                match self.stdout.read(&mut chunk) {
                    Ok(0) => {
                        if read_any {
                            break;
                        }
                        let status = self.child.try_wait()?;
                        return Err(SessionEventError::StreamEnded(
                            status.and_then(|value| value.code()),
                        ));
                    }
                    Ok(count) => {
                        read_any = true;
                        self.input.extend_from_slice(&chunk[..count]);
                        validate_event_buffer(&self.input)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            if read_any {
                continue;
            }
            if !poll_readable_until(self.stdout.as_raw_fd(), deadline)? {
                return Ok(false);
            }
        }
    }
}

impl Drop for LogindEventStream {
    fn drop(&mut self) {
        terminate(&mut self.child);
    }
}

fn logind_session_object_path(session_id: &str) -> String {
    let mut escaped = String::with_capacity(session_id.len());
    for (index, byte) in session_id.bytes().enumerate() {
        if byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit()) {
            escaped.push(char::from(byte));
        } else {
            escaped.push('_');
            escaped.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
            escaped.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
        }
    }
    format!("/org/freedesktop/login1/session/{escaped}")
}

fn parse_logind_event_line(line: &[u8], expected_path: &str) -> Result<bool, SessionEventError> {
    let line = std::str::from_utf8(line)?;
    if line.starts_with("Monitoring signals on object ") || line.starts_with("The name ") {
        return Ok(false);
    }
    let (path, signal) = line
        .split_once(": ")
        .ok_or(SessionEventError::InvalidEventLine)?;
    if path != expected_path {
        return Err(SessionEventError::UnexpectedObjectPath);
    }
    let Some(arguments) = signal.strip_prefix(
        "org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', ",
    ) else {
        return Ok(false);
    };
    let (changed, invalidated) = arguments.rsplit_once(", ").unwrap_or((arguments, ""));
    Ok(changed.contains("'Active':")
        || changed.contains("'LockedHint':")
        || invalidated.contains("'Active'")
        || invalidated.contains("'LockedHint'"))
}

fn validate_event_buffer(input: &[u8]) -> Result<(), SessionEventError> {
    match input.iter().position(|byte| *byte == b'\n') {
        Some(line_bytes) if line_bytes > MAX_LOGIND_EVENT_LINE_BYTES => {
            Err(SessionEventError::LineTooLong)
        }
        None if input.len() > MAX_LOGIND_EVENT_LINE_BYTES => Err(SessionEventError::LineTooLong),
        _ => Ok(()),
    }
}

fn parse_logind_properties(value: &str) -> Result<SessionSnapshot, SessionProbeError> {
    if value.len() > MAX_LOGINCTL_OUTPUT_BYTES {
        return Err(SessionProbeError::OutputTooLarge);
    }
    let properties: HashMap<_, _> = value
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let active = parse_bool(required(&properties, "Active")?)?;
    let locked = parse_bool(required(&properties, "LockedHint")?)?;
    Ok(SessionSnapshot {
        active,
        locked,
        idle: false,
    })
}

fn required<'a>(
    properties: &'a HashMap<&str, &str>,
    name: &'static str,
) -> Result<&'a str, SessionProbeError> {
    properties
        .get(name)
        .copied()
        .ok_or(SessionProbeError::MissingProperty(name))
}

fn parse_bool(value: &str) -> Result<bool, SessionProbeError> {
    match value {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(SessionProbeError::InvalidBoolean(value.to_owned())),
    }
}

#[derive(Debug, Error)]
pub enum SessionProbeError {
    #[error("XDG_SESSION_ID is not set")]
    MissingSessionId,
    #[error("session id contains unsupported characters")]
    InvalidSessionId,
    #[error("failed to execute loginctl: {0}")]
    Io(#[from] io::Error),
    #[error("loginctl output is not UTF-8: {0}")]
    Utf8(std::string::FromUtf8Error),
    #[error("loginctl failed with code {code:?}: {stderr}")]
    CommandFailed { code: Option<i32>, stderr: String },
    #[error("loginctl did not finish within 2 seconds")]
    Timeout,
    #[error("loginctl output exceeds 64 KiB")]
    OutputTooLarge,
    #[error("loginctl omitted {0}")]
    MissingProperty(&'static str),
    #[error("loginctl returned an invalid {0}")]
    InvalidProperty(&'static str),
    #[error("loginctl returned invalid boolean {0:?}")]
    InvalidBoolean(String),
}

#[derive(Debug, Error)]
pub enum SessionEventError {
    #[error("session id contains unsupported characters")]
    InvalidSessionId,
    #[error("failed to execute or read gdbus: {0}")]
    Io(#[from] io::Error),
    #[error("logind event monitor output is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("logind event monitor did not expose stdout")]
    MissingStdout,
    #[error("logind event monitor ended (exit code: {0:?})")]
    StreamEnded(Option<i32>),
    #[error("logind event line exceeds 64 KiB")]
    LineTooLong,
    #[error("logind event monitor returned an invalid line")]
    InvalidEventLine,
    #[error("logind event monitor returned an unexpected object path")]
    UnexpectedObjectPath,
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn collect_bounded(
    child: &mut Child,
    timeout: Duration,
    limit: usize,
) -> Result<BoundedOutput, SessionProbeError> {
    let mut stdout = child.stdout.take().ok_or_else(|| {
        SessionProbeError::Io(io::Error::other("loginctl stdout pipe is unavailable"))
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        SessionProbeError::Io(io::Error::other("loginctl stderr pipe is unavailable"))
    })?;
    set_nonblocking(stdout.as_raw_fd())?;
    set_nonblocking(stderr.as_raw_fd())?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut buffer = [0_u8; 4 * 1024];

    loop {
        let used = stdout_bytes.len().saturating_add(stderr_bytes.len());
        if used > limit {
            terminate(child);
            return Err(SessionProbeError::OutputTooLarge);
        }
        let mut budget = limit - used;
        if let Err(error) = drain(&mut stdout, &mut stdout_bytes, &mut buffer, &mut budget)
            .and_then(|_| drain(&mut stderr, &mut stderr_bytes, &mut buffer, &mut budget))
        {
            terminate(child);
            return Err(error);
        }
        if let Some(status) = child.try_wait()? {
            let mut final_budget = limit
                .checked_sub(stdout_bytes.len().saturating_add(stderr_bytes.len()))
                .ok_or(SessionProbeError::OutputTooLarge)?;
            drain(
                &mut stdout,
                &mut stdout_bytes,
                &mut buffer,
                &mut final_budget,
            )?;
            drain(
                &mut stderr,
                &mut stderr_bytes,
                &mut buffer,
                &mut final_budget,
            )?;
            return Ok(BoundedOutput {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            terminate(child);
            return Err(SessionProbeError::Timeout);
        }
        poll_pipes_until(stdout.as_raw_fd(), stderr.as_raw_fd(), deadline)?;
    }
}

fn drain(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    buffer: &mut [u8],
    remaining: &mut usize,
) -> Result<(), SessionProbeError> {
    loop {
        let read_limit = if *remaining == 0 {
            1
        } else {
            buffer.len().min(remaining.saturating_add(1))
        };
        match reader.read(&mut buffer[..read_limit]) {
            Ok(0) => return Ok(()),
            Ok(count) if count > *remaining => return Err(SessionProbeError::OutputTooLarge),
            Ok(count) => {
                output.extend_from_slice(&buffer[..count]);
                *remaining -= count;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    // SAFETY: `fd` is a live child pipe descriptor for both fcntl operations.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL only changes status flags on the valid descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn poll_pipes_until(stdout_fd: i32, stderr_fd: i32, deadline: Instant) -> io::Result<()> {
    let mut descriptors = [
        libc::pollfd {
            fd: stdout_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
        libc::pollfd {
            fd: stderr_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
    ];
    loop {
        let Some(timeout_ms) = remaining_poll_millis(deadline) else {
            return Ok(());
        };
        // SAFETY: `descriptors` is a valid two-element pollfd array for this call.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, timeout_ms) };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn poll_readable_until(fd: i32, deadline: Instant) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let Some(timeout_ms) = remaining_poll_millis(deadline) else {
            return Ok(false);
        };
        // SAFETY: `descriptor` is a valid one-element pollfd array for this call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return Ok(result > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn remaining_poll_millis(deadline: Instant) -> Option<i32> {
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let millis = remaining.as_millis();
    Some(millis.clamp(1, i32::MAX as u128) as i32)
}

fn terminate(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event_stream(mut command: Command) -> LogindEventStream {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        set_nonblocking(stdout.as_raw_fd()).unwrap();
        LogindEventStream {
            child,
            stdout,
            input: Vec::new(),
            object_path: "/org/freedesktop/login1/session/c1".to_owned(),
        }
    }

    #[test]
    fn parses_authoritative_active_and_lock_properties() {
        let snapshot = parse_logind_properties("Active=yes\nLockedHint=no\n").unwrap();
        assert!(snapshot.active);
        assert!(!snapshot.locked);
        assert!(!snapshot.idle);
        assert!(snapshot.permits_accounting());
    }

    #[test]
    fn missing_fact_is_an_error_not_a_default() {
        let error = parse_logind_properties("Active=yes\n").unwrap_err();
        assert!(matches!(
            error,
            SessionProbeError::MissingProperty("LockedHint")
        ));
    }

    #[test]
    fn session_id_and_parser_input_are_bounded() {
        assert!(matches!(
            LogindProbe::new("a".repeat(MAX_SESSION_ID_BYTES + 1)),
            Err(SessionProbeError::InvalidSessionId)
        ));
        assert!(matches!(
            parse_logind_properties(&"a".repeat(MAX_LOGINCTL_OUTPUT_BYTES + 1)),
            Err(SessionProbeError::OutputTooLarge)
        ));
    }

    #[test]
    fn bounded_collector_terminates_a_timed_out_child() {
        let mut child = Command::new("sleep")
            .arg("1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        assert!(matches!(
            collect_bounded(&mut child, Duration::from_millis(5), 1024),
            Err(SessionProbeError::Timeout)
        ));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn bounded_collector_rejects_output_above_the_exact_cap() {
        let mut child = Command::new("head")
            .args(["-c", "1025", "/dev/zero"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        assert!(matches!(
            collect_bounded(&mut child, Duration::from_secs(1), 1024),
            Err(SessionProbeError::OutputTooLarge)
        ));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn logind_object_path_uses_systemd_path_escaping() {
        assert_eq!(
            logind_session_object_path("session-1_test"),
            "/org/freedesktop/login1/session/session_2d1_5ftest"
        );
        assert_eq!(
            logind_session_object_path("1"),
            "/org/freedesktop/login1/session/_31"
        );
    }

    #[test]
    fn event_stream_ignores_gdbus_banner_and_reports_accounting_property_edges() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf '%s\\n' 'Monitoring signals on object /org/freedesktop/login1/session/c1 owned by org.freedesktop.login1' 'The name org.freedesktop.login1 is owned by :1.8' \"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {'LockedHint': <true>}, @as [])\"",
        ]);
        let mut stream = test_event_stream(command);
        assert!(stream.poll_changed(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn event_parser_ignores_unrelated_session_property_changes() {
        let path = "/org/freedesktop/login1/session/c1";
        assert!(
            !parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {'IdleHint': <true>}, @as [])",
                path,
            )
            .unwrap()
        );
        assert!(
            !parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {}, @as [])",
                path,
            )
            .unwrap()
        );
        assert!(
            !parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {'Desktop': <'Active'>}, @as [])",
                path,
            )
            .unwrap()
        );
    }

    #[test]
    fn event_parser_accepts_changed_or_invalidated_accounting_properties() {
        let path = "/org/freedesktop/login1/session/c1";
        assert!(
            parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {'Active': <false>}, @as [])",
                path,
            )
            .unwrap()
        );
        assert!(
            parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.DBus.Properties.PropertiesChanged ('org.freedesktop.login1.Session', {}, ['LockedHint'])",
                path,
            )
            .unwrap()
        );
    }

    #[test]
    fn event_parser_ignores_other_signals_and_rejects_wrong_paths() {
        let path = "/org/freedesktop/login1/session/c1";
        assert!(
            !parse_logind_event_line(
                b"/org/freedesktop/login1/session/c1: org.freedesktop.login1.Session.Lock ()",
                path,
            )
            .unwrap()
        );
        assert!(matches!(
            parse_logind_event_line(
                b"/org/freedesktop/login1/session/c2: org.freedesktop.DBus.Properties.PropertiesChanged ()",
                path,
            ),
            Err(SessionEventError::UnexpectedObjectPath)
        ));
    }

    #[test]
    fn event_stream_bounds_unterminated_input() {
        let mut command = Command::new("head");
        command.args([
            "-c",
            &(MAX_LOGIND_EVENT_LINE_BYTES + 1).to_string(),
            "/dev/zero",
        ]);
        let mut stream = test_event_stream(command);
        assert!(matches!(
            stream.poll_changed(Duration::from_secs(1)),
            Err(SessionEventError::LineTooLong)
        ));
    }
}
