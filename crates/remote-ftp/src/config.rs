use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::num::NonZeroU16;
use std::time::Duration;

use crate::FtpError;

pub const PLAIN_FTP_ACKNOWLEDGEMENT: &str =
    "I understand that plain FTP exposes credentials and file contents";

#[derive(Clone)]
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    /// Creates credentials kept outside URLs and redacted from `Debug` output.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty username or control characters.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Result<Self, FtpError> {
        let username = username.into();
        let password = password.into();
        reject_control("username", &username)?;
        reject_control("password", &password)?;
        if username.is_empty() {
            return Err(FtpError::Configuration("username must not be empty".into()));
        }
        Ok(Self { username, password })
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &"[redacted]")
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainFtpConfirmation(());

impl PlainFtpConfirmation {
    /// Confirms the exact plain FTP risk statement.
    ///
    /// # Errors
    ///
    /// Returns [`FtpError::Policy`] unless `statement` exactly matches
    /// [`PLAIN_FTP_ACKNOWLEDGEMENT`].
    pub fn acknowledge(statement: &str) -> Result<Self, FtpError> {
        if statement == PLAIN_FTP_ACKNOWLEDGEMENT {
            Ok(Self(()))
        } else {
            Err(FtpError::Policy(
                "plain FTP requires the exact risk acknowledgement".into(),
            ))
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SecurityMode {
    #[default]
    ExplicitFtps,
    PlainFtp(PlainFtpConfirmation),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DataMode {
    #[default]
    Passive,
    Active {
        bind_address: IpAddr,
        listen_port: NonZeroU16,
    },
}

impl DataMode {
    pub(crate) fn active_binding(&self) -> Result<Option<String>, FtpError> {
        let Self::Active {
            bind_address,
            listen_port,
        } = self
        else {
            return Ok(None);
        };

        if bind_address.is_unspecified() || bind_address.is_multicast() {
            return Err(FtpError::Policy(
                "active mode requires one exact non-unspecified, non-multicast IP".into(),
            ));
        }
        if matches!(bind_address, IpAddr::V6(address) if *address == Ipv6Addr::LOCALHOST) {
            return Ok(Some(format!("[{bind_address}]:{listen_port}")));
        }
        let binding = match bind_address {
            IpAddr::V4(address) => format!("{address}:{listen_port}"),
            IpAddr::V6(address) => format!("[{address}]:{listen_port}"),
        };
        Ok(Some(binding))
    }
}

#[derive(Clone, Debug)]
pub struct FtpConfig {
    pub host: String,
    pub port: NonZeroU16,
    pub credentials: Credentials,
    pub security: SecurityMode,
    pub data_mode: DataMode,
    pub ca_certificate_pem: Option<Vec<u8>>,
    pub connect_timeout: Duration,
    pub operation_timeout: Duration,
}

impl FtpConfig {
    /// Creates a passive explicit FTPS configuration with system CA verification.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint or default policy is invalid.
    pub fn explicit_ftps(
        host: impl Into<String>,
        port: NonZeroU16,
        credentials: Credentials,
    ) -> Result<Self, FtpError> {
        let config = Self {
            host: host.into(),
            port,
            credentials,
            security: SecurityMode::ExplicitFtps,
            data_mode: DataMode::Passive,
            ca_certificate_pem: None,
            connect_timeout: Duration::from_secs(10),
            operation_timeout: Duration::from_mins(5),
        };
        config.validate()?;
        Ok(config)
    }

    /// Validates endpoint, timeout, TLS, and data-mode policy.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe or internally inconsistent configuration.
    pub fn validate(&self) -> Result<(), FtpError> {
        validate_host(&self.host)?;
        if self.connect_timeout.is_zero() || self.operation_timeout.is_zero() {
            return Err(FtpError::Configuration(
                "timeouts must be greater than zero".into(),
            ));
        }
        if self.connect_timeout > self.operation_timeout {
            return Err(FtpError::Configuration(
                "connect timeout must not exceed operation timeout".into(),
            ));
        }
        self.data_mode.active_binding()?;
        if matches!(self.security, SecurityMode::PlainFtp(_)) && self.ca_certificate_pem.is_some() {
            return Err(FtpError::Configuration(
                "a CA certificate is only meaningful for explicit FTPS".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn authority(&self) -> String {
        let host = if self.host.parse::<Ipv6Addr>().is_ok() {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{host}:{}", self.port)
    }
}

fn validate_host(host: &str) -> Result<(), FtpError> {
    if host.is_empty() {
        return Err(FtpError::Configuration("host must not be empty".into()));
    }
    reject_control("host", host)?;
    if host.contains(['/', '\\', '@', ' ', '\t']) {
        return Err(FtpError::Configuration(
            "host must be an IP literal or DNS name without URL delimiters".into(),
        ));
    }
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let dns_name = host.strip_suffix('.').unwrap_or(host);
    if dns_name.is_empty()
        || dns_name.len() > 253
        || !dns_name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(FtpError::Configuration(
            "host must be an ASCII DNS name or IP literal".into(),
        ));
    }
    Ok(())
}

fn reject_control(name: &str, value: &str) -> Result<(), FtpError> {
    if value.chars().any(char::is_control) {
        return Err(FtpError::Configuration(format!(
            "{name} must not contain control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ftp_requires_exact_acknowledgement() {
        assert!(PlainFtpConfirmation::acknowledge("yes").is_err());
        assert!(PlainFtpConfirmation::acknowledge(PLAIN_FTP_ACKNOWLEDGEMENT).is_ok());
    }

    #[test]
    fn active_mode_rejects_unspecified_listener() {
        let mode = DataMode::Active {
            bind_address: "0.0.0.0".parse().unwrap(),
            listen_port: NonZeroU16::new(2020).unwrap(),
        };
        assert!(mode.active_binding().is_err());
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = Credentials::new("operator", "secret").unwrap();
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("operator"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn host_rejects_embedded_port_or_url_syntax() {
        let credentials = Credentials::new("operator", "secret").unwrap();
        let config =
            FtpConfig::explicit_ftps("example.test:21", NonZeroU16::new(21).unwrap(), credentials);
        assert!(config.is_err());
    }
}
