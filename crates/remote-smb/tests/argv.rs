mod common;

use std::path::PathBuf;

use localdesk_remote_smb::{
    Authentication, Authority, CredentialRevision, DiagnosticOperation, DiagnosticRequest,
    OperationKind, Protection, ResumeTicket, Secret, build_plan,
};

fn args_as_strings(plan: &localdesk_remote_smb::DiagnosticPlan) -> Vec<String> {
    plan.args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn password_domain_browse_forces_smb2_and_smb3() {
    let plan = build_plan(
        "/usr/sbin/smbclient",
        common::password_request(DiagnosticOperation::BrowseShares {
            server: "files.example.test".to_owned(),
        }),
    )
    .expect("valid plan");
    let args = args_as_strings(&plan);

    assert!(args.contains(&"--option=client min protocol=SMB2".to_owned()));
    assert!(args.contains(&"--option=client max protocol=SMB3".to_owned()));
    assert!(args.contains(&"--option=client ipc min protocol=SMB2".to_owned()));
    assert!(args.contains(&"--option=client ipc max protocol=SMB3".to_owned()));
    assert!(args.contains(&"--workgroup=OPS".to_owned()));
    assert!(args.contains(&"--user=OPS\\operator".to_owned()));
    assert!(args.contains(&"--client-protection=sign".to_owned()));
    assert!(args.contains(&"--grepable".to_owned()));
    assert!(args.contains(&"--list=files.example.test".to_owned()));
    assert_eq!(plan.operation(), OperationKind::BrowseShares);
}

#[test]
fn kerberos_encryption_uses_required_ccache_authentication() {
    let request = DiagnosticRequest::new(
        Authentication::Kerberos {
            realm: Some("EXAMPLE.TEST".to_owned()),
            ccache: Some(PathBuf::from("/run/user/1000/krb5cc")),
        },
        Protection::Encryption,
        CredentialRevision {
            expected: 8,
            active: 8,
        },
        DiagnosticOperation::InspectShare {
            server: "files.example.test".to_owned(),
            share: "engineering".to_owned(),
        },
    );
    let plan = build_plan("smbclient", request).expect("valid plan");
    let args = args_as_strings(&plan);

    assert!(args.contains(&"--use-kerberos=required".to_owned()));
    assert!(args.contains(&"--realm=EXAMPLE.TEST".to_owned()));
    assert!(args.contains(&"--use-krb5-ccache=/run/user/1000/krb5cc".to_owned()));
    assert!(args.contains(&"--client-protection=encrypt".to_owned()));
    assert!(args.contains(&"//files.example.test/engineering".to_owned()));
    assert_eq!(plan.sensitive_environment_key(), None);
}

#[test]
fn controlled_resume_uses_reget_only_after_ticket_validation() {
    let request = common::password_request(DiagnosticOperation::ResumeDownload {
        server: "files.example.test".to_owned(),
        share: "engineering".to_owned(),
        ticket: ResumeTicket {
            remote_path: "releases/image.iso".to_owned(),
            local_partial_path: PathBuf::from("/var/tmp/image.iso.part"),
            verified_offset: 4096,
            observed_local_len: 4096,
            verified_remote_len: 8192,
            remote_identity: "size=8192;mtime=1720000000".to_owned(),
        },
    });
    let plan = build_plan("smbclient", request).expect("valid resume plan");
    let args = args_as_strings(&plan);

    assert!(
        args.contains(&"--command=reget releases/image.iso /var/tmp/image.iso.part".to_owned())
    );
    assert_eq!(plan.operation(), OperationKind::ResumeDownload);
}

#[test]
fn workgroup_authentication_and_fresh_process_reauth_avoid_logon_command() {
    let request = DiagnosticRequest::new(
        Authentication::Password {
            username: "operator".to_owned(),
            password: Secret::new("fresh-secret"),
            authority: Authority::Workgroup("LAB".to_owned()),
        },
        Protection::Signing,
        CredentialRevision {
            expected: 9,
            active: 9,
        },
        DiagnosticOperation::Reauthenticate {
            server: "nas.lab.test".to_owned(),
            share: "data".to_owned(),
        },
    );
    let plan = build_plan("smbclient", request).expect("valid reauth plan");
    let args = args_as_strings(&plan);

    assert!(args.contains(&"--workgroup=LAB".to_owned()));
    assert!(args.contains(&"--user=operator".to_owned()));
    assert!(args.contains(&"--command=quit".to_owned()));
    assert!(args.iter().all(|arg| !arg.contains("logon")));
    assert_eq!(plan.operation(), OperationKind::Reauthenticate);
}
