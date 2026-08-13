use localdesk_remote_core::{
    AdapterAvailability, AdapterFuture, Authentication, CapabilityStatus, FILE_OPERATIONS,
    FileOperation, ProfileId, ProfileOptions, RemoteConnectionProfile, RemoteEndpoint,
    RemoteErrorKind, RemoteFileAdapter, RemoteOperation, RemoteProtocol, RetryDisposition,
    SafeReason, SecretRef, SecretStore, SecretStoreError, SecretValue, SmbDialect, TrustPolicy,
};
use localdesk_remote_smb::{
    CapabilityReport, CapabilityStatus as DiagnosticCapabilityStatus, OutputContract,
    ReauthenticationMode, SmbRemoteFileAdapter,
};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use uuid::Uuid;

const SECRET_TEXT: &str = "bridge-password-must-stay-redacted";

struct FakeSecretStore {
    resolves: AtomicUsize,
}

impl FakeSecretStore {
    fn new() -> Self {
        Self {
            resolves: AtomicUsize::new(0),
        }
    }
}

impl SecretStore for FakeSecretStore {
    fn resolve<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
        self.resolves.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(SecretValue::new(SECRET_TEXT.as_bytes().to_vec())) })
    }

    fn delete<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct LockedSecretStore;

impl SecretStore for LockedSecretStore {
    fn resolve<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>> {
        Box::pin(async { Err(SecretStoreError::Locked(reason("secret_service_locked"))) })
    }

    fn delete<'a>(
        &'a self,
        _reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

fn reason(value: &str) -> SafeReason {
    SafeReason::new(value).expect("test reason")
}

fn healthy_report() -> CapabilityReport {
    CapabilityReport {
        status: DiagnosticCapabilityStatus::Healthy,
        reason: "local diagnostic available".to_owned(),
        client_version: Some("Version 4.24.5".to_owned()),
        dialects: ["SMB2", "SMB3"],
        smb1_enabled: false,
        supports_workgroup_domain: true,
        supports_kerberos: true,
        supports_signing: true,
        supports_encryption: true,
        supports_share_browse_diagnostic: true,
        reauthentication: ReauthenticationMode::FreshProcess,
        output_contract: OutputContract::OpaqueHumanOutput,
    }
}

fn password_profile() -> RemoteConnectionProfile {
    RemoteConnectionProfile::new(
        ProfileId::new(),
        "SMB diagnostic profile",
        RemoteProtocol::Smb,
        RemoteEndpoint::new("nas.example.test", 1445).expect("endpoint"),
        Some("operator".to_owned()),
        Some("OPS".to_owned()),
        Authentication::Password {
            secret: SecretRef::secret_service(
                Uuid::parse_str("12345678-1234-5678-1234-567812345678").expect("stable UUID"),
            ),
        },
        TrustPolicy::SmbNegotiated,
        ProfileOptions::Smb {
            share: Some("engineering".to_owned()),
            minimum_dialect: SmbDialect::Smb3,
            require_signing: true,
            require_encryption: true,
        },
    )
    .expect("valid SMB profile")
}

fn kerberos_profile() -> RemoteConnectionProfile {
    RemoteConnectionProfile::new(
        ProfileId::new(),
        "SMB Kerberos diagnostic profile",
        RemoteProtocol::Smb,
        RemoteEndpoint::new("nas.example.test", 445).expect("endpoint"),
        None,
        Some("EXAMPLE.TEST".to_owned()),
        Authentication::Kerberos,
        TrustPolicy::SmbNegotiated,
        ProfileOptions::Smb {
            share: None,
            minimum_dialect: SmbDialect::Smb2,
            require_signing: true,
            require_encryption: false,
        },
    )
    .expect("valid Kerberos SMB profile")
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[test]
fn bridge_maps_typed_profile_and_secret_without_exposing_secret() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());
    let store = FakeSecretStore::new();
    let profile = password_profile();

    let plan = block_on(adapter.prepare_diagnostic(&profile, &store)).expect("diagnostic plan");
    let args: Vec<_> = plan
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert!(args.contains(&"--port=1445".to_owned()));
    assert!(args.contains(&"--option=client min protocol=SMB3".to_owned()));
    assert!(args.contains(&"--option=client ipc min protocol=SMB3".to_owned()));
    assert!(args.contains(&"--client-protection=encrypt".to_owned()));
    assert!(args.contains(&"--workgroup=OPS".to_owned()));
    assert!(args.contains(&"--user=OPS\\operator".to_owned()));
    assert!(args.contains(&"//nas.example.test/engineering".to_owned()));
    assert!(args.iter().all(|arg| !arg.contains(SECRET_TEXT)));
    assert!(!format!("{plan:?}").contains(SECRET_TEXT));
    assert_eq!(store.resolves.load(Ordering::Relaxed), 1);
}

#[test]
fn kerberos_profile_maps_realm_and_browse_without_secret_resolution() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());
    let store = FakeSecretStore::new();
    let profile = kerberos_profile();

    let plan = block_on(adapter.prepare_diagnostic(&profile, &store)).expect("Kerberos plan");
    let args: Vec<_> = plan
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert!(args.contains(&"--use-kerberos=required".to_owned()));
    assert!(args.contains(&"--realm=EXAMPLE.TEST".to_owned()));
    assert!(args.contains(&"--client-protection=sign".to_owned()));
    assert!(args.contains(&"--list=nas.example.test".to_owned()));
    assert_eq!(store.resolves.load(Ordering::Relaxed), 0);
}

#[test]
fn secret_store_failure_maps_to_typed_remote_error() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());
    let profile = password_profile();

    let error = block_on(adapter.prepare_diagnostic(&profile, &LockedSecretStore))
        .expect_err("locked store must fail");

    assert_eq!(error.kind, RemoteErrorKind::SecretStore);
    assert_eq!(error.operation, RemoteOperation::ResolveSecret);
    assert_eq!(error.reason, reason("secret_service_locked"));
    assert_eq!(error.retry, RetryDisposition::UserAction);
}

#[test]
fn diagnostic_availability_never_claims_production_file_capabilities() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());

    assert_eq!(adapter.protocol(), RemoteProtocol::Smb);
    assert_eq!(
        adapter.availability(),
        AdapterAvailability::Degraded(reason("smb_file_adapter_diagnostic_only"))
    );
    for operation in FILE_OPERATIONS {
        assert!(matches!(
            adapter.capabilities().status(*operation),
            CapabilityStatus::Unsupported(_)
        ));
    }
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeRead),
        &CapabilityStatus::Unsupported(reason("smb_resume_read_not_identity_safe"))
    );
}

#[test]
fn diagnostic_only_adapter_rejects_file_sessions_before_secret_resolution() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());
    let store = FakeSecretStore::new();
    let profile = password_profile();
    let error = match block_on(adapter.connect(&profile, &store)) {
        Ok(_) => panic!("diagnostic-only adapter must reject file sessions"),
        Err(error) => error,
    };
    assert_eq!(store.resolves.load(Ordering::Relaxed), 0);
    assert_eq!(error.kind, RemoteErrorKind::Unsupported);
    assert_eq!(error.operation, RemoteOperation::Connect);
    assert_eq!(error.reason, reason("smb_file_adapter_diagnostic_only"));
    assert_eq!(error.retry, RetryDisposition::Never);
}

#[test]
fn diagnostic_only_adapter_does_not_expose_resume_operations() {
    let adapter = SmbRemoteFileAdapter::from_report("smbclient", healthy_report());
    let store = FakeSecretStore::new();
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeRead),
        &CapabilityStatus::Unsupported(reason("smb_resume_read_not_identity_safe"))
    );
    assert_eq!(
        adapter.capabilities().status(FileOperation::ResumeWrite),
        &CapabilityStatus::Unsupported(reason("smb_resume_write_not_implemented"))
    );
    assert_eq!(store.resolves.load(Ordering::Relaxed), 0);
}

#[test]
fn missing_smbclient_is_unsupported_before_secret_resolution() {
    let mut report = healthy_report();
    report.status = DiagnosticCapabilityStatus::Unsupported;
    report.reason = "missing".to_owned();
    let adapter = SmbRemoteFileAdapter::from_report("missing-smbclient", report);
    let store = FakeSecretStore::new();
    let profile = password_profile();

    assert_eq!(
        adapter.availability(),
        AdapterAvailability::Unsupported(reason("smbclient_not_installed"))
    );
    let error = match block_on(adapter.connect(&profile, &store)) {
        Ok(_) => panic!("missing client must not create a session"),
        Err(error) => error,
    };
    assert_eq!(error.kind, RemoteErrorKind::Unsupported);
    assert_eq!(error.operation, RemoteOperation::Connect);
    assert_eq!(error.reason, reason("smbclient_not_installed"));
    assert_eq!(store.resolves.load(Ordering::Relaxed), 0);
}
