use localdesk_remote_smb::{
    Authentication, Authority, CredentialRevision, DiagnosticOperation, DiagnosticRequest,
    Protection, Secret,
};

pub fn password_request(operation: DiagnosticOperation) -> DiagnosticRequest {
    DiagnosticRequest::new(
        Authentication::Password {
            username: "operator".to_owned(),
            password: Secret::new("correct-horse-battery-staple"),
            authority: Authority::Domain("OPS".to_owned()),
        },
        Protection::Signing,
        CredentialRevision {
            expected: 7,
            active: 7,
        },
        operation,
    )
}
