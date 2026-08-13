use std::path::{Path, PathBuf};
use thiserror::Error;

pub const MAX_JUMP_HOSTS: usize = 8;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HostKeyPolicy {
    Strict,
    AcceptNew,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HostTrust {
    pub known_hosts_file: PathBuf,
    pub revoked_host_keys_file: Option<PathBuf>,
    pub policy: HostKeyPolicy,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Authentication {
    Agent,
    IdentityFile(PathBuf),
    IdentityFileWithPassphrase(PathBuf),
    Password,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub trust: HostTrust,
    pub authentication: Authentication,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SshProfile {
    pub target: Endpoint,
    pub jump_hosts: Vec<Endpoint>,
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum ProfileError {
    #[error("{field} must not be empty")]
    Empty { field: String },
    #[error("{field} contains characters OpenSSH config does not accept")]
    InvalidToken { field: String },
    #[error("{field} must be an absolute UTF-8 path without config expansion tokens")]
    InvalidPath { field: String },
    #[error("{field} port must not be zero")]
    InvalidPort { field: String },
    #[error("jump host count exceeds {MAX_JUMP_HOSTS}")]
    TooManyJumpHosts,
}

impl SshProfile {
    pub(crate) fn validate(&self) -> Result<(), ProfileError> {
        if self.jump_hosts.len() > MAX_JUMP_HOSTS {
            return Err(ProfileError::TooManyJumpHosts);
        }
        validate_endpoint(&self.target, "target")?;
        for (index, endpoint) in self.jump_hosts.iter().enumerate() {
            validate_endpoint(endpoint, &format!("jump_hosts[{index}]"))?;
        }
        Ok(())
    }
}

fn validate_endpoint(endpoint: &Endpoint, field: &str) -> Result<(), ProfileError> {
    validate_token(&endpoint.host, &format!("{field}.host"), is_host_char)?;
    if let Some(user) = &endpoint.user {
        validate_token(user, &format!("{field}.user"), is_user_char)?;
    }
    if endpoint.port == 0 {
        return Err(ProfileError::InvalidPort {
            field: field.to_owned(),
        });
    }
    validate_path(
        &endpoint.trust.known_hosts_file,
        &format!("{field}.known_hosts_file"),
    )?;
    if let Some(path) = &endpoint.trust.revoked_host_keys_file {
        validate_path(path, &format!("{field}.revoked_host_keys_file"))?;
    }
    if let Authentication::IdentityFile(path) | Authentication::IdentityFileWithPassphrase(path) =
        &endpoint.authentication
    {
        validate_path(path, &format!("{field}.identity_file"))?;
    }
    Ok(())
}

fn validate_token(
    value: &str,
    field: &str,
    allowed: impl Fn(char) -> bool,
) -> Result<(), ProfileError> {
    if value.is_empty() {
        return Err(ProfileError::Empty {
            field: field.to_owned(),
        });
    }
    if value.len() > 255 || !value.chars().all(allowed) {
        return Err(ProfileError::InvalidToken {
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn is_host_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':')
}

fn is_user_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
}

pub(crate) fn validate_path(path: &Path, field: &str) -> Result<(), ProfileError> {
    let Some(value) = path.to_str() else {
        return Err(ProfileError::InvalidPath {
            field: field.to_owned(),
        });
    };
    if !path.is_absolute() || value.is_empty() || value.contains(['\0', '\n', '\r', '%', '$']) {
        return Err(ProfileError::InvalidPath {
            field: field.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint {
            host: "host.example".to_owned(),
            port: 22,
            user: Some("operator".to_owned()),
            trust: HostTrust {
                known_hosts_file: PathBuf::from("/tmp/localdesk known_hosts"),
                revoked_host_keys_file: Some(PathBuf::from("/tmp/revoked_keys")),
                policy: HostKeyPolicy::Strict,
            },
            authentication: Authentication::Agent,
        }
    }

    #[test]
    fn rejects_config_injection_and_relative_trust_paths() {
        let mut profile = SshProfile {
            target: endpoint(),
            jump_hosts: Vec::new(),
        };
        profile.target.host = "host\nProxyCommand evil".to_owned();
        assert!(matches!(
            profile.validate(),
            Err(ProfileError::InvalidToken { field }) if field == "target.host"
        ));

        profile.target = endpoint();
        profile.target.trust.known_hosts_file = PathBuf::from("relative/known_hosts");
        assert!(matches!(
            profile.validate(),
            Err(ProfileError::InvalidPath { field }) if field == "target.known_hosts_file"
        ));
    }

    #[test]
    fn bounds_jump_chain_and_rejects_expansion_tokens_in_paths() {
        let mut profile = SshProfile {
            target: endpoint(),
            jump_hosts: vec![endpoint(); MAX_JUMP_HOSTS + 1],
        };
        assert_eq!(profile.validate(), Err(ProfileError::TooManyJumpHosts));

        profile.jump_hosts.clear();
        profile.target.trust.known_hosts_file = PathBuf::from("/tmp/%h-known_hosts");
        assert!(matches!(
            profile.validate(),
            Err(ProfileError::InvalidPath { .. })
        ));
    }
}
