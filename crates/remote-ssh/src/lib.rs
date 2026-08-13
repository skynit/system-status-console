#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("localdesk-remote-ssh requires Linux PTY and OpenSSH semantics");

mod askpass;
mod bridge;
mod openssh;
mod profile;
mod pty;
mod terminal_bridge;

pub use bridge::{JumpProfileResolver, NoJumpProfileResolver, SftpRemoteFileAdapter};
pub use openssh::{
    AdapterError, DisconnectReason, MAX_SFTP_STDERR_BYTES, MAX_SFTP_STDOUT_BYTES,
    MAX_TERMINAL_INPUT_BYTES, MAX_TERMINAL_OUTPUT_BYTES, MAX_TERMINAL_TRANSCRIPT_BYTES,
    OpenSshAdapter, SessionState, SftpOperation, SftpOutput, SftpSession, TERMINAL_CAPABILITIES,
    TerminalCapabilities, TerminalError, TerminalOutput, TerminalRead, TerminalSession,
    TerminalStatus,
};
pub use profile::{Authentication, Endpoint, HostKeyPolicy, HostTrust, ProfileError, SshProfile};
pub use pty::{PtyError, PtySize};
pub use terminal_bridge::{SshTerminalAdapter, SshTerminalSession};
