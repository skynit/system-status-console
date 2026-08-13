use localdesk_remote_smb::{
    CapabilityStatus, OutputContract, ReauthenticationMode, capability_from_version_output,
    probe_smbclient,
};

#[test]
fn successful_local_version_probe_reports_poc_capabilities() {
    let report = capability_from_version_output(true, b"Version 4.24.5\n", b"");

    assert_eq!(report.status, CapabilityStatus::Healthy);
    assert_eq!(report.client_version.as_deref(), Some("Version 4.24.5"));
    assert_eq!(report.dialects, ["SMB2", "SMB3"]);
    assert!(!report.smb1_enabled);
    assert!(report.supports_workgroup_domain);
    assert!(report.supports_kerberos);
    assert!(report.supports_signing);
    assert!(report.supports_encryption);
    assert!(report.supports_share_browse_diagnostic);
    assert_eq!(report.reauthentication, ReauthenticationMode::FreshProcess);
    assert_eq!(report.output_contract, OutputContract::OpaqueHumanOutput);
}

#[test]
fn failed_or_missing_client_does_not_report_healthy() {
    let failed = capability_from_version_output(false, b"", b"failed");
    assert_eq!(failed.status, CapabilityStatus::Degraded);

    let missing = probe_smbclient("/definitely/missing/localdesk-smbclient");
    assert_eq!(missing.status, CapabilityStatus::Unsupported);
    assert!(missing.reason.contains("probe failed"));
    assert!(!missing.supports_share_browse_diagnostic);
    assert!(!missing.supports_kerberos);
}
