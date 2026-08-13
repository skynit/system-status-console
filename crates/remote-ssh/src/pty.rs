use nix::{
    libc,
    pty::{Winsize, openpty},
};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const MAX_PTY_ROWS: u16 = 1_000;
pub const MAX_PTY_COLUMNS: u16 = 1_000;
pub const MAX_PTY_PIXEL_DIMENSION: u16 = 32_767;
const CLOSE_GRACE_PERIOD: Duration = Duration::from_millis(250);
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PtySize {
    rows: u16,
    columns: u16,
    pixel_width: u16,
    pixel_height: u16,
}

impl PtySize {
    pub fn new(rows: u16, columns: u16) -> Result<Self, PtyError> {
        Self::with_pixels(rows, columns, 0, 0)
    }

    pub fn with_pixels(
        rows: u16,
        columns: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<Self, PtyError> {
        if rows == 0
            || rows > MAX_PTY_ROWS
            || columns == 0
            || columns > MAX_PTY_COLUMNS
            || pixel_width > MAX_PTY_PIXEL_DIMENSION
            || pixel_height > MAX_PTY_PIXEL_DIMENSION
        {
            return Err(PtyError::InvalidSize {
                rows,
                columns,
                pixel_width,
                pixel_height,
            });
        }
        Ok(Self {
            rows,
            columns,
            pixel_width,
            pixel_height,
        })
    }

    pub fn rows(self) -> u16 {
        self.rows
    }

    pub fn columns(self) -> u16 {
        self.columns
    }

    pub fn pixel_width(self) -> u16 {
        self.pixel_width
    }

    pub fn pixel_height(self) -> u16 {
        self.pixel_height
    }

    fn winsize(self) -> Winsize {
        Winsize {
            ws_row: self.rows,
            ws_col: self.columns,
            ws_xpixel: self.pixel_width,
            ws_ypixel: self.pixel_height,
        }
    }
}

#[derive(Debug, Error)]
pub enum PtyError {
    #[error(
        "invalid PTY size rows={rows}, columns={columns}, pixel_width={pixel_width}, pixel_height={pixel_height}"
    )]
    InvalidSize {
        rows: u16,
        columns: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    #[error("failed to allocate PTY: {0}")]
    Allocate(#[source] nix::Error),
    #[error("failed to clone PTY slave: {0}")]
    CloneSlave(#[source] io::Error),
    #[error("failed to spawn fixed OpenSSH process: {0}")]
    Spawn(#[source] io::Error),
    #[error("failed to make PTY output non-blocking: {0}")]
    ConfigureNonblocking(#[source] io::Error),
    #[error("failed to resize PTY: {0}")]
    Resize(#[source] io::Error),
    #[error("failed to inspect PTY child: {0}")]
    Inspect(#[source] io::Error),
    #[error("failed to close PTY child: {0}")]
    Close(#[source] io::Error),
}

pub(crate) struct PtySession {
    master: File,
    child: Child,
    close_requested: bool,
}

impl PtySession {
    pub(crate) fn spawn(command: &mut Command, size: PtySize) -> Result<Self, PtyError> {
        let opened = openpty(&size.winsize(), None).map_err(PtyError::Allocate)?;
        let master = File::from(opened.master);
        set_nonblocking(&master)?;
        let slave = File::from(opened.slave);
        let stdin = slave.try_clone().map_err(PtyError::CloneSlave)?;
        let stdout = slave.try_clone().map_err(PtyError::CloneSlave)?;
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(slave));

        // SAFETY: only async-signal-safe libc calls run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command.spawn().map_err(PtyError::Spawn)?;
        Ok(Self {
            master,
            child,
            close_requested: false,
        })
    }

    pub(crate) fn process_id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        let winsize = size.winsize();
        // SAFETY: master is a live PTY fd and winsize points to an initialized value.
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if result == -1 {
            return Err(PtyError::Resize(io::Error::last_os_error()));
        }
        Ok(())
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Inspect)
    }

    pub(crate) fn close(&mut self) -> Result<ExitStatus, PtyError> {
        self.close_requested = true;
        if let Some(status) = self.child.try_wait().map_err(PtyError::Inspect)? {
            return Ok(status);
        }

        let result = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGHUP) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(PtyError::Close(error));
            }
        }

        let deadline = Instant::now() + CLOSE_GRACE_PERIOD;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().map_err(PtyError::Inspect)? {
                return Ok(status);
            }
            thread::sleep(CLOSE_POLL_INTERVAL);
        }

        self.child.kill().map_err(PtyError::Close)?;
        self.child.wait().map_err(PtyError::Close)
    }

    pub(crate) fn close_requested(&self) -> bool {
        self.close_requested
    }
}

fn set_nonblocking(master: &File) -> Result<(), PtyError> {
    let descriptor = master.as_raw_fd();
    // SAFETY: descriptor is owned by master and F_GETFL does not mutate memory.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(PtyError::ConfigureNonblocking(io::Error::last_os_error()));
    }
    // SAFETY: descriptor is owned by master and flags preserves its existing status flags.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(PtyError::ConfigureNonblocking(io::Error::last_os_error()));
    }
    Ok(())
}

impl Read for PtySession {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.master.read(buffer)
    }
}

impl Write for PtySession {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.master.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.master.flush()
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn pty_lifecycle_supports_output_resize_and_exit() {
        let size = PtySize::new(24, 80).expect("size");
        let mut command = Command::new("/usr/bin/printf");
        command.arg("pty-ready");
        let mut session = PtySession::spawn(&mut command, size).expect("spawn fixture");
        session
            .resize(PtySize::new(40, 120).expect("resized dimensions"))
            .expect("resize");

        let mut buffer = [0_u8; 64];
        let deadline = Instant::now() + Duration::from_secs(2);
        let count = loop {
            match session.read(&mut buffer) {
                Ok(count) => break count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "PTY output did not arrive");
                    std::thread::yield_now();
                }
                Err(error) => panic!("read PTY: {error}"),
            }
        };
        assert_eq!(&buffer[..count], b"pty-ready");

        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = session.try_wait().expect("poll") {
                break status;
            }
            assert!(Instant::now() < deadline, "PTY child did not exit");
            std::thread::yield_now();
        };
        assert!(status.success());
    }

    #[test]
    fn close_marks_lifecycle_and_reaps_child() {
        let mut command = Command::new("/usr/bin/cat");
        let mut session = PtySession::spawn(&mut command, PtySize::new(24, 80).expect("size"))
            .expect("spawn fixture");
        assert!(!session.close_requested());
        let _ = session.close().expect("close");
        assert!(session.close_requested());
        assert!(session.try_wait().expect("reaped").is_some());
    }

    #[test]
    fn pty_size_rejects_zero_and_excessive_dimensions() {
        assert!(matches!(
            PtySize::new(0, 80),
            Err(PtyError::InvalidSize { .. })
        ));
        assert!(matches!(
            PtySize::new(MAX_PTY_ROWS + 1, 80),
            Err(PtyError::InvalidSize { .. })
        ));
        assert!(matches!(
            PtySize::with_pixels(24, 80, u16::MAX, 0),
            Err(PtyError::InvalidSize { .. })
        ));
    }

    #[test]
    fn pty_master_is_nonblocking_when_no_output_is_available() {
        let mut command = Command::new("/usr/bin/cat");
        let mut session = PtySession::spawn(&mut command, PtySize::new(24, 80).expect("size"))
            .expect("spawn fixture");
        let mut buffer = [0_u8; 8];
        let error = session
            .read(&mut buffer)
            .expect_err("no output must not block");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        session.close().expect("close fixture");
    }
}
