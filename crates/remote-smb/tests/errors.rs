mod common;

use std::io;
use std::path::PathBuf;

use localdesk_remote_smb::{
    CredentialRevision, DiagnosticOperation, DiagnosticOutcome, ErrorKind, ResumeTicket,
    build_plan, classify_io_error, classify_raw_exit,
};

#[test]
fn stale_credential_revision_is_a_conflict() {
    let mut request = common::password_request(DiagnosticOperation::Reauthenticate {
        server: "files.example.test".to_owned(),
        share: "engineering".to_owned(),
    });
    request.credential_revision = CredentialRevision {
        expected: 6,
        active: 7,
    };

    let error = build_plan("smbclient", request).expect_err("stale revision must fail");
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(error.reason.contains("credential revision conflict"));
}

#[test]
fn changed_partial_file_is_a_resume_conflict() {
    let request = common::password_request(DiagnosticOperation::ResumeDownload {
        server: "files.example.test".to_owned(),
        share: "engineering".to_owned(),
        ticket: ResumeTicket {
            remote_path: "releases/image.iso".to_owned(),
            local_partial_path: PathBuf::from("/var/tmp/image.iso.part"),
            verified_offset: 4096,
            observed_local_len: 4097,
            verified_remote_len: 8192,
            remote_identity: "size=8192;mtime=1720000000".to_owned(),
        },
    });

    let error = build_plan("smbclient", request).expect_err("changed partial must fail");
    assert_eq!(error.kind, ErrorKind::Conflict);
}

#[test]
fn unsafe_command_path_is_rejected_before_spawn() {
    let request = common::password_request(DiagnosticOperation::ResumeDownload {
        server: "files.example.test".to_owned(),
        share: "engineering".to_owned(),
        ticket: ResumeTicket {
            remote_path: "release;rm".to_owned(),
            local_partial_path: PathBuf::from("/var/tmp/image.iso.part"),
            verified_offset: 1,
            observed_local_len: 1,
            verified_remote_len: 2,
            remote_identity: "stable".to_owned(),
        },
    });

    let error = build_plan("smbclient", request).expect_err("command injection must fail");
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
}

#[test]
fn error_mapping_never_parses_human_output() {
    assert_eq!(
        classify_io_error(io::ErrorKind::NotFound),
        ErrorKind::Unsupported
    );
    assert_eq!(
        classify_io_error(io::ErrorKind::TimedOut),
        ErrorKind::TimedOut
    );
    assert_eq!(
        classify_raw_exit(false, Some(1)),
        DiagnosticOutcome::ClientRejected
    );
    assert_eq!(
        classify_raw_exit(true, Some(0)),
        DiagnosticOutcome::Succeeded
    );
}
