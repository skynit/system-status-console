use std::collections::HashMap;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;

pub const MAX_NIRI_LINE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NIRI_WINDOWS: usize = 4_096;
pub const MAX_NIRI_APP_ID_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowIdentity {
    pub window_id: u64,
    pub app_id: String,
    pub pid: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NiriUpdate {
    FocusChanged(Option<WindowIdentity>),
    StateChanged,
    Ignored,
}

#[derive(Debug, Default)]
pub struct NiriState {
    windows: HashMap<u64, RawWindow>,
    focused_id: Option<u64>,
}

impl NiriState {
    pub fn apply_json_line(&mut self, line: &str) -> Result<NiriUpdate, NiriError> {
        if line.len() > MAX_NIRI_LINE_BYTES {
            return Err(NiriError::LineTooLong);
        }
        let value: serde_json::Value = serde_json::from_str(line)?;
        let Some(event_name) = value.as_object().and_then(|object| object.keys().next()) else {
            return Err(NiriError::InvalidEnvelope);
        };
        if !matches!(
            event_name.as_str(),
            "WindowsChanged" | "WindowOpenedOrChanged" | "WindowClosed" | "WindowFocusChanged"
        ) {
            return Ok(NiriUpdate::Ignored);
        }
        let event: RawEvent = serde_json::from_value(value)?;
        Ok(match event {
            RawEvent::WindowsChanged { windows } => {
                if windows.len() > MAX_NIRI_WINDOWS {
                    return Err(NiriError::TooManyWindows);
                }
                validate_windows(&windows)?;
                self.focused_id = windows
                    .iter()
                    .find(|window| window.is_focused)
                    .map(|window| window.id);
                self.windows = windows
                    .into_iter()
                    .map(|window| (window.id, window))
                    .collect();
                NiriUpdate::FocusChanged(self.focused_identity())
            }
            RawEvent::WindowOpenedOrChanged { window } => {
                validate_window(&window)?;
                let id = window.id;
                let is_focused = window.is_focused;
                if !self.windows.contains_key(&id) && self.windows.len() >= MAX_NIRI_WINDOWS {
                    return Err(NiriError::TooManyWindows);
                }
                self.windows.insert(id, window);
                if is_focused {
                    self.focused_id = Some(id);
                    NiriUpdate::FocusChanged(self.focused_identity())
                } else if self.focused_id == Some(id) {
                    // A following explicit focus event may identify the replacement. Stop
                    // attribution until that fact is available.
                    self.focused_id = None;
                    NiriUpdate::FocusChanged(None)
                } else {
                    NiriUpdate::StateChanged
                }
            }
            RawEvent::WindowClosed { id } => {
                self.windows.remove(&id);
                if self.focused_id == Some(id) {
                    self.focused_id = None;
                    NiriUpdate::FocusChanged(None)
                } else {
                    NiriUpdate::StateChanged
                }
            }
            RawEvent::WindowFocusChanged { id } => {
                self.focused_id = id;
                // A focus id can arrive before its window update. Returning None makes
                // the accounting state conservative until identity becomes known.
                NiriUpdate::FocusChanged(self.focused_identity())
            }
        })
    }

    pub fn focused_identity(&self) -> Option<WindowIdentity> {
        let window = self.windows.get(&self.focused_id?)?;
        let app_id = window
            .app_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        Some(WindowIdentity {
            window_id: window.id,
            app_id: app_id.to_owned(),
            pid: window.pid,
        })
    }
}

#[derive(Debug)]
pub struct NiriEventStream {
    child: Child,
    stdout: ChildStdout,
    state: NiriState,
    input: Vec<u8>,
}

impl NiriEventStream {
    pub fn spawn() -> Result<Self, NiriError> {
        let mut child = Command::new("niri")
            .args(["msg", "--json", "event-stream"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or(NiriError::MissingStdout)?;
        set_nonblocking(&stdout)?;
        Ok(Self {
            child,
            stdout,
            state: NiriState::default(),
            input: Vec::new(),
        })
    }

    /// Waits for at most `timeout` for one complete event line.
    ///
    /// A partial line is retained for the next call, but can never exceed 4 MiB.
    /// Returning `Ok(None)` gives the owner a cancellation/checkpoint boundary.
    pub fn poll_update(&mut self, timeout: Duration) -> Result<Option<NiriUpdate>, NiriError> {
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
                let line = std::str::from_utf8(&line).map_err(NiriError::Utf8)?;
                return self.state.apply_json_line(line).map(Some);
            }
            validate_buffered_line(&self.input)?;

            if !poll_readable_until(self.stdout.as_raw_fd(), deadline)? {
                return Ok(None);
            }

            let mut chunk = [0_u8; 16 * 1024];
            loop {
                match self.stdout.read(&mut chunk) {
                    Ok(0) => {
                        if self.input.contains(&b'\n') {
                            break;
                        }
                        let status = self.child.try_wait()?;
                        return Err(NiriError::StreamEnded(
                            status.and_then(|value| value.code()),
                        ));
                    }
                    Ok(count) => {
                        self.input.extend_from_slice(&chunk[..count]);
                        validate_buffered_line(&self.input)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    pub fn state(&self) -> &NiriState {
        &self.state
    }
}

impl Drop for NiriEventStream {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug, Error)]
pub enum NiriError {
    #[error("failed to execute or read niri: {0}")]
    Io(#[from] io::Error),
    #[error("invalid niri event JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("niri event stream produced non-UTF-8 input")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("niri event JSON must be an externally tagged object")]
    InvalidEnvelope,
    #[error("niri event stream did not expose stdout")]
    MissingStdout,
    #[error("niri event stream ended (exit code: {0:?})")]
    StreamEnded(Option<i32>),
    #[error("niri event line exceeds 4 MiB")]
    LineTooLong,
    #[error("niri state exceeds 4096 windows")]
    TooManyWindows,
    #[error("niri app_id exceeds 512 bytes")]
    AppIdTooLong,
}

#[derive(Clone, Debug, Deserialize)]
enum RawEvent {
    WindowsChanged { windows: Vec<RawWindow> },
    WindowOpenedOrChanged { window: RawWindow },
    WindowClosed { id: u64 },
    WindowFocusChanged { id: Option<u64> },
}

#[derive(Clone, Debug, Deserialize)]
struct RawWindow {
    id: u64,
    app_id: Option<String>,
    pid: Option<i32>,
    is_focused: bool,
}

fn validate_windows(windows: &[RawWindow]) -> Result<(), NiriError> {
    windows.iter().try_for_each(validate_window)
}

fn validate_window(window: &RawWindow) -> Result<(), NiriError> {
    if window
        .app_id
        .as_ref()
        .is_some_and(|app_id| app_id.len() > MAX_NIRI_APP_ID_BYTES)
    {
        return Err(NiriError::AppIdTooLong);
    }
    Ok(())
}

fn validate_buffered_line(input: &[u8]) -> Result<(), NiriError> {
    match input.iter().position(|byte| *byte == b'\n') {
        Some(line_bytes) if line_bytes > MAX_NIRI_LINE_BYTES => Err(NiriError::LineTooLong),
        None if input.len() > MAX_NIRI_LINE_BYTES => Err(NiriError::LineTooLong),
        _ => Ok(()),
    }
}

fn set_nonblocking(stdout: &ChildStdout) -> io::Result<()> {
    let fd = stdout.as_raw_fd();
    // SAFETY: `fd` is owned by `stdout` and remains valid for both fcntl calls.
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

fn poll_readable_until(fd: i32, deadline: Instant) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(false);
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stream(mut command: Command) -> NiriEventStream {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        set_nonblocking(&stdout).unwrap();
        NiriEventStream {
            child,
            stdout,
            state: NiriState::default(),
            input: Vec::new(),
        }
    }

    #[test]
    fn follows_snapshot_focus_and_explicit_focus_events() {
        let mut state = NiriState::default();
        let initial = r#"{"WindowsChanged":{"windows":[{"id":7,"app_id":"kitty","pid":42,"is_focused":true,"title":"ignored"},{"id":8,"app_id":"firefox","pid":77,"is_focused":false}]}}"#;
        assert_eq!(
            state.apply_json_line(initial).unwrap(),
            NiriUpdate::FocusChanged(Some(WindowIdentity {
                window_id: 7,
                app_id: "kitty".into(),
                pid: Some(42),
            }))
        );
        assert_eq!(
            state
                .apply_json_line(r#"{"WindowFocusChanged":{"id":8}}"#)
                .unwrap(),
            NiriUpdate::FocusChanged(Some(WindowIdentity {
                window_id: 8,
                app_id: "firefox".into(),
                pid: Some(77),
            }))
        );
    }

    #[test]
    fn unknown_focus_and_missing_app_id_are_not_attributed() {
        let mut state = NiriState::default();
        assert_eq!(
            state
                .apply_json_line(r#"{"WindowFocusChanged":{"id":99}}"#)
                .unwrap(),
            NiriUpdate::FocusChanged(None)
        );
        assert_eq!(
            state
                .apply_json_line(
                    r#"{"WindowOpenedOrChanged":{"window":{"id":99,"app_id":null,"pid":2,"is_focused":true}}}"#,
                )
                .unwrap(),
            NiriUpdate::FocusChanged(None)
        );
    }

    #[test]
    fn ignores_unrelated_forward_compatible_events() {
        let mut state = NiriState::default();
        assert_eq!(
            state
                .apply_json_line(r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{}}}"#)
                .unwrap(),
            NiriUpdate::Ignored
        );
    }

    #[test]
    fn rejects_oversized_state_and_application_ids() {
        let mut state = NiriState::default();
        let app_id = "a".repeat(MAX_NIRI_APP_ID_BYTES + 1);
        let line = format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{{"id":1,"app_id":"{app_id}","pid":2,"is_focused":true}}}}}}"#
        );
        assert!(matches!(
            state.apply_json_line(&line),
            Err(NiriError::AppIdTooLong)
        ));

        let windows = (0..=MAX_NIRI_WINDOWS)
            .map(|id| format!(r#"{{"id":{id},"app_id":"a","pid":2,"is_focused":false}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let line = format!(r#"{{"WindowsChanged":{{"windows":[{windows}]}}}}"#);
        assert!(matches!(
            state.apply_json_line(&line),
            Err(NiriError::TooManyWindows)
        ));
    }

    #[test]
    fn polling_has_a_cancellation_boundary_and_keeps_exit_buffered_event() {
        let mut wait_command = Command::new("sleep");
        wait_command.arg("1");
        let mut waiting = test_stream(wait_command);
        assert_eq!(waiting.poll_update(Duration::from_millis(5)).unwrap(), None);

        let mut event_command = Command::new("printf");
        event_command.args(["%s\n", r#"{"WindowFocusChanged":{"id":null}}"#]);
        let mut exited = test_stream(event_command);
        assert_eq!(
            exited.poll_update(Duration::from_secs(1)).unwrap(),
            Some(NiriUpdate::FocusChanged(None))
        );
    }
}
