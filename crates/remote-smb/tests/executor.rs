mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use localdesk_remote_smb::{
    DiagnosticOperation, DiagnosticOutcome, OperationKind, build_plan, execute,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "localdesk-remote-smb-{}-{sequence}.sh",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("create isolated fake client");
        file.write_all(b"#!/bin/sh\n").expect("write shebang");
        file.write_all(body.as_bytes()).expect("write fixture body");
        let mut permissions = file.metadata().expect("fixture metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("make fixture executable");
        Self { path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[test]
fn executor_bounds_opaque_output_without_parsing_it() {
    let fixture = Fixture::new(
        "i=0\nwhile [ \"$i\" -lt 2048 ]; do printf x; i=$((i + 1)); done\nprintf rejected >&2\nexit 7\n",
    );
    let mut request = common::password_request(DiagnosticOperation::BrowseShares {
        server: "files.example.test".to_owned(),
    });
    request.output_limit = 1024;
    let plan = build_plan(&fixture.path, request).expect("valid fake-client plan");

    let result = execute(plan).expect("fake client executes");

    assert_eq!(result.operation, OperationKind::BrowseShares);
    assert_eq!(result.outcome, DiagnosticOutcome::ClientRejected);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.stdout.bytes.len(), 1024);
    assert_eq!(result.stdout.total_bytes, 2048);
    assert!(result.stdout.truncated);
    assert_eq!(result.stderr.bytes, b"rejected");
}
