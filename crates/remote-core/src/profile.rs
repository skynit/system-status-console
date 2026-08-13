use crate::{AdapterFuture, SafeReason};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProfileId(Uuid);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProtocol {
    Ssh,
    Sftp,
    Ftp,
    FtpsExplicit,
    Smb,
}

impl RemoteProtocol {
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ssh | Self::Sftp => 22,
            Self::Ftp | Self::FtpsExplicit => 21,
            Self::Smb => 445,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteEndpoint {
    host: String,
    pub port: u16,
}

impl RemoteEndpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, ProfileValidationError> {
        let host = host.into();
        if host.is_empty() || host.len() > 255 {
            return Err(ProfileValidationError::InvalidHost);
        }
        if host.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '/' | '\\' | '@')
        }) || host.contains("://")
        {
            return Err(ProfileValidationError::InvalidHost);
        }
        if port == 0 {
            return Err(ProfileValidationError::InvalidPort);
        }
        Ok(Self { host, port })
    }

    pub fn host(&self) -> &str {
        &self.host
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    SecretService,
}

#[derive(Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SecretRef {
    backend: SecretBackend,
    item_id: Uuid,
}

pub const MAX_SECRET_INPUT_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Password,
    PrivateKey,
    KeyPassphrase,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SecretInput(Vec<u8>);

impl SecretInput {
    pub fn new(value: Vec<u8>) -> Result<Self, ProfileValidationError> {
        if value.is_empty() || value.len() > MAX_SECRET_INPUT_BYTES {
            return Err(ProfileValidationError::InvalidSecretInput);
        }
        Ok(Self(value))
    }

    pub fn expose_secret(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretInput(<redacted>)")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SecretCommand {
    Store {
        kind: SecretKind,
        value: SecretInput,
    },
    Delete {
        reference: SecretRef,
    },
}

impl fmt::Debug for SecretCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store { kind, .. } => formatter
                .debug_struct("Store")
                .field("kind", kind)
                .field("value", &"<redacted>")
                .finish(),
            Self::Delete { reference } => formatter
                .debug_struct("Delete")
                .field("reference", reference)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum SecretCommandResult {
    Stored { reference: SecretRef },
    Deleted,
}

impl SecretCommand {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        match self {
            Self::Store { value, .. } => {
                if value.0.is_empty() || value.0.len() > MAX_SECRET_INPUT_BYTES {
                    return Err(ProfileValidationError::InvalidSecretInput);
                }
                Ok(())
            }
            Self::Delete { .. } => Ok(()),
        }
    }
}

impl SecretCommandResult {
    pub fn validate_for(&self, command: &SecretCommand) -> Result<(), ProfileValidationError> {
        match (self, command) {
            (Self::Stored { reference }, SecretCommand::Store { .. })
                if reference.backend() == SecretBackend::SecretService =>
            {
                Ok(())
            }
            (Self::Deleted, SecretCommand::Delete { .. }) => Ok(()),
            _ => Err(ProfileValidationError::InvalidResult),
        }
    }
}

impl SecretRef {
    pub const fn secret_service(item_id: Uuid) -> Self {
        Self {
            backend: SecretBackend::SecretService,
            item_id,
        }
    }

    pub const fn backend(&self) -> SecretBackend {
        self.backend
    }

    pub const fn item_id(&self) -> Uuid {
        self.item_id
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretRef")
            .field("backend", &self.backend)
            .field("item_id", &"<opaque>")
            .finish()
    }
}

pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    pub fn expose_secret(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue(<redacted>)")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Debug, Clone, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum SecretStoreError {
    #[error("secret store is locked: {0}")]
    Locked(SafeReason),
    #[error("secret store permission denied: {0}")]
    PermissionDenied(SafeReason),
    #[error("secret store is unavailable: {0}")]
    Unavailable(SafeReason),
    #[error("secret reference was not found: {0}")]
    NotFound(SafeReason),
    #[error("secret store operation failed: {0}")]
    Backend(SafeReason),
}

pub trait SecretStore: Send + Sync {
    fn resolve<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<SecretValue, SecretStoreError>>;

    fn delete<'a>(
        &'a self,
        reference: &'a SecretRef,
    ) -> AdapterFuture<'a, Result<(), SecretStoreError>>;
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Authentication {
    Anonymous,
    Password {
        secret: SecretRef,
    },
    SshAgent,
    SshKey {
        private_key: SecretRef,
        passphrase: Option<SecretRef>,
    },
    Kerberos,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstUsePolicy {
    Reject,
    AskUser,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustPolicy {
    SshKnownHosts { first_use: FirstUsePolicy },
    SystemTls,
    PinnedTlsCertificate { certificate_pem: String },
    PlaintextAcknowledged,
    SmbNegotiated,
}

pub const MAX_PINNED_TLS_CERTIFICATE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataConnectionMode {
    Passive,
    ActiveRestricted,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbDialect {
    Smb2,
    Smb3,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ProfileOptions {
    Ssh {
        jump_profiles: Vec<ProfileId>,
        agent_forwarding: bool,
    },
    Sftp {
        jump_profiles: Vec<ProfileId>,
    },
    Ftp {
        data_connection: DataConnectionMode,
    },
    FtpsExplicit {
        data_connection: DataConnectionMode,
        require_protected_data_channel: bool,
    },
    Smb {
        share: Option<String>,
        minimum_dialect: SmbDialect,
        require_signing: bool,
        require_encryption: bool,
    },
}

impl ProfileOptions {
    pub const fn protocol(&self) -> RemoteProtocol {
        match self {
            Self::Ssh { .. } => RemoteProtocol::Ssh,
            Self::Sftp { .. } => RemoteProtocol::Sftp,
            Self::Ftp { .. } => RemoteProtocol::Ftp,
            Self::FtpsExplicit { .. } => RemoteProtocol::FtpsExplicit,
            Self::Smb { .. } => RemoteProtocol::Smb,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteConnectionProfile {
    pub id: ProfileId,
    pub label: String,
    pub protocol: RemoteProtocol,
    pub endpoint: RemoteEndpoint,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub authentication: Authentication,
    pub trust: TrustPolicy,
    pub options: ProfileOptions,
}

pub const MAX_REMOTE_PROFILE_PAGE_SIZE: u16 = 16;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredRemoteProfile {
    pub profile: RemoteConnectionProfile,
    pub revision: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl StoredRemoteProfile {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        self.profile.validate()?;
        if self.created_at_unix_ms < 0 || self.updated_at_unix_ms < self.created_at_unix_ms {
            return Err(ProfileValidationError::InvalidTimestamp);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProfilePageQuery {
    pub after: Option<ProfileId>,
    pub limit: u16,
}

impl RemoteProfilePageQuery {
    pub fn validate(self) -> Result<(), ProfileValidationError> {
        if self.limit == 0 || self.limit > MAX_REMOTE_PROFILE_PAGE_SIZE {
            return Err(ProfileValidationError::InvalidPageLimit);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProfilePage {
    pub profiles: Vec<StoredRemoteProfile>,
    pub next_after: Option<ProfileId>,
}

impl RemoteProfilePage {
    pub fn validate(&self, query: RemoteProfilePageQuery) -> Result<(), ProfileValidationError> {
        query.validate()?;
        if self.profiles.len() > usize::from(query.limit)
            || self
                .profiles
                .windows(2)
                .any(|pair| pair[0].profile.id >= pair[1].profile.id)
            || self
                .profiles
                .iter()
                .any(|stored| stored.validate().is_err())
            || query.after.is_some_and(|after| {
                self.profiles
                    .first()
                    .is_some_and(|stored| stored.profile.id <= after)
            })
            || self.next_after.is_some_and(|next| {
                self.profiles
                    .last()
                    .is_none_or(|stored| stored.profile.id != next)
            })
        {
            return Err(ProfileValidationError::InvalidPage);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum RemoteProfileCommand {
    List {
        query: RemoteProfilePageQuery,
    },
    Upsert {
        profile: RemoteConnectionProfile,
        expected_revision: Option<u64>,
    },
    Delete {
        profile_id: ProfileId,
        expected_revision: u64,
    },
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum RemoteProfileResult {
    Page(RemoteProfilePage),
    Stored(StoredRemoteProfile),
    Deleted { profile_id: ProfileId },
}

impl RemoteProfileCommand {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        match self {
            Self::List { query } => query.validate(),
            Self::Upsert {
                profile,
                expected_revision,
            } => {
                profile.validate()?;
                if expected_revision == &Some(u64::MAX) {
                    return Err(ProfileValidationError::InvalidRevision);
                }
                Ok(())
            }
            Self::Delete { .. } => Ok(()),
        }
    }
}

impl RemoteProfileResult {
    pub fn validate_for(
        &self,
        command: &RemoteProfileCommand,
    ) -> Result<(), ProfileValidationError> {
        match (self, command) {
            (Self::Page(page), RemoteProfileCommand::List { query }) => page.validate(*query),
            (
                Self::Stored(stored),
                RemoteProfileCommand::Upsert {
                    profile,
                    expected_revision,
                },
            ) => {
                stored.validate()?;
                let expected_next = expected_revision.map_or(0, |revision| revision + 1);
                if stored.profile.id != profile.id || stored.revision != expected_next {
                    return Err(ProfileValidationError::InvalidResult);
                }
                Ok(())
            }
            (
                Self::Deleted { profile_id },
                RemoteProfileCommand::Delete {
                    profile_id: requested,
                    ..
                },
            ) if profile_id == requested => Ok(()),
            _ => Err(ProfileValidationError::InvalidResult),
        }
    }
}

impl RemoteConnectionProfile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ProfileId,
        label: impl Into<String>,
        protocol: RemoteProtocol,
        endpoint: RemoteEndpoint,
        username: Option<String>,
        domain: Option<String>,
        authentication: Authentication,
        trust: TrustPolicy,
        options: ProfileOptions,
    ) -> Result<Self, ProfileValidationError> {
        let profile = Self {
            id,
            label: label.into(),
            protocol,
            endpoint,
            username,
            domain,
            authentication,
            trust,
            options,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        validate_display_field(&self.label, 128, ProfileValidationError::InvalidLabel)?;
        if let Some(username) = &self.username {
            validate_display_field(username, 256, ProfileValidationError::InvalidUsername)?;
        }
        if let Some(domain) = &self.domain {
            validate_display_field(domain, 255, ProfileValidationError::InvalidDomain)?;
        }
        if self.protocol != self.options.protocol() {
            return Err(ProfileValidationError::ProtocolOptionsMismatch);
        }

        match (&self.protocol, &self.trust) {
            (RemoteProtocol::Ssh | RemoteProtocol::Sftp, TrustPolicy::SshKnownHosts { .. })
            | (RemoteProtocol::Ftp, TrustPolicy::PlaintextAcknowledged)
            | (
                RemoteProtocol::FtpsExplicit,
                TrustPolicy::SystemTls | TrustPolicy::PinnedTlsCertificate { .. },
            )
            | (RemoteProtocol::Smb, TrustPolicy::SmbNegotiated) => {}
            _ => return Err(ProfileValidationError::TrustPolicyMismatch),
        }
        if let TrustPolicy::PinnedTlsCertificate { certificate_pem } = &self.trust {
            validate_pinned_tls_certificate(certificate_pem)?;
        }

        match (&self.protocol, &self.authentication) {
            (RemoteProtocol::Ssh | RemoteProtocol::Sftp, Authentication::SshAgent)
            | (RemoteProtocol::Ssh | RemoteProtocol::Sftp, Authentication::SshKey { .. })
            | (RemoteProtocol::Ssh | RemoteProtocol::Sftp, Authentication::Password { .. })
            | (RemoteProtocol::Ftp | RemoteProtocol::FtpsExplicit, Authentication::Anonymous)
            | (
                RemoteProtocol::Ftp | RemoteProtocol::FtpsExplicit,
                Authentication::Password { .. },
            )
            | (RemoteProtocol::Smb, Authentication::Password { .. })
            | (RemoteProtocol::Smb, Authentication::Kerberos) => Ok(()),
            _ => Err(ProfileValidationError::AuthenticationMismatch),
        }
    }
}

fn validate_pinned_tls_certificate(value: &str) -> Result<(), ProfileValidationError> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    if value.len() > MAX_PINNED_TLS_CERTIFICATE_BYTES
        || !value.starts_with(BEGIN)
        || !value.trim_end().ends_with(END)
        || value.matches(BEGIN).count() != 1
        || value.matches(END).count() != 1
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric()
                || matches!(character, '+' | '/' | '=' | '-' | ' ' | '\r' | '\n'))
        })
    {
        return Err(ProfileValidationError::InvalidPinnedTlsCertificate);
    }
    Ok(())
}

fn validate_display_field(
    value: &str,
    max_bytes: usize,
    error: ProfileValidationError,
) -> Result<(), ProfileValidationError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum ProfileValidationError {
    #[error("profile label is invalid")]
    InvalidLabel,
    #[error("endpoint host is invalid")]
    InvalidHost,
    #[error("endpoint port must be non-zero")]
    InvalidPort,
    #[error("username is invalid")]
    InvalidUsername,
    #[error("domain is invalid")]
    InvalidDomain,
    #[error("profile options do not match the selected protocol")]
    ProtocolOptionsMismatch,
    #[error("trust policy does not match the selected protocol")]
    TrustPolicyMismatch,
    #[error("pinned TLS certificate is not one bounded PEM certificate")]
    InvalidPinnedTlsCertificate,
    #[error("profile timestamps are invalid")]
    InvalidTimestamp,
    #[error("profile page limit is outside the hard bound")]
    InvalidPageLimit,
    #[error("profile page is inconsistent with the query")]
    InvalidPage,
    #[error("profile revision is invalid")]
    InvalidRevision,
    #[error("profile operation result does not match its request")]
    InvalidResult,
    #[error("secret input is empty or exceeds the hard byte limit")]
    InvalidSecretInput,
    #[error("authentication method does not match the selected protocol")]
    AuthenticationMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftps_profile(trust: TrustPolicy) -> RemoteConnectionProfile {
        RemoteConnectionProfile {
            id: ProfileId::new(),
            label: "FTPS".into(),
            protocol: RemoteProtocol::FtpsExplicit,
            endpoint: RemoteEndpoint::new("files.example", 21).unwrap(),
            username: None,
            domain: None,
            authentication: Authentication::Anonymous,
            trust,
            options: ProfileOptions::FtpsExplicit {
                data_connection: DataConnectionMode::Passive,
                require_protected_data_channel: true,
            },
        }
    }

    #[test]
    fn ftps_accepts_one_bounded_pinned_pem_certificate() {
        let profile = ftps_profile(TrustPolicy::PinnedTlsCertificate {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n"
                .into(),
        });
        assert_eq!(profile.validate(), Ok(()));
    }

    #[test]
    fn ftps_rejects_invalid_or_multiple_pinned_certificates() {
        for certificate_pem in [
            "not a certificate",
            "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nZGVm\n-----END CERTIFICATE-----\n",
        ] {
            assert_eq!(
                ftps_profile(TrustPolicy::PinnedTlsCertificate {
                    certificate_pem: certificate_pem.into(),
                })
                .validate(),
                Err(ProfileValidationError::InvalidPinnedTlsCertificate)
            );
        }
    }
}
