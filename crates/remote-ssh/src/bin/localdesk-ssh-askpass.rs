use std::{
    env, fs,
    io::{self, Read, Write},
    path::Path,
};

const SECRET_ENV: &str = "LOCALDESK_SSH_ASKPASS_SECRET";
const EXPECTED_TARGET: &str = "/memfd:localdesk-ssh-askpass-secret (deleted)";
const MAX_SECRET_BYTES: u64 = 8 * 1024;

fn main() {
    if run().is_err() {
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let path = env::var_os(SECRET_ENV)
        .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "missing secret path"))?;
    let path = Path::new(&path);
    validate_path(path)?;
    let target = fs::read_link(path)?;
    if target != Path::new(EXPECTED_TARGET) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid secret source",
        ));
    }
    let mut secret = Vec::new();
    fs::File::open(path)?
        .take(MAX_SECRET_BYTES + 1)
        .read_to_end(&mut secret)?;
    if secret.is_empty()
        || secret.len() > MAX_SECRET_BYTES as usize
        || secret.contains(&0)
        || secret.contains(&b'\n')
        || secret.contains(&b'\r')
    {
        secret.fill(0);
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid secret"));
    }
    let result = io::stdout()
        .write_all(&secret)
        .and_then(|_| io::stdout().write_all(b"\n"));
    secret.fill(0);
    result
}

fn validate_path(path: &Path) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid secret path"))?;
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() != 5
        || !parts[0].is_empty()
        || parts[1] != "proc"
        || parts[3] != "fd"
        || parts[2].parse::<u32>().is_err()
        || parts[4].parse::<u32>().is_err()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid secret path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_numeric_proc_fd_paths() {
        validate_path(Path::new("/proc/123/fd/7")).expect("valid proc fd path");

        for invalid in [
            "/proc/self/fd/7",
            "/proc/123/fd/secret",
            "/proc/123/task/7",
            "/tmp/123/fd/7",
            "proc/123/fd/7",
            "/proc/123/fd/7/extra",
        ] {
            let error = validate_path(Path::new(invalid)).expect_err("path must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{invalid}");
        }
    }
}
