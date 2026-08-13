//! Structured, system-first SMB2/3 file access through Samba `libsmbclient`,
//! plus bounded `smbclient` diagnostics.
//!
//! Remote command output is intentionally opaque. Consumers must not turn the
//! human-facing output into production share or file entities.

mod native;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_PROTOCOL: &str = "client max protocol=SMB3";
const IPC_MAX_PROTOCOL: &str = "client ipc max protocol=SMB3";
const DEFAULT_OUTPUT_LIMIT: usize = 256 * 1024;
const MIN_OUTPUT_LIMIT: usize = 1024;
const MAX_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Healthy,
    Degraded,
    Unsupported,
    Unreachable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReauthenticationMode {
    FreshProcess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputContract {
    OpaqueHumanOutput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityReport {
    pub status: CapabilityStatus,
    pub reason: String,
    pub client_version: Option<String>,
    pub dialects: [&'static str; 2],
    pub smb1_enabled: bool,
    pub supports_workgroup_domain: bool,
    pub supports_kerberos: bool,
    pub supports_signing: bool,
    pub supports_encryption: bool,
    pub supports_share_browse_diagnostic: bool,
    pub reauthentication: ReauthenticationMode,
    pub output_contract: OutputContract,
}

impl CapabilityReport {
    fn base(status: CapabilityStatus, reason: String, client_version: Option<String>) -> Self {
        let available = status == CapabilityStatus::Healthy;
        Self {
            status,
            reason,
            client_version,
            dialects: ["SMB2", "SMB3"],
            smb1_enabled: false,
            supports_workgroup_domain: available,
            supports_kerberos: available,
            supports_signing: available,
            supports_encryption: available,
            supports_share_browse_diagnostic: available,
            reauthentication: ReauthenticationMode::FreshProcess,
            output_contract: OutputContract::OpaqueHumanOutput,
        }
    }
}

#[derive(Eq, PartialEq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().into_bytes())
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Authority {
    Default,
    Workgroup(String),
    Domain(String),
}

#[derive(Eq, PartialEq)]
pub enum Authentication {
    Password {
        username: String,
        password: Secret,
        authority: Authority,
    },
    Kerberos {
        realm: Option<String>,
        ccache: Option<PathBuf>,
    },
}

impl fmt::Debug for Authentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password {
                username,
                password: _,
                authority,
            } => formatter
                .debug_struct("Password")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .field("authority", authority)
                .finish(),
            Self::Kerberos { realm, ccache } => formatter
                .debug_struct("Kerberos")
                .field("realm", realm)
                .field("ccache", ccache)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protection {
    Negotiated,
    Signing,
    Encryption,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinimumDialect {
    Smb2,
    Smb3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialRevision {
    pub expected: u64,
    pub active: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeTicket {
    pub remote_path: String,
    pub local_partial_path: PathBuf,
    pub verified_offset: u64,
    pub observed_local_len: u64,
    pub verified_remote_len: u64,
    pub remote_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticOperation {
    BrowseShares {
        server: String,
    },
    InspectShare {
        server: String,
        share: String,
    },
    Reauthenticate {
        server: String,
        share: String,
    },
    ResumeDownload {
        server: String,
        share: String,
        ticket: ResumeTicket,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct DiagnosticRequest {
    pub authentication: Authentication,
    pub protection: Protection,
    pub credential_revision: CredentialRevision,
    pub operation: DiagnosticOperation,
    pub port: u16,
    pub minimum_dialect: MinimumDialect,
    pub operation_timeout: Duration,
    pub process_deadline: Duration,
    pub output_limit: usize,
}

impl DiagnosticRequest {
    pub fn new(
        authentication: Authentication,
        protection: Protection,
        credential_revision: CredentialRevision,
        operation: DiagnosticOperation,
    ) -> Self {
        Self {
            authentication,
            protection,
            credential_revision,
            operation,
            port: 445,
            minimum_dialect: MinimumDialect::Smb2,
            operation_timeout: Duration::from_secs(20),
            process_deadline: Duration::from_secs(30),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Unsupported,
    Unreachable,
    Conflict,
    InvalidRequest,
    TimedOut,
    ClientRejected,
    Io,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticError {
    pub kind: ErrorKind,
    pub reason: String,
}

impl DiagnosticError {
    fn new(kind: ErrorKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for DiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.reason)
    }
}

impl std::error::Error for DiagnosticError {}

struct SensitiveEnvironment {
    key: &'static str,
    value: Secret,
}

impl fmt::Debug for SensitiveEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveEnvironment")
            .field("key", &self.key)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

pub struct DiagnosticPlan {
    program: PathBuf,
    args: Vec<OsString>,
    sensitive_environment: Option<SensitiveEnvironment>,
    process_deadline: Duration,
    output_limit: usize,
    operation: OperationKind,
}

impl fmt::Debug for DiagnosticPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiagnosticPlan")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("sensitive_environment", &self.sensitive_environment)
            .field("process_deadline", &self.process_deadline)
            .field("output_limit", &self.output_limit)
            .field("operation", &self.operation)
            .finish()
    }
}

impl DiagnosticPlan {
    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub fn sensitive_environment_key(&self) -> Option<&'static str> {
        self.sensitive_environment.as_ref().map(|entry| entry.key)
    }

    pub fn process_deadline(&self) -> Duration {
        self.process_deadline
    }

    pub fn output_limit(&self) -> usize {
        self.output_limit
    }

    pub fn operation(&self) -> OperationKind {
        self.operation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    BrowseShares,
    InspectShare,
    Reauthenticate,
    ResumeDownload,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueOutput {
    pub bytes: Vec<u8>,
    pub total_bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticOutcome {
    Succeeded,
    ClientRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticResult {
    pub operation: OperationKind,
    pub outcome: DiagnosticOutcome,
    pub exit_code: Option<i32>,
    pub stdout: OpaqueOutput,
    pub stderr: OpaqueOutput,
}

pub fn probe_smbclient(program: impl AsRef<Path>) -> CapabilityReport {
    match Command::new(program.as_ref()).arg("--version").output() {
        Ok(output) => {
            capability_from_version_output(output.status.success(), &output.stdout, &output.stderr)
        }
        Err(error) => {
            let kind = classify_io_error(error.kind());
            let status = match kind {
                ErrorKind::Unsupported => CapabilityStatus::Unsupported,
                ErrorKind::Unreachable => CapabilityStatus::Unreachable,
                _ => CapabilityStatus::Degraded,
            };
            CapabilityReport::base(status, format!("smbclient probe failed: {error}"), None)
        }
    }
}

pub fn capability_from_version_output(
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> CapabilityReport {
    if !success {
        return CapabilityReport::base(
            CapabilityStatus::Degraded,
            "smbclient --version returned a non-zero status".to_owned(),
            None,
        );
    }

    let candidate = String::from_utf8_lossy(stdout).trim().to_owned();
    if candidate.is_empty() {
        let stderr_candidate = String::from_utf8_lossy(stderr).trim().to_owned();
        return CapabilityReport::base(
            CapabilityStatus::Degraded,
            "smbclient version output was empty".to_owned(),
            (!stderr_candidate.is_empty()).then_some(stderr_candidate),
        );
    }

    CapabilityReport::base(
        CapabilityStatus::Healthy,
        "installed smbclient supports the diagnostic POC; remote capability is not probed"
            .to_owned(),
        Some(candidate),
    )
}

pub fn build_plan(
    program: impl Into<PathBuf>,
    request: DiagnosticRequest,
) -> Result<DiagnosticPlan, DiagnosticError> {
    validate_request(&request)?;

    let operation_timeout = request.operation_timeout.as_secs();
    let minimum_dialect = match request.minimum_dialect {
        MinimumDialect::Smb2 => "SMB2",
        MinimumDialect::Smb3 => "SMB3",
    };
    let mut args = vec![
        OsString::from(format!("--option=client min protocol={minimum_dialect}")),
        OsString::from(format!("--option={MAX_PROTOCOL}")),
        OsString::from(format!(
            "--option=client ipc min protocol={minimum_dialect}"
        )),
        OsString::from(format!("--option={IPC_MAX_PROTOCOL}")),
        OsString::from(format!("--timeout={operation_timeout}")),
        OsString::from(format!("--port={}", request.port)),
    ];
    match request.protection {
        Protection::Negotiated => {}
        Protection::Signing => args.push(OsString::from("--client-protection=sign")),
        Protection::Encryption => args.push(OsString::from("--client-protection=encrypt")),
    }

    let sensitive_environment = match request.authentication {
        Authentication::Password {
            username,
            password,
            authority,
        } => {
            let authority = match authority {
                Authority::Default => None,
                Authority::Workgroup(value) => {
                    args.push(OsString::from(format!("--workgroup={value}")));
                    None
                }
                Authority::Domain(value) => {
                    args.push(OsString::from(format!("--workgroup={value}")));
                    Some(value)
                }
            };
            let qualified_user = authority
                .map(|domain| format!("{domain}\\{username}"))
                .unwrap_or(username);
            args.push(OsString::from(format!("--user={qualified_user}")));
            Some(SensitiveEnvironment {
                key: "PASSWD",
                value: password,
            })
        }
        Authentication::Kerberos { realm, ccache } => {
            args.push(OsString::from("--use-kerberos=required"));
            if let Some(realm) = realm {
                args.push(OsString::from(format!("--realm={realm}")));
            }
            if let Some(ccache) = ccache {
                args.push(OsString::from(format!(
                    "--use-krb5-ccache={}",
                    ccache.to_string_lossy()
                )));
            }
            None
        }
    };

    let operation = match request.operation {
        DiagnosticOperation::BrowseShares { server } => {
            args.push(OsString::from("--grepable"));
            args.push(OsString::from(format!("--list={server}")));
            OperationKind::BrowseShares
        }
        DiagnosticOperation::InspectShare { server, share } => {
            args.push(service_arg(&server, &share));
            args.push(OsString::from("--command=quit"));
            OperationKind::InspectShare
        }
        DiagnosticOperation::Reauthenticate { server, share } => {
            args.push(service_arg(&server, &share));
            args.push(OsString::from("--command=quit"));
            OperationKind::Reauthenticate
        }
        DiagnosticOperation::ResumeDownload {
            server,
            share,
            ticket,
        } => {
            args.push(service_arg(&server, &share));
            args.push(OsString::from(format!(
                "--command=reget {} {}",
                ticket.remote_path,
                ticket.local_partial_path.to_string_lossy()
            )));
            OperationKind::ResumeDownload
        }
    };

    Ok(DiagnosticPlan {
        program: program.into(),
        args,
        sensitive_environment,
        process_deadline: request.process_deadline,
        output_limit: request.output_limit,
        operation,
    })
}

pub fn execute(plan: DiagnosticPlan) -> Result<DiagnosticResult, DiagnosticError> {
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .env_remove("PASSWD")
        .env_remove("PASSWD_FD")
        .env_remove("PASSWD_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(environment) = &plan.sensitive_environment {
        command.env(
            environment.key,
            OsStr::from_bytes(environment.value.expose()),
        );
    }

    let mut child = command.spawn().map_err(|error| {
        DiagnosticError::new(
            classify_io_error(error.kind()),
            format!("failed to start smbclient: {error}"),
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DiagnosticError::new(ErrorKind::Io, "stdout pipe was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DiagnosticError::new(ErrorKind::Io, "stderr pipe was unavailable"))?;
    let stdout_reader = capture_bounded(stdout, plan.output_limit);
    let stderr_reader = capture_bounded(stderr, plan.output_limit);

    let started = Instant::now();
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < plan.process_deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(DiagnosticError::new(
                    ErrorKind::TimedOut,
                    "smbclient exceeded the process deadline and was terminated",
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DiagnosticError::new(
                    ErrorKind::Io,
                    format!("failed while waiting for smbclient: {error}"),
                ));
            }
        }
    };

    let stdout = join_capture(stdout_reader, "stdout")?;
    let stderr = join_capture(stderr_reader, "stderr")?;
    Ok(DiagnosticResult {
        operation: plan.operation,
        outcome: classify_exit_status(exit_status),
        exit_code: exit_status.code(),
        stdout,
        stderr,
    })
}

pub fn classify_io_error(kind: io::ErrorKind) -> ErrorKind {
    match kind {
        io::ErrorKind::NotFound => ErrorKind::Unsupported,
        io::ErrorKind::TimedOut => ErrorKind::TimedOut,
        _ => ErrorKind::Io,
    }
}

#[cfg(unix)]
pub fn classify_raw_exit(success: bool, _code: Option<i32>) -> DiagnosticOutcome {
    if success {
        DiagnosticOutcome::Succeeded
    } else {
        DiagnosticOutcome::ClientRejected
    }
}

#[cfg(not(unix))]
pub fn classify_raw_exit(success: bool, _code: Option<i32>) -> DiagnosticOutcome {
    if success {
        DiagnosticOutcome::Succeeded
    } else {
        DiagnosticOutcome::ClientRejected
    }
}

fn classify_exit_status(status: ExitStatus) -> DiagnosticOutcome {
    classify_raw_exit(status.success(), status.code())
}

fn capture_bounded<R>(mut reader: R, limit: usize) -> thread::JoinHandle<io::Result<OpaqueOutput>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(limit.min(8192));
        let mut total_bytes = 0usize;
        let mut buffer = [0u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total_bytes = total_bytes.saturating_add(read);
            let remaining = limit.saturating_sub(bytes.len());
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        Ok(OpaqueOutput {
            truncated: total_bytes > bytes.len(),
            bytes,
            total_bytes,
        })
    })
}

fn join_capture(
    handle: thread::JoinHandle<io::Result<OpaqueOutput>>,
    stream: &str,
) -> Result<OpaqueOutput, DiagnosticError> {
    handle
        .join()
        .map_err(|_| DiagnosticError::new(ErrorKind::Io, format!("{stream} reader panicked")))?
        .map_err(|error| {
            DiagnosticError::new(ErrorKind::Io, format!("failed reading {stream}: {error}"))
        })
}

fn validate_request(request: &DiagnosticRequest) -> Result<(), DiagnosticError> {
    if request.credential_revision.expected != request.credential_revision.active {
        return Err(DiagnosticError::new(
            ErrorKind::Conflict,
            format!(
                "credential revision conflict: expected {}, active {}",
                request.credential_revision.expected, request.credential_revision.active
            ),
        ));
    }
    if !(1..=300).contains(&request.operation_timeout.as_secs()) {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            "operation timeout must be between 1 and 300 seconds",
        ));
    }
    if request.port == 0 {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            "SMB port must be non-zero",
        ));
    }
    if request.process_deadline <= request.operation_timeout {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            "process deadline must be greater than the smbclient operation timeout",
        ));
    }
    if !(MIN_OUTPUT_LIMIT..=MAX_OUTPUT_LIMIT).contains(&request.output_limit) {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            "output limit must be between 1024 bytes and 1 MiB",
        ));
    }
    validate_authentication(&request.authentication)?;
    validate_operation(&request.operation)
}

fn validate_authentication(authentication: &Authentication) -> Result<(), DiagnosticError> {
    match authentication {
        Authentication::Password {
            username,
            password,
            authority,
        } => {
            validate_identity_atom("username", username)?;
            if password.expose().is_empty() {
                return Err(DiagnosticError::new(
                    ErrorKind::InvalidRequest,
                    "password must not be empty",
                ));
            }
            match authority {
                Authority::Default => Ok(()),
                Authority::Workgroup(value) => validate_identity_atom("workgroup", value),
                Authority::Domain(value) => validate_identity_atom("domain", value),
            }
        }
        Authentication::Kerberos { realm, ccache } => {
            if let Some(realm) = realm {
                validate_identity_atom("realm", realm)?;
            }
            if let Some(ccache) = ccache {
                validate_argv_path("Kerberos ccache", ccache)?;
            }
            Ok(())
        }
    }
}

fn validate_operation(operation: &DiagnosticOperation) -> Result<(), DiagnosticError> {
    match operation {
        DiagnosticOperation::BrowseShares { server } => validate_server(server),
        DiagnosticOperation::InspectShare { server, share }
        | DiagnosticOperation::Reauthenticate { server, share } => {
            validate_server(server)?;
            validate_share(share)
        }
        DiagnosticOperation::ResumeDownload {
            server,
            share,
            ticket,
        } => {
            validate_server(server)?;
            validate_share(share)?;
            validate_command_path("remote resume path", &ticket.remote_path, false)?;
            validate_safe_path("local partial path", &ticket.local_partial_path, true)?;
            if ticket.local_partial_path.extension() != Some(OsStr::new("part")) {
                return Err(DiagnosticError::new(
                    ErrorKind::InvalidRequest,
                    "resume destination must use the .part suffix",
                ));
            }
            if ticket.remote_identity.is_empty()
                || ticket.remote_identity.chars().any(char::is_control)
            {
                return Err(DiagnosticError::new(
                    ErrorKind::InvalidRequest,
                    "remote identity must be a non-empty printable value",
                ));
            }
            if ticket.verified_offset != ticket.observed_local_len {
                return Err(DiagnosticError::new(
                    ErrorKind::Conflict,
                    "local partial length changed after the resume ticket was issued",
                ));
            }
            if ticket.verified_offset == 0 || ticket.verified_offset >= ticket.verified_remote_len {
                return Err(DiagnosticError::new(
                    ErrorKind::InvalidRequest,
                    "resume offset must be greater than zero and smaller than remote length",
                ));
            }
            Ok(())
        }
    }
}

fn validate_server(value: &str) -> Result<(), DiagnosticError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            "server must be a hostname or IP literal without URI or option syntax",
        ));
    }
    Ok(())
}

fn validate_share(value: &str) -> Result<(), DiagnosticError> {
    validate_identity_atom("share", value)
}

fn validate_identity_atom(label: &str, value: &str) -> Result<(), DiagnosticError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} contains unsupported characters"),
        ));
    }
    Ok(())
}

fn validate_safe_path(
    label: &str,
    path: &Path,
    require_absolute: bool,
) -> Result<(), DiagnosticError> {
    if require_absolute && !path.is_absolute() {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} must be absolute"),
        ));
    }
    let value = path.to_str().ok_or_else(|| {
        DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} must be valid UTF-8"),
        )
    })?;
    validate_command_path(label, value, require_absolute)
}

fn validate_argv_path(label: &str, path: &Path) -> Result<(), DiagnosticError> {
    let value = path.to_str().ok_or_else(|| {
        DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} must be valid UTF-8"),
        )
    })?;
    if value.is_empty() || value.contains('\0') {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} must be a non-empty path without NUL"),
        ));
    }
    Ok(())
}

fn validate_command_path(
    label: &str,
    value: &str,
    allow_leading_slash: bool,
) -> Result<(), DiagnosticError> {
    if value.is_empty()
        || (!allow_leading_slash && (value.starts_with('/') || value.starts_with('\\')))
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_'))
        || value.split('/').any(|component| component == "..")
    {
        return Err(DiagnosticError::new(
            ErrorKind::InvalidRequest,
            format!("{label} is not safe for smbclient command mode"),
        ));
    }
    Ok(())
}

fn service_arg(server: &str, share: &str) -> OsString {
    OsString::from(format!("//{server}/{share}"))
}

mod bridge;

pub use bridge::SmbRemoteFileAdapter;
