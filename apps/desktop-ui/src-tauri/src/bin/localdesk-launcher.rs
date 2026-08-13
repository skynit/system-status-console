use nix::unistd::Uid;
use std::{
    env, fs, io,
    os::unix::process::CommandExt,
    os::unix::{fs::MetadataExt, net::UnixStream, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};

const APPD: &str = "localdesk-appd";
const DESKTOP: &str = "localdesk-desktop";
const TELEMETRY_HELPER: &str = "localdesk-telemetry-helper";
const NETWORK_HELPER: &str = "localdesk-network-helper";
const SSH_ASKPASS: &str = "localdesk-ssh-askpass";
const SOCKET_RELATIVE_PATH: &str = "localdesk/appd.sock";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaunchMode {
    Desktop,
    DaemonOnly,
    Check,
}

#[derive(Debug)]
struct InstalledBinaries {
    appd: PathBuf,
    desktop: PathBuf,
}

fn main() -> ExitCode {
    match run(env::args_os().skip(1).map(PathBuf::from).collect()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("localdesk-launcher: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<PathBuf>) -> Result<u8, String> {
    let mode = parse_mode(&arguments)?;
    let binaries = installed_binaries().map_err(|error| error.to_string())?;
    if mode == LaunchMode::Check {
        println!("localdesk release layout is valid");
        return Ok(0);
    }

    let runtime_dir = runtime_directory().map_err(|error| error.to_string())?;
    let socket = runtime_dir.join(SOCKET_RELATIVE_PATH);
    if mode == LaunchMode::DaemonOnly {
        if UnixStream::connect(&socket).is_ok() {
            return Ok(0);
        }
        return Err(format!(
            "cannot exec {}: {}",
            binaries.appd.display(),
            Command::new(&binaries.appd).exec()
        ));
    }
    let mut spawned_appd = ensure_appd(&binaries.appd, &socket)?;

    let status = Command::new(&binaries.desktop)
        .status()
        .map_err(|error| format!("cannot start {}: {error}", binaries.desktop.display()))?;
    if let Some(child) = spawned_appd.as_mut() {
        let _ = child.try_wait();
    }
    Ok(exit_code(status.code(), status.signal()))
}

fn parse_mode(arguments: &[PathBuf]) -> Result<LaunchMode, String> {
    match arguments {
        [] => Ok(LaunchMode::Desktop),
        [value] if value.as_os_str() == "--daemon-only" => Ok(LaunchMode::DaemonOnly),
        [value] if value.as_os_str() == "--check" => Ok(LaunchMode::Check),
        _ => Err("expected no arguments, --daemon-only, or --check".to_owned()),
    }
}

fn installed_binaries() -> io::Result<InstalledBinaries> {
    let launcher = env::current_exe()?;
    let directory = launcher
        .parent()
        .ok_or_else(|| io::Error::other("launcher has no parent directory"))?;
    let launcher_metadata = fs::metadata(&launcher)?;
    let expected_owner = launcher_metadata.uid();
    let appd = validate_sibling(directory, APPD, expected_owner)?;
    let desktop = validate_sibling(directory, DESKTOP, expected_owner)?;
    for helper in [TELEMETRY_HELPER, NETWORK_HELPER, SSH_ASKPASS] {
        validate_sibling(directory, helper, expected_owner)?;
    }
    Ok(InstalledBinaries { appd, desktop })
}

fn validate_sibling(directory: &Path, name: &str, expected_owner: u32) -> io::Result<PathBuf> {
    let path = directory.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    let mode = metadata.mode();
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.uid() != expected_owner
        || mode & 0o111 == 0
        || mode & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe installed binary: {}", path.display()),
        ));
    }
    Ok(path)
}

fn runtime_directory() -> io::Result<PathBuf> {
    let path = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is not set"))?;
    if !path.is_absolute() {
        return Err(io::Error::other("XDG_RUNTIME_DIR is not absolute"));
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_dir()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "XDG_RUNTIME_DIR has unsafe ownership, type, or permissions",
        ));
    }
    Ok(path)
}

fn ensure_appd(program: &Path, socket: &Path) -> Result<Option<Child>, String> {
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let mut child = Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", program.display()))?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            return Ok(Some(child));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("cannot inspect appd startup: {error}"))?
        {
            if UnixStream::connect(socket).is_ok() {
                return Ok(None);
            }
            return Err(format!("appd exited before socket readiness: {status}"));
        }
        thread::sleep(POLL_INTERVAL);
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("appd did not become ready within 10 seconds".to_owned())
}

fn exit_code(code: Option<i32>, signal: Option<i32>) -> u8 {
    if let Some(code) = code {
        return u8::try_from(code.clamp(0, 255)).unwrap_or(1);
    }
    signal
        .and_then(|value| u8::try_from((128 + value).clamp(1, 255)).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn launch_mode_is_closed_to_unknown_arguments() {
        assert_eq!(parse_mode(&[]), Ok(LaunchMode::Desktop));
        assert_eq!(
            parse_mode(&[PathBuf::from("--daemon-only")]),
            Ok(LaunchMode::DaemonOnly)
        );
        assert_eq!(
            parse_mode(&[PathBuf::from("--check")]),
            Ok(LaunchMode::Check)
        );
        assert!(parse_mode(&[PathBuf::from("--route")]).is_err());
        assert!(parse_mode(&[PathBuf::from("--check"), PathBuf::from("extra")]).is_err());
    }

    #[test]
    fn sibling_validation_rejects_symlinks_and_writable_executables() {
        let directory = TempDir::new().unwrap();
        let owner = fs::metadata(directory.path()).unwrap().uid();
        let executable = directory.path().join("valid");
        fs::write(&executable, b"fixture").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            validate_sibling(directory.path(), "valid", owner).unwrap(),
            executable
        );

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(validate_sibling(directory.path(), "valid", owner).is_err());
        std::os::unix::fs::symlink(&executable, directory.path().join("linked")).unwrap();
        assert!(validate_sibling(directory.path(), "linked", owner).is_err());
    }

    #[test]
    fn child_status_mapping_is_bounded() {
        assert_eq!(exit_code(Some(0), None), 0);
        assert_eq!(exit_code(Some(300), None), 255);
        assert_eq!(exit_code(None, Some(15)), 143);
        assert_eq!(exit_code(None, None), 1);
    }
}
