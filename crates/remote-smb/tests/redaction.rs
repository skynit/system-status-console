mod common;

use localdesk_remote_smb::{DiagnosticOperation, Secret, build_plan};

const PASSWORD: &str = "correct-horse-battery-staple";

#[test]
fn secret_and_plan_debug_output_are_redacted() {
    let secret = Secret::new(PASSWORD);
    assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");

    let plan = build_plan(
        "smbclient",
        common::password_request(DiagnosticOperation::BrowseShares {
            server: "files.example.test".to_owned(),
        }),
    )
    .expect("valid plan");
    let debug = format!("{plan:?}");

    assert!(!debug.contains(PASSWORD));
    assert!(debug.contains("[REDACTED]"));
    assert_eq!(plan.sensitive_environment_key(), Some("PASSWD"));
}

#[test]
fn password_never_appears_in_argv() {
    let plan = build_plan(
        "smbclient",
        common::password_request(DiagnosticOperation::BrowseShares {
            server: "files.example.test".to_owned(),
        }),
    )
    .expect("valid plan");

    assert!(
        plan.args()
            .iter()
            .all(|arg| !arg.to_string_lossy().contains(PASSWORD))
    );
}
